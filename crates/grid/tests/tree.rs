//! Headless UI tests of the tree on screen (`.agents/COMPONENT_TEST_RULES.md`):
//! turning a tree on and off against a grouping (the two exclude each other),
//! the tree column and its chevrons, the mouse and the keys that expand and
//! collapse tree rows, the focus that follows a collapse, the two selection
//! policies of tree rows (`FocusCaptions` by default: tree rows are selected as
//! data rows; `FocusBranches`: rows with children take the focus only, as group
//! captions do), the menu of a tree row, live data and the accessibility tree.
//!
//! The data is one fund (1), two desks (2, 3), three books under each (4–6,
//! 7–9) and three positions under each book (10–27), sorted by id: a book's
//! positions are `10 + 3 × (book − 4)` and the next two. The tree column is
//! Instrument, which holds the names; at `xsmall` ten lines fit.

mod common;

use std::hash::{Hash as _, Hasher as _};

use gpui_kit::accesskit::Action as A11yAction;
use gpui_kit::{
    App, Context, ElementId, Focusable as _, Modifiers, MouseButton, Pixels, Point, Role,
    TestAppContext, point, px, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    ExpandDepth, ExpandTarget, FocusBranches, GridMenuContext, GridTarget, GroupBy, GroupOrigin,
    LiveGridDelegate, LiveGridEvent, LiveGridState, ManualReorder, SelectionMode, SelectionOrigin,
    SortDirection, TreeFilter, TreeSpec, ViewSpec,
};

use crate::common::{
    GroupSeen, Grouped, GroupedColumn, GroupedFixture, GroupedRow, ROW, Side as TradeSide,
    open_grouped, open_grouped_with, press_at, unthrottled,
};

use GroupOrigin::{Programmatic, User};
use GroupedColumn::{Id, Instrument, Qty, Side, Trader};
use SortDirection::Ascending;

const BOOKS: [&str; 6] = ["Book 1", "Book 2", "Book 3", "Book 4", "Book 5", "Book 6"];
const POSITIONS: [&str; 4] = ["AAPL", "MSFT", "NVDA", "TSLA"];

/// The name of tree row `id`: Fund, Desk A, Desk B, Book 1–6, then an
/// instrument for a position.
fn name(id: u32) -> &'static str {
    match id {
        1 => "Fund",
        2 => "Desk A",
        3 => "Desk B",
        4..=9 => BOOKS[id as usize - 4],
        _ => POSITIONS[id as usize % POSITIONS.len()],
    }
}

/// Row `id` under `parent`: the name in Instrument, the trader by the id.
fn node(id: u32, parent: Option<u32>) -> GroupedRow {
    GroupedRow {
        id,
        instrument: name(id),
        side: TradeSide::Buy,
        qty: i64::from(id) * 10,
        price: 100.0 + f64::from(id),
        trader: ["Ann", "Bob", "Cid"][id as usize % 3],
        parent,
    }
}

/// The parent of a row of [`tree_rows`].
fn parent_of(id: u32) -> Option<u32> {
    match id {
        1 => None,
        2 | 3 => Some(1),
        4..=6 => Some(2),
        7..=9 => Some(3),
        _ => Some(4 + (id - 10) / 3),
    }
}

/// The fund, its two desks, six books and eighteen positions.
fn tree_rows() -> Vec<GroupedRow> {
    (1..=27).map(|id| node(id, parent_of(id))).collect()
}

/// The positions of book `book`.
fn positions(book: u32) -> Vec<u32> {
    let first = 10 + 3 * (book - 4);
    (first..first + 3).collect()
}

/// Sorted by id, a tree in the Instrument column.
fn tree_view() -> ViewSpec<GroupedRow> {
    ViewSpec::new()
        .with_sort([(Id, Ascending)])
        .with_tree(TreeSpec::new().with_column(Instrument))
}

/// An `xsmall` grid of [`tree_rows`] as a tree with the fund and the desks
/// expanded, the books collapsed: Fund, Desk A, Book 1–3, Desk B, Book 4–6
/// (nine lines), in `RowSet`; configured further by `configure`.
fn open_tree(
    cx: &mut TestAppContext,
    configure: impl FnOnce(LiveGridState<Grouped>) -> LiveGridState<Grouped> + 'static,
) -> GroupedFixture {
    open_grouped(cx, tree_rows(), |grid| {
        configure(
            grid.with_view(tree_view())
                .with_tuning(unthrottled())
                .with_expand_depth(ExpandDepth::Levels(2))
                .with_selection_mode(SelectionMode::RowSet),
        )
    })
}

fn row(key: u32) -> ExpandTarget<u32> {
    ExpandTarget::Row(key)
}

// Window positions over the Instrument column (x 50–130 at `xsmall`): 6 px
// of cell padding, 16 px per level, then the 20 px chevron lane.

/// The middle of the chevron of the tree row at `depth` on screen line
/// `line`.
fn chevron(line: usize, depth: u16) -> Point<Pixels> {
    point(
        px(50. + 6. + 16. * f32::from(depth) + 10.),
        px(ROW * (line as f32 + 1.5)),
    )
}

/// The tree cell's text on screen line `line`, right of the chevron lane
/// of any row down to depth 3.
fn tree_text(line: usize) -> Point<Pixels> {
    point(px(127.), px(ROW * (line as f32 + 1.5)))
}

fn read<T, D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    f: impl FnOnce(&LiveGridState<D>) -> T,
) -> T {
    fixture.grid.read_with(cx, |grid, _| f(grid))
}

fn update<T, D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    f: impl FnOnce(&mut LiveGridState<D>, &mut Context<LiveGridState<D>>) -> T,
) -> T {
    fixture.grid.update(cx, f)
}

fn expanded<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    key: u32,
) -> bool {
    read(fixture, cx, |grid| grid.is_expanded(&row(key)))
}

/// The focused row and column.
fn focus_of<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Option<(u32, GroupedColumn)> {
    read(fixture, cx, |grid| {
        grid.focused_cell()
            .and_then(|cell| Some((*cell.row()?, cell.column())))
    })
}

/// The selected keys, sorted.
fn selected<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Vec<u32> {
    read(fixture, cx, |grid| {
        let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
        keys.sort_unstable();
        keys
    })
}

/// Every row of the tree, shown or under a collapsed row, with its depth,
/// in the order of `Projection::rows`.
fn all_rows<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Vec<(u32, u16)> {
    read(fixture, cx, |grid| {
        let (projection, snapshot) = (grid.projection(), grid.snapshot());
        projection
            .expanded_lines()
            .filter_map(|line| Some((snapshot.row(line.row()?)?.id, line.depth())))
            .collect()
    })
}

/// Gives the grid the keyboard focus and types `key`, then settles.
fn type_key<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    key: &str,
) {
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
        window.press(key, cx);
    });
    fixture.settle(cx);
}

/// Dispatches an Action to the focused grid, then settles.
fn dispatch<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    action: &dyn gpui_kit::Action,
) {
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
        window.dispatch_action(action.boxed_clone(), cx);
    });
    fixture.settle(cx);
}

/// A press (and release) of `button` at a window position, `count`th of a
/// series.
fn press<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    position: Point<Pixels>,
    button: MouseButton,
    modifiers: Modifiers,
    count: usize,
) {
    fixture.input(cx, |window, cx| {
        press_at(window, position, button, modifiers, count, cx)
    });
}

fn left<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    position: Point<Pixels>,
) {
    press(
        fixture,
        cx,
        position,
        MouseButton::Left,
        Modifiers::default(),
        1,
    );
}

/// Every event so far, forgetting them.
fn take_events<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
) -> Vec<LiveGridEvent<u32, GroupedColumn>> {
    std::mem::take(&mut *fixture.events.borrow_mut())
}

/// The keys of the `Confirm` events among `events`.
fn confirms(events: &[LiveGridEvent<u32, GroupedColumn>]) -> Vec<Vec<u32>> {
    events
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::Confirm { keys, .. } => Some(keys.to_vec()),
            _ => None,
        })
        .collect()
}

fn clicks(events: &[LiveGridEvent<u32, GroupedColumn>]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, LiveGridEvent::Click { .. }))
        .count()
}

/// The expansion, focus, selection and projection events among `events`,
/// in order.
fn seen(events: &[LiveGridEvent<u32, GroupedColumn>]) -> Vec<GroupSeen> {
    events
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::ExpansionChange { target, origin } => {
                Some(GroupSeen::Expansion(target.clone(), *origin))
            }
            LiveGridEvent::FocusChange { origin, .. } => Some(GroupSeen::Focus(*origin)),
            LiveGridEvent::SelectionChange { origin, .. } => Some(GroupSeen::Selection(*origin)),
            LiveGridEvent::ProjectionChange { .. } => Some(GroupSeen::Projection),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn test_tree_state_builder(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, tree_rows(), |grid| {
        grid.with_view(
            ViewSpec::new().with_sort([(Id, Ascending)]).with_tree(
                TreeSpec::new()
                    .with_column(Instrument)
                    .with_filter(TreeFilter::HideSubtree),
            ),
        )
        .with_tuning(unthrottled())
        .with_expand_depth(ExpandDepth::Levels(1))
        .with_selection_mode(SelectionMode::RowSet)
        .with_selection_policy(FocusBranches)
    });
    let (column, filter) = read(&fixture, cx, |grid| {
        let tree = grid.tree().expect("a tree");
        (
            tree.column(),
            matches!(tree.filter(), TreeFilter::HideSubtree),
        )
    });
    assert_eq!(column, Some(Instrument));
    assert!(filter);
    assert_eq!(
        read(&fixture, cx, |grid| grid.expand_depth()),
        ExpandDepth::Levels(1)
    );
    assert!(read(&fixture, cx, |grid| grid.group_by().is_empty()));
    // Only the fund is expanded: the fund and its desks.
    assert_eq!(fixture.visible_keys(cx), [1, 2, 3]);
    assert_eq!(read(&fixture, cx, |grid| grid.stats().tree_cycles()), 0);
    // `FocusBranches`: a press on the fund focuses it, selects nothing.
    left(&fixture, cx, tree_text(0));
    assert_eq!(focus_of(&fixture, cx), Some((1, Instrument)));
    assert!(selected(&fixture, cx).is_empty());

    // The defaults: flat, everything expanded.
    let defaults = open_grouped(cx, tree_rows(), |grid| grid);
    assert!(read(&defaults, cx, |grid| grid.tree().is_none()));
    assert_eq!(
        read(&defaults, cx, |grid| grid.expand_depth()),
        ExpandDepth::All
    );
    assert!(!read(&defaults, cx, |grid| grid.projection().is_tree()));
}

#[gpui_kit::test]
fn test_set_tree_replaces_grouping(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, tree_rows(), |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Qty, Ascending)])
                .with_group_by([GroupBy::column(Trader)]),
        )
        .with_tuning(unthrottled())
        .with_reorder_policy(ManualReorder)
    });
    assert!(read(&fixture, cx, |grid| grid.projection().is_grouped()));
    // A new order held back, and the rows scrolled.
    fixture.writer(cx).upsert(GroupedRow {
        qty: -1,
        ..node(27, parent_of(27))
    });
    fixture.settle(cx);
    assert!(read(&fixture, cx, LiveGridState::is_order_held));
    update(&fixture, cx, |grid, cx| grid.scroll_to_row(8, cx));
    fixture.settle(cx);
    assert!(read(&fixture, cx, |grid| grid.visible_rows().start) > 0);
    fixture.take_seen();

    // A tree replaces the grouping: the held order goes first, then
    // one `GroupChange`; the new order shows from the top.
    update(&fixture, cx, |grid, cx| {
        grid.set_tree(Some(TreeSpec::new().with_column(Instrument)), cx)
    });
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Held(false), GroupSeen::Group(Programmatic)]
    );
    fixture.settle(cx);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);
    assert!(read(&fixture, cx, |grid| grid.group_by().is_empty()));
    assert!(read(&fixture, cx, |grid| grid.projection().is_tree()));
    assert_eq!(read(&fixture, cx, |grid| grid.visible_rows().start), 0);
    assert_eq!(fixture.visible_keys(cx)[..3], [1, 2, 4]);
    // A tree has no equality: the same tree again rebuilds and says so.
    update(&fixture, cx, |grid, cx| {
        grid.set_tree(Some(TreeSpec::new().with_column(Instrument)), cx)
    });
    assert_eq!(fixture.take_seen(), [GroupSeen::Group(Programmatic)]);
    fixture.settle(cx);
    fixture.take_seen();

    // Book 1 collapsed, then a grouping replaces the tree: one event.
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        row(4),
        false,
        cx
    )));
    fixture.settle(cx);
    fixture.take_seen();
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([GroupBy::column(Trader)], cx)
    });
    assert_eq!(fixture.take_seen(), [GroupSeen::Group(Programmatic)]);
    fixture.settle(cx);
    assert!(read(&fixture, cx, |grid| grid.tree().is_none()));
    assert!(read(&fixture, cx, |grid| grid.projection().is_grouped()));
    // No tree to turn off: nothing happens.
    fixture.take_seen();
    update(&fixture, cx, |grid, cx| grid.set_tree(None, cx));
    assert!(fixture.take_seen().is_empty());
    assert!(read(&fixture, cx, |grid| grid.projection().is_grouped()));

    // The tree again: book 1 is still collapsed, its toggle kept by key.
    update(&fixture, cx, |grid, cx| {
        grid.set_tree(Some(TreeSpec::new().with_column(Instrument)), cx)
    });
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, 4));
    assert_eq!(fixture.visible_keys(cx)[..4], [1, 2, 4, 5]);
    // Off: a flat view, one event.
    fixture.take_seen();
    update(&fixture, cx, |grid, cx| grid.set_tree(None, cx));
    assert_eq!(fixture.take_seen(), [GroupSeen::Group(Programmatic)]);
    fixture.settle(cx);
    assert!(read(&fixture, cx, |grid| {
        grid.tree().is_none() && !grid.projection().is_tree() && !grid.projection().is_grouped()
    }));
}

#[gpui_kit::test]
fn test_tree_chevron_press(cx: &mut TestAppContext) {
    let fixture = open_tree(cx, |grid| grid);
    // Lines: Fund, Desk A, Book 1, Book 2, Book 3, Desk B, Book 4–6.
    assert_eq!(fixture.visible_keys(cx), [1, 2, 4, 5, 6, 3, 7, 8, 9]);
    // Book 2 selected and focused by a press on its text.
    left(&fixture, cx, tree_text(3));
    assert_eq!(selected(&fixture, cx), [5]);
    assert_eq!(focus_of(&fixture, cx), Some((5, Instrument)));
    take_events(&fixture);

    // Book 1's chevron expands it: the selection, the focus stay, and no
    // click (a chevron is a control, as Finder's disclosure triangle).
    left(&fixture, cx, chevron(2, 2));
    assert!(expanded(&fixture, cx, 4));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), [1, 2, 4, 10, 11, 12, 5, 6, 3, 7]);
    assert_eq!(selected(&fixture, cx), [5]);
    assert_eq!(focus_of(&fixture, cx), Some((5, Instrument)));
    let events = take_events(&fixture);
    assert_eq!(clicks(&events), 0);
    assert_eq!(
        seen(&events),
        [
            GroupSeen::Expansion(Some(row(4)), User),
            GroupSeen::Projection
        ]
    );
    // Again: collapsed.
    left(&fixture, cx, chevron(2, 2));
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, 4));
    assert_eq!(fixture.visible_keys(cx), [1, 2, 4, 5, 6, 3, 7, 8, 9]);

    // Alt (Option) on Desk A: it and every row below it, one event. The
    // collapse hides the focused book: the focus goes to the desk.
    take_events(&fixture);
    press(
        &fixture,
        cx,
        chevron(1, 1),
        MouseButton::Left,
        Modifiers::alt(),
        1,
    );
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, 2));
    assert_eq!(fixture.visible_keys(cx), [1, 2, 3, 7, 8, 9]);
    assert_eq!(focus_of(&fixture, cx), Some((2, Instrument)));
    assert_eq!(
        seen(&take_events(&fixture)),
        [
            GroupSeen::Expansion(Some(row(2)), User),
            GroupSeen::Focus(SelectionOrigin::User),
            GroupSeen::Projection
        ]
    );
    // Alt again: the desk and its books, positions shown.
    press(
        &fixture,
        cx,
        chevron(1, 1),
        MouseButton::Left,
        Modifiers::alt(),
        1,
    );
    fixture.settle(cx);
    for key in [2, 4, 5, 6] {
        assert!(expanded(&fixture, cx, key), "{key}");
    }
    assert_eq!(
        fixture.visible_keys(cx),
        [1, 2, 4, 10, 11, 12, 5, 13, 14, 15]
    );

    // A leaf's lane is empty: a press there is a press on the cell.
    take_events(&fixture);
    left(&fixture, cx, point(px(50. + 6. + 48. + 10.), px(ROW * 4.5)));
    assert_eq!(selected(&fixture, cx), [10]);
    assert_eq!(focus_of(&fixture, cx), Some((10, Instrument)));
    assert_eq!(clicks(&take_events(&fixture)), 1);
    // A press on a branch's text selects it, as on any row.
    left(&fixture, cx, tree_text(2));
    assert_eq!(selected(&fixture, cx), [4]);
    assert!(expanded(&fixture, cx, 4));
    assert_eq!(clicks(&take_events(&fixture)), 1);
}

#[gpui_kit::test]
fn test_tree_double_click_confirms(cx: &mut TestAppContext) {
    let fixture = open_tree(cx, |grid| grid);
    take_events(&fixture);
    // A double click on Book 1's text: `Confirm` with the book; the book
    // stays collapsed.
    for count in [1, 2] {
        press(
            &fixture,
            cx,
            tree_text(2),
            MouseButton::Left,
            Modifiers::default(),
            count,
        );
    }
    fixture.settle(cx);
    assert_eq!(confirms(&take_events(&fixture)), [vec![4]]);
    assert!(!expanded(&fixture, cx, 4));
    // A double click on the chevron toggles once and confirms nothing.
    for count in [1, 2] {
        press(
            &fixture,
            cx,
            chevron(2, 2),
            MouseButton::Left,
            Modifiers::default(),
            count,
        );
    }
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, 4));
    assert!(confirms(&take_events(&fixture)).is_empty());
}

#[gpui_kit::test]
fn test_tree_keys(cx: &mut TestAppContext) {
    let fixture = open_tree(cx, |grid| grid);
    assert!(update(&fixture, cx, |grid, cx| {
        grid.set_focused_cell(&5, Some(Instrument), cx)
    }));
    fixture.settle(cx);

    // `+` expands a collapsed book; on an expanded one it goes on.
    type_key(&fixture, cx, "+");
    assert!(expanded(&fixture, cx, 5));
    assert_eq!(fixture.visible_keys(cx)[3..7], [5, 13, 14, 15]);
    take_events(&fixture);
    type_key(&fixture, cx, "+");
    assert!(seen(&take_events(&fixture)).is_empty());
    // `-` collapses an expanded book.
    type_key(&fixture, cx, "-");
    assert!(!expanded(&fixture, cx, 5));
    // `-` on a collapsed book collapses its desk, and the focus goes to the
    // desk in the same column (WAI-ARIA treegrid, "move to parent").
    take_events(&fixture);
    type_key(&fixture, cx, "-");
    assert!(!expanded(&fixture, cx, 2));
    assert_eq!(focus_of(&fixture, cx), Some((2, Instrument)));
    assert_eq!(
        seen(&take_events(&fixture)),
        [
            GroupSeen::Expansion(Some(row(2)), User),
            GroupSeen::Focus(SelectionOrigin::User),
            GroupSeen::Projection
        ]
    );
    assert_eq!(fixture.visible_keys(cx), [1, 2, 3, 7, 8, 9]);

    // `-` on a leaf collapses its book and focuses it.
    type_key(&fixture, cx, "+");
    type_key(&fixture, cx, "down");
    type_key(&fixture, cx, "+");
    type_key(&fixture, cx, "down");
    assert_eq!(focus_of(&fixture, cx), Some((10, Instrument)));
    type_key(&fixture, cx, "+");
    assert_eq!(focus_of(&fixture, cx), Some((10, Instrument)), "a leaf");
    type_key(&fixture, cx, "-");
    assert!(!expanded(&fixture, cx, 4));
    assert_eq!(focus_of(&fixture, cx), Some((4, Instrument)));

    // ← and → move between cells, they expand nothing.
    type_key(&fixture, cx, "right");
    assert_eq!(focus_of(&fixture, cx), Some((4, Side)));
    assert!(!expanded(&fixture, cx, 4));
    type_key(&fixture, cx, "left");
    type_key(&fixture, cx, "left");
    assert_eq!(focus_of(&fixture, cx), Some((4, Id)));
    assert!(expanded(&fixture, cx, 2));

    // ↑ and ↓ walk the shown lines: the rows of collapsed books are skipped.
    type_key(&fixture, cx, "down");
    assert_eq!(focus_of(&fixture, cx), Some((5, Id)));

    // On the root: `-` collapses it, then goes on.
    update(&fixture, cx, |grid, cx| grid.set_focused_cell(&1, None, cx));
    fixture.settle(cx);
    type_key(&fixture, cx, "-");
    assert!(!expanded(&fixture, cx, 1));
    assert_eq!(fixture.visible_keys(cx), [1]);
    take_events(&fixture);
    type_key(&fixture, cx, "-");
    assert!(seen(&take_events(&fixture)).is_empty());
    assert_eq!(focus_of(&fixture, cx), Some((1, Id)));
}

#[gpui_kit::test]
fn test_tree_focus_follows_collapse(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, tree_rows(), |grid| {
        grid.with_view(tree_view()).with_tuning(unthrottled())
    });
    // Everything expanded: position 11 is on line 4.
    assert!(update(&fixture, cx, |grid, cx| grid
        .set_focused_cell(&11, None, cx)));
    fixture.settle(cx);
    take_events(&fixture);
    // Collapse all: the focus goes to the outermost collapsed row shown,
    // the fund, at once, after the expansion's event.
    dispatch(&fixture, cx, &gpui_kit_grid::actions::CollapseAll);
    assert_eq!(fixture.visible_keys(cx), [1]);
    assert_eq!(focus_of(&fixture, cx).map(|(key, _)| key), Some(1));
    assert_eq!(
        seen(&take_events(&fixture))[..2],
        [
            GroupSeen::Expansion(None, User),
            GroupSeen::Focus(SelectionOrigin::User)
        ]
    );

    // Expand all, the focus on the position again; collapsing its book
    // from the API moves the focus to the book, `Programmatic`.
    dispatch(&fixture, cx, &gpui_kit_grid::actions::ExpandAll);
    assert!(update(&fixture, cx, |grid, cx| grid
        .set_focused_cell(&11, None, cx)));
    fixture.settle(cx);
    take_events(&fixture);
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        row(4),
        false,
        cx
    )));
    assert_eq!(focus_of(&fixture, cx).map(|(key, _)| key), Some(4));
    assert_eq!(
        seen(&take_events(&fixture)),
        [
            GroupSeen::Expansion(Some(row(4)), Programmatic),
            GroupSeen::Focus(SelectionOrigin::Programmatic)
        ]
    );
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx)[..4], [1, 2, 4, 5]);
}

#[gpui_kit::test]
fn test_tree_column_fallback(cx: &mut TestAppContext) {
    let fixture = open_tree(cx, |grid| grid);
    // The tree's column hidden: the tree shows in the first visible one,
    // Id; its chevrons line up from the body's left edge.
    assert!(update(&fixture, cx, |grid, cx| {
        grid.set_column_visible(Instrument, false, cx)
    }));
    fixture.settle(cx);
    left(&fixture, cx, common::chevron_point(0, 0));
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, 1));
    assert_eq!(fixture.visible_keys(cx), [1]);

    // Shown again and moved to the third place (x 110–190): the tree goes
    // with it.
    update(&fixture, cx, |grid, cx| {
        assert!(grid.set_column_visible(Instrument, true, cx));
        assert!(grid.move_column(Instrument, Some(Qty), cx));
    });
    fixture.settle(cx);
    assert_eq!(
        read(&fixture, cx, |grid| grid
            .visible_columns()
            .collect::<Vec<_>>()),
        [Id, Side, Instrument, Qty, GroupedColumn::Price, Trader]
    );
    // The first column has no chevron now: a press there selects the fund.
    left(&fixture, cx, common::chevron_point(0, 0));
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, 1));
    assert_eq!(selected(&fixture, cx), [1]);
    // The chevron is in the moved column.
    left(&fixture, cx, point(px(110. + 6. + 10.), px(ROW * 1.5)));
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, 1));
    assert_eq!(fixture.visible_keys(cx), [1, 2, 4, 5, 6, 3, 7, 8, 9]);
    // The tree's column is a column like any other for the rest of it:
    // its cell takes the focus.
    left(&fixture, cx, point(px(185.), px(ROW * 2.5)));
    assert_eq!(focus_of(&fixture, cx), Some((2, Instrument)));
}

/// What the delegate's menu hook saw.
#[derive(Clone, Debug)]
struct MenuSeen {
    target: GridTarget<u32, GroupedColumn>,
    scope: Vec<u32>,
}

/// A delegate with a primary command that records every menu.
#[derive(Clone, Default)]
struct Recording(std::rc::Rc<std::cell::RefCell<Vec<MenuSeen>>>);

impl Recording {
    fn last_menu(&self) -> MenuSeen {
        self.0.borrow().last().cloned().expect("a menu was built")
    }
}

impl LiveGridDelegate for Recording {
    type Row = GroupedRow;

    fn primary_command(
        &self,
        menu: &GridMenuContext<'_, GroupedRow>,
        _: &App,
    ) -> Option<gpui_kit::SharedString> {
        Some(format!("Open {}", menu.selection().len()).into())
    }

    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, GroupedRow>,
        menu: gpui_kit::component::menu::PopupMenu,
        window: &mut gpui_kit::Window,
        cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
    ) -> gpui_kit::component::menu::PopupMenu {
        self.0.borrow_mut().push(MenuSeen {
            target: menu_context.target().clone(),
            scope: menu_context.selection().keys().to_vec(),
        });
        menu_context.standard_items(menu, window, cx)
    }
}

/// [`open_tree`] with the recording delegate.
fn open_recording(
    cx: &mut TestAppContext,
    configure: impl FnOnce(LiveGridState<Recording>) -> LiveGridState<Recording> + 'static,
) -> (GroupedFixture<Recording>, Recording) {
    let recording = Recording::default();
    let fixture = open_grouped_with(cx, tree_rows(), recording.clone(), |grid| {
        configure(
            grid.with_view(tree_view())
                .with_tuning(unthrottled())
                .with_expand_depth(ExpandDepth::Levels(2))
                .with_selection_mode(SelectionMode::RowSet),
        )
    });
    (fixture, recording)
}

/// The labels of the open menu's items, top to bottom.
fn menu_labels<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Vec<String> {
    let mut labels = Vec::new();
    fixture.input(cx, |window, _| {
        let mut items: Vec<_> = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|item| item.role() == Some(Role::MenuItem) && item.visible())
            .collect();
        items.sort_by(|a, b| {
            a.bounds()
                .origin
                .y
                .partial_cmp(&b.bounds().origin.y)
                .expect("finite")
        });
        labels = items
            .iter()
            .filter_map(|item| item.label().map(str::to_owned))
            .collect();
    });
    labels
}

fn right<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    position: Point<Pixels>,
) {
    press(
        fixture,
        cx,
        position,
        MouseButton::Right,
        Modifiers::default(),
        1,
    );
    fixture.settle(cx);
}

fn close_menu<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) {
    fixture.input(cx, |window, cx| window.press("escape", cx));
    fixture.settle(cx);
}

#[gpui_kit::test]
fn test_tree_selection_policies(cx: &mut TestAppContext) {
    // `FocusCaptions`, the default: tree rows are rows.
    let (fixture, menus) = open_recording(cx, |grid| grid);
    left(&fixture, cx, tree_text(0));
    assert_eq!(selected(&fixture, cx), [1]);
    // Cmd/Ctrl+A selects the rows shown: no position of a collapsed book.
    type_key(&fixture, cx, "secondary-a");
    assert_eq!(selected(&fixture, cx), (1..=9).collect::<Vec<_>>());
    // A position of a collapsed book selected explicitly stays in the
    // scope, and a command over a book acts on the selection.
    update(&fixture, cx, |grid, cx| {
        grid.set_selected_keys([10, 5], cx);
        grid.set_focused_cell(&5, None, cx)
    });
    fixture.settle(cx);
    assert_eq!(
        read(&fixture, cx, |grid| grid.scoped_selection().keys().to_vec()),
        [10, 5]
    );
    take_events(&fixture);
    type_key(&fixture, cx, "enter");
    assert_eq!(confirms(&take_events(&fixture)), [vec![10, 5]]);
    right(&fixture, cx, tree_text(3));
    assert_eq!(menus.last_menu().scope, [10, 5]);
    close_menu(&fixture, cx);

    // `FocusBranches`, the rule of group captions: rows with children take
    // the focus only; a command over one acts on its subtree's leaves.
    let (fixture, menus) = open_recording(cx, |grid| grid.with_selection_policy(FocusBranches));
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        row(4),
        true,
        cx
    )));
    fixture.settle(cx);
    // Lines: Fund, Desk A, Book 1, 10, 11, 12, Book 2, Book 3, Desk B, Book 4.
    left(&fixture, cx, tree_text(0));
    assert_eq!(focus_of(&fixture, cx), Some((1, Instrument)));
    assert!(selected(&fixture, cx).is_empty());
    // Space on a branch goes on.
    type_key(&fixture, cx, "space");
    assert!(selected(&fixture, cx).is_empty());
    // Cmd/Ctrl+A selects the leaves shown.
    type_key(&fixture, cx, "secondary-a");
    assert_eq!(selected(&fixture, cx), [10, 11, 12]);
    // A range from 10 over Book 2 leaves the book out; the focus on it.
    left(&fixture, cx, tree_text(3));
    press(
        &fixture,
        cx,
        tree_text(6),
        MouseButton::Left,
        Modifiers::shift(),
        1,
    );
    assert_eq!(selected(&fixture, cx), [10, 11, 12]);
    assert_eq!(focus_of(&fixture, cx), Some((5, Instrument)));
    // Enter on a collapsed book: its positions.
    take_events(&fixture);
    type_key(&fixture, cx, "enter");
    assert_eq!(confirms(&take_events(&fixture)), [positions(5)]);
    // Enter on the fund: every position, in the order of the rows.
    update(&fixture, cx, |grid, cx| grid.set_focused_cell(&1, None, cx));
    type_key(&fixture, cx, "enter");
    assert_eq!(
        confirms(&take_events(&fixture)),
        [(10..=27).collect::<Vec<_>>()]
    );
    // A double click on Desk B: its positions; it stays expanded.
    for count in [1, 2] {
        press(
            &fixture,
            cx,
            tree_text(8),
            MouseButton::Left,
            Modifiers::default(),
            count,
        );
    }
    fixture.settle(cx);
    assert_eq!(
        confirms(&take_events(&fixture)),
        [(19..=27).collect::<Vec<_>>()]
    );
    assert!(expanded(&fixture, cx, 3));
    // The menu of Book 4: the target is its cell, the scope its positions;
    // the selection stays.
    right(&fixture, cx, tree_text(9));
    let menu = menus.last_menu();
    assert_eq!(
        menu.target,
        GridTarget::Cell {
            key: 7,
            column: Instrument
        }
    );
    assert_eq!(menu.scope, positions(7));
    assert_eq!(focus_of(&fixture, cx), Some((7, Instrument)));
    assert_eq!(selected(&fixture, cx), [10, 11, 12]);
    close_menu(&fixture, cx);
    assert_eq!(selected(&fixture, cx), [10, 11, 12]);
    // On a leaf, the selection.
    right(&fixture, cx, tree_text(4));
    assert_eq!(menus.last_menu().scope, [10, 11, 12]);
}

#[gpui_kit::test]
fn test_tree_live_updates(cx: &mut TestAppContext) {
    let fixture = open_tree(cx, |grid| grid);
    let writer = fixture.writer(cx);
    let depth_of = |fixture: &GroupedFixture, cx: &mut TestAppContext, key: u32| {
        all_rows(fixture, cx)
            .into_iter()
            .find(|(each, _)| *each == key)
            .map(|(_, depth)| depth)
    };
    let after = |fixture: &GroupedFixture, cx: &mut TestAppContext, key: u32| {
        let rows = all_rows(fixture, cx);
        let ix = rows.iter().position(|(each, _)| *each == key)?;
        rows.get(ix + 1).map(|(next, _)| *next)
    };

    // A child before its parent: a root until the parent comes, then under
    // it; the new book is collapsed, by the default depth.
    writer.upsert(node(40, Some(30)));
    fixture.settle(cx);
    assert_eq!(depth_of(&fixture, cx, 40), Some(0));
    writer.upsert(node(30, Some(2)));
    fixture.settle(cx);
    assert_eq!(depth_of(&fixture, cx, 30), Some(2));
    assert_eq!(depth_of(&fixture, cx, 40), Some(3));
    assert_eq!(after(&fixture, cx, 30), Some(40));
    assert!(!expanded(&fixture, cx, 30));
    assert!(!fixture.visible_keys(cx).contains(&40));

    // A position into a collapsed book: the book stays collapsed.
    writer.upsert(node(41, Some(5)));
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, 5));
    assert_eq!(after(&fixture, cx, 15), Some(41));
    assert!(!fixture.visible_keys(cx).contains(&41));

    // The first child of a leaf makes it a branch.
    update(&fixture, cx, |grid, cx| grid.set_expanded(row(4), true, cx));
    fixture.settle(cx);
    let branch = |fixture: &GroupedFixture, cx: &mut TestAppContext, key: u32| {
        read(fixture, cx, |grid| {
            grid.snapshot()
                .row_id(&key)
                .and_then(|id| grid.projection().branch(id))
                .is_some()
        })
    };
    assert!(!branch(&fixture, cx, 11));
    writer.upsert(node(42, Some(11)));
    fixture.settle(cx);
    assert!(branch(&fixture, cx, 11));
    // Its last child gone: a leaf again.
    writer.remove(42);
    fixture.settle(cx);
    assert!(!branch(&fixture, cx, 11));

    // A position moves to another book, its columns as they were: on the
    // next rebuild it is under the new book. The focus on it follows it,
    // to the new book while that is collapsed.
    assert!(update(&fixture, cx, |grid, cx| grid
        .set_focused_cell(&10, None, cx)));
    fixture.settle(cx);
    writer.upsert(node(10, Some(7)));
    fixture.settle(cx);
    assert_eq!(after(&fixture, cx, 7), Some(10));
    assert_eq!(depth_of(&fixture, cx, 10), Some(3));
    assert_eq!(focus_of(&fixture, cx).map(|(key, _)| key), Some(7));

    // A book removed: its positions are roots.
    writer.remove(6);
    fixture.settle(cx);
    for key in positions(6) {
        assert_eq!(depth_of(&fixture, cx, key), Some(0), "{key}");
    }

    // A cycle: both rows are roots, and the stats count them.
    writer.upsert(node(50, Some(51)));
    writer.upsert(node(51, Some(50)));
    fixture.settle(cx);
    assert_eq!(depth_of(&fixture, cx, 50), Some(0));
    assert_eq!(depth_of(&fixture, cx, 51), Some(0));
    assert_eq!(read(&fixture, cx, |grid| grid.stats().tree_cycles()), 2);
    // Grouped, no tree: no cycles.
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([GroupBy::column(Trader)], cx)
    });
    fixture.settle(cx);
    assert_eq!(read(&fixture, cx, |grid| grid.stats().tree_cycles()), 0);
}

#[gpui_kit::test]
fn test_tree_menu_items(cx: &mut TestAppContext) {
    let (fixture, menus) = open_recording(cx, |grid| grid);
    // A collapsed book: Expand, Expand all, Collapse all; no Captions only.
    right(&fixture, cx, tree_text(2));
    assert_eq!(
        menus.last_menu().target,
        GridTarget::Cell {
            key: 4,
            column: Instrument
        }
    );
    let labels = menu_labels(&fixture, cx);
    let expand_items = labels
        .iter()
        .position(|label| label == "Expand")
        .expect("Expand");
    assert_eq!(
        labels[expand_items..expand_items + 3],
        ["Expand", "Expand all", "Collapse all"]
    );
    assert!(labels.iter().any(|label| label == "Copy"));
    assert!(!labels.iter().any(|label| label == "Collapse"));
    assert!(!labels.iter().any(|label| label == "Captions only"));
    // Expand: its Action goes to the grid's focus, the book.
    let mut item = None;
    fixture.input(cx, |window, _| {
        item = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|item| item.role() == Some(Role::MenuItem) && item.label() == Some("Expand"))
            .map(|item| item.bounds().center());
    });
    left(&fixture, cx, item.expect("the Expand item"));
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, 4));

    // An expanded desk: Collapse.
    right(&fixture, cx, tree_text(1));
    let labels = menu_labels(&fixture, cx);
    assert!(labels.iter().any(|label| label == "Collapse"));
    assert!(!labels.iter().any(|label| label == "Expand"));
    close_menu(&fixture, cx);

    // A position, a leaf: no expansion items.
    right(&fixture, cx, tree_text(3));
    assert_eq!(
        menus.last_menu().target,
        GridTarget::Cell {
            key: 10,
            column: Instrument
        }
    );
    let labels = menu_labels(&fixture, cx);
    for label in [
        "Expand",
        "Collapse",
        "Expand all",
        "Collapse all",
        "Captions only",
    ] {
        assert!(!labels.iter().any(|each| each == label), "{label}");
    }
    assert!(labels.iter().any(|label| label == "Copy"));
}

/// The accessibility row of the data row `key`: `NamedInteger(
/// "live-grid-row", key hash)`, hashed with `FxHasher`.
fn data_row_id(key: u32) -> ElementId {
    let mut hasher = rustc_hash::FxHasher::default();
    key.hash(&mut hasher);
    ElementId::NamedInteger("live-grid-row".into(), hasher.finish())
}

/// The data rows of the last frame's accessibility tree, top to bottom:
/// their ids, `expanded` and `selected`.
fn a11y_rows<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Vec<(ElementId, Option<bool>, Option<bool>)> {
    let mut rows = Vec::new();
    fixture.input(cx, |window, _| {
        let mut found: Vec<_> = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|element| element.role() == Some(Role::Row))
            .filter(|element| {
                matches!(
                    element.path().last(),
                    Some(ElementId::NamedInteger(name, _)) if name == "live-grid-row"
                )
            })
            .collect();
        found.sort_by(|a, b| {
            a.bounds()
                .origin
                .y
                .partial_cmp(&b.bounds().origin.y)
                .expect("finite")
        });
        rows = found
            .iter()
            .map(|element| {
                (
                    element.path().last().cloned().expect("an id"),
                    element.expanded(),
                    element.selected(),
                )
            })
            .collect();
    });
    rows
}

fn root_role<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Option<Role> {
    let mut role = None;
    fixture.input(cx, |window, _| {
        role = window.try_find("live-grid").and_then(|root| root.role())
    });
    role
}

#[gpui_kit::test]
fn test_tree_accessibility(cx: &mut TestAppContext) {
    let fixture = open_tree(cx, |grid| grid.with_selection_policy(FocusBranches));
    update(&fixture, cx, |grid, cx| {
        grid.set_a11y_forced(true);
        grid.set_expanded(row(4), true, cx)
    });
    fixture.settle(cx);
    // A tree grid: rows with children say whether they are expanded, the
    // leaves say nothing.
    assert_eq!(root_role(&fixture, cx), Some(Role::TreeGrid));
    let expanded_states: Vec<(ElementId, Option<bool>)> = a11y_rows(&fixture, cx)
        .into_iter()
        .map(|(id, expanded, _)| (id, expanded))
        .collect();
    assert_eq!(
        expanded_states,
        [
            (data_row_id(1), Some(true)),
            (data_row_id(2), Some(true)),
            (data_row_id(4), Some(true)),
            (data_row_id(10), None),
            (data_row_id(11), None),
            (data_row_id(12), None),
            (data_row_id(5), Some(false)),
            (data_row_id(6), Some(false)),
            (data_row_id(3), Some(true)),
            (data_row_id(7), Some(false)),
        ]
    );
    // Selected: only rows the policy selects and the selection holds.
    type_key(&fixture, cx, "secondary-a");
    let selected_rows: Vec<ElementId> = a11y_rows(&fixture, cx)
        .into_iter()
        .filter(|(_, _, selected)| *selected == Some(true))
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(
        selected_rows,
        [data_row_id(10), data_row_id(11), data_row_id(12)]
    );

    // A screen reader's Expand on a book's row expands it, as the chevron.
    take_events(&fixture);
    update(&fixture, cx, |grid, cx| {
        grid.send_a11y_action(&data_row_id(5), A11yAction::Expand, cx)
    });
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, 5));
    assert_eq!(
        seen(&take_events(&fixture))[0],
        GroupSeen::Expansion(Some(row(5)), User)
    );
    assert!(
        a11y_rows(&fixture, cx)
            .iter()
            .any(|(id, expanded, _)| *id == data_row_id(5) && *expanded == Some(true))
    );

    // Flat again: a grid.
    update(&fixture, cx, |grid, cx| grid.set_tree(None, cx));
    fixture.settle(cx);
    assert_eq!(root_role(&fixture, cx), Some(Role::Grid));
}
