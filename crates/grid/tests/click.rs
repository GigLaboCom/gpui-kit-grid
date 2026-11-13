//! Headless UI tests of `LiveGridEvent::Click`
//! (`.agents/COMPONENT_TEST_RULES.md`): clicks on a data cell, a pinned cell, a
//! header and the empty area, by key and column; double clicks; drags, context
//! presses and the press that closes the menu, which are no clicks; the order
//! of a click's events; clicks on the totals row and a group footer. Assertions
//! read the events, not pixels.

mod common;

use std::{sync::Arc, time::Duration};

use gpui_kit::component::Size;
use gpui_kit::{
    App, AppContext as _, Modifiers, MouseButton, Pixels, Point, TestAppContext, Window, point, px,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    GridTarget, LiveGridEvent, PinnedEdge, SelectionMode, SortDirection, ViewSpec,
};

use crate::common::{
    Fixture, Item, ItemColumn, ROW, drag_move_to, mouse_down_at, mouse_up_at, open,
    open_configured, press_at, unthrottled,
};

use ItemColumn::{Id, Price};
use SortDirection::{Ascending, Descending};

/// The events a click test follows, in order.
#[derive(Debug, PartialEq)]
enum Seen {
    /// The selected keys, sorted.
    Selection(Vec<u32>),
    /// The focused row.
    Focus(Option<u32>),
    Sort(Vec<(ItemColumn, SortDirection)>),
    Confirm(Vec<u32>),
    Click(GridTarget<u32, ItemColumn>, MouseButton, Modifiers, usize),
}

/// A left click with no modifiers.
fn click(target: GridTarget<u32, ItemColumn>, click_count: usize) -> Seen {
    Seen::Click(target, MouseButton::Left, Modifiers::default(), click_count)
}

fn cell_target(key: u32, column: ItemColumn) -> GridTarget<u32, ItemColumn> {
    GridTarget::Cell { key, column }
}

/// Every followed event so far, then forgets them.
fn take_events(fixture: &Fixture) -> Vec<Seen> {
    std::mem::take(&mut *fixture.events.borrow_mut())
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::SelectionChange { selection, .. } => {
                let mut keys: Vec<u32> = selection.iter().copied().collect();
                keys.sort_unstable();
                Some(Seen::Selection(keys))
            }
            LiveGridEvent::FocusChange { cell, .. } => {
                Some(Seen::Focus(cell.and_then(|cell| cell.row().copied())))
            }
            LiveGridEvent::SortChange { sort, .. } => Some(Seen::Sort(sort.to_vec())),
            LiveGridEvent::Confirm { keys, .. } => Some(Seen::Confirm(keys.to_vec())),
            LiveGridEvent::Click {
                target,
                button,
                modifiers,
                click_count,
            } => Some(Seen::Click(target, button, modifiers, click_count)),
            _ => None,
        })
        .collect()
}

/// `count` rows in insertion order (`price = id * 10`), events forgotten.
fn open_rows(cx: &mut TestAppContext, count: u32) -> Fixture {
    let fixture = open(cx, count, ViewSpec::new(), Duration::ZERO);
    take_events(&fixture);
    fixture
}

/// The center of a body cell on screen line `line`: Id is x 0..100, Price
/// 100..200 (the default width).
fn cell(line: usize, column: ItemColumn) -> Point<Pixels> {
    point(column_x(column), px(ROW * (line as f32 + 1.5)))
}

/// The center of a header cell.
fn header(column: ItemColumn) -> Point<Pixels> {
    point(column_x(column), px(ROW / 2.))
}

fn column_x(column: ItemColumn) -> Pixels {
    px(match column {
        Id => 50.,
        Price => 150.,
    })
}

impl Fixture {
    fn input<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("window is open")
    }

    /// Presses and releases `button` at `position` with `modifiers`, as the
    /// `click_count`-th press of a series, and settles.
    fn press(
        &self,
        cx: &mut TestAppContext,
        position: Point<Pixels>,
        button: MouseButton,
        modifiers: Modifiers,
        click_count: usize,
    ) {
        self.input(cx, |window, cx| {
            press_at(window, position, button, modifiers, click_count, cx)
        });
        self.settle(cx);
    }

    fn click(&self, cx: &mut TestAppContext, position: Point<Pixels>) {
        self.press(cx, position, MouseButton::Left, Modifiers::default(), 1);
    }

    /// The second press of a double click (after a [`Self::click`]).
    fn double_click(&self, cx: &mut TestAppContext, position: Point<Pixels>) {
        self.press(cx, position, MouseButton::Left, Modifiers::default(), 2);
    }

    /// A press of another button, with no modifiers.
    fn press_button(&self, cx: &mut TestAppContext, position: Point<Pixels>, button: MouseButton) {
        self.press(cx, position, button, Modifiers::default(), 1);
    }

    fn is_menu_open(&self, cx: &mut TestAppContext) -> bool {
        self.grid.read_with(cx, |grid, _| grid.is_menu_open())
    }
}

#[gpui_kit::test]
fn test_click_on_cell_follows_selection_events(cx: &mut TestAppContext) {
    let fixture = open_rows(cx, 5);

    // One `SelectionChange` and one `FocusChange` at the release, then the
    // click.
    fixture.click(cx, cell(1, Price));
    assert_eq!(
        take_events(&fixture),
        [
            Seen::Selection(vec![1]),
            Seen::Focus(Some(1)),
            click(cell_target(1, Price), 1),
        ]
    );

    // A press on the selected row changes nothing, and still clicks. Right
    // of the columns, a row's press is on its last column.
    fixture.click(cx, cell(1, Price));
    fixture.click(cx, point(px(300.), cell(1, Id).y));
    assert_eq!(
        take_events(&fixture),
        [
            click(cell_target(1, Price), 1),
            click(cell_target(1, Price), 1)
        ]
    );

    // The modifiers held at the press, and the other buttons: a middle
    // click selects nothing.
    fixture.press(cx, cell(3, Id), MouseButton::Left, Modifiers::shift(), 1);
    fixture.press_button(cx, cell(4, Price), MouseButton::Middle);
    let events = take_events(&fixture);
    assert_eq!(
        events.last(),
        Some(&Seen::Click(
            cell_target(4, Price),
            MouseButton::Middle,
            Modifiers::default(),
            1
        ))
    );
    assert!(events.contains(&Seen::Click(
        cell_target(3, Id),
        MouseButton::Left,
        Modifiers::shift(),
        1
    )));
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid
            .focused_cell()
            .and_then(|cell| cell.row().copied())),
        Some(3),
        "the middle button moved no focus"
    );
}

#[gpui_kit::test]
fn test_click_target_is_the_key_at_the_press(cx: &mut TestAppContext) {
    let fixture = open_rows(cx, 5);

    // Key 1 is pressed on line 1; before the release a re-sort puts key 3
    // there. The click names key 1.
    fixture.input(cx, |window, cx| {
        mouse_down_at(
            window,
            cell(1, Price),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_sort([(Price, Descending)], cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx)[1], 3);
    fixture.input(cx, |window, cx| {
        mouse_up_at(
            window,
            cell(1, Price),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture.settle(cx);
    assert_eq!(
        take_events(&fixture),
        [
            Seen::Sort(vec![(Price, Descending)]),
            Seen::Selection(vec![1]),
            Seen::Focus(Some(1)),
            click(cell_target(1, Price), 1),
        ]
    );

    // The pressed row leaves the store before the release: no click.
    fixture.input(cx, |window, cx| {
        mouse_down_at(
            window,
            cell(0, Id),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture.writer(cx).remove(4);
    fixture.settle(cx);
    fixture.input(cx, |window, cx| {
        mouse_up_at(
            window,
            cell(0, Id),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture.settle(cx);
    assert!(
        !take_events(&fixture)
            .iter()
            .any(|seen| matches!(seen, Seen::Click(..))),
        "a removed row gives no click"
    );
    // The next press clicks as usual.
    fixture.click(cx, cell(0, Id));
    assert_eq!(
        take_events(&fixture).last(),
        Some(&click(cell_target(3, Id), 1))
    );
}

#[gpui_kit::test]
fn test_click_on_pinned_cell(cx: &mut TestAppContext) {
    let fixture = open_rows(cx, 2);
    // The band reuses data key 0: its key is of its own key space.
    fixture
        .writer(cx)
        .set_pinned_rows(PinnedEdge::Top, [Arc::new(Item { id: 0, price: -1 })]);
    fixture.settle(cx);
    take_events(&fixture);
    let pinned = GridTarget::Pinned {
        edge: PinnedEdge::Top,
        key: 0,
        column: Price,
    };
    assert_eq!(pinned.key(), None, "not a data row's key");

    // Line 0 is the band: nothing is selected, and the click names the
    // pinned row. A double click on it confirms nothing.
    fixture.click(cx, cell(0, Price));
    fixture.double_click(cx, cell(0, Price));
    assert_eq!(
        take_events(&fixture),
        [click(pinned.clone(), 1), click(pinned, 2)]
    );
    assert!(fixture.selected_keys(cx).is_empty());

    // Line 1 is the first data row, key 0.
    fixture.click(cx, cell(1, Id));
    assert_eq!(
        take_events(&fixture).last(),
        Some(&click(cell_target(0, Id), 1))
    );
}

#[gpui_kit::test]
fn test_click_on_header_follows_sort_change(cx: &mut TestAppContext) {
    let fixture = open_rows(cx, 3);
    fixture.click(cx, cell(2, Id));
    take_events(&fixture);

    // The sort first, then the click; the selection does not change.
    fixture.click(cx, header(Price));
    assert_eq!(
        take_events(&fixture),
        [
            Seen::Sort(vec![(Price, Ascending)]),
            click(GridTarget::Header { column: Price }, 1),
        ]
    );

    // A double click is two steps of the sort cycle, each before its click.
    fixture.click(cx, header(Id));
    fixture.double_click(cx, header(Id));
    let id_header = GridTarget::Header { column: Id };
    assert_eq!(
        take_events(&fixture),
        [
            Seen::Sort(vec![(Id, Ascending)]),
            click(id_header.clone(), 1),
            Seen::Sort(vec![(Id, Descending)]),
            click(id_header, 2),
        ]
    );
    assert_eq!(fixture.selected_keys(cx), [2]);

    // Right of the columns: the empty area, nothing sorted.
    fixture.click(cx, point(px(300.), px(ROW / 2.)));
    assert_eq!(take_events(&fixture), [click(GridTarget::Empty, 1)]);
}

#[gpui_kit::test]
fn test_click_on_empty_space(cx: &mut TestAppContext) {
    let fixture = open_rows(cx, 3);
    fixture.click(cx, cell(1, Id));
    take_events(&fixture);

    // Below the rows: no row, so nothing is selected, and the click is
    // `Empty`.
    fixture.click(cx, cell(6, Price));
    assert_eq!(take_events(&fixture), [click(GridTarget::Empty, 1)]);
    assert_eq!(fixture.selected_keys(cx), [1]);
}

#[gpui_kit::test]
fn test_double_click_counts_and_confirms(cx: &mut TestAppContext) {
    let fixture = open_rows(cx, 3);

    // The second press confirms at once; its click comes at the release.
    fixture.click(cx, cell(2, Price));
    fixture.double_click(cx, cell(2, Price));
    assert_eq!(
        take_events(&fixture),
        [
            Seen::Selection(vec![2]),
            Seen::Focus(Some(2)),
            click(cell_target(2, Price), 1),
            Seen::Confirm(vec![2]),
            click(cell_target(2, Price), 2),
        ]
    );
}

#[gpui_kit::test]
fn test_drag_is_no_click(cx: &mut TestAppContext) {
    let fixture = open_configured(cx, 5, Some(Size::XSmall), |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_tuning(unthrottled())
    });
    take_events(&fixture);
    let clicks = |fixture: &Fixture| {
        take_events(fixture)
            .into_iter()
            .filter(|seen| matches!(seen, Seen::Click(..)))
            .count()
    };

    // A drag-selection: one selection, no click.
    let (from, to) = (cell(1, Price), cell(4, Price));
    fixture.input(cx, |window, cx| window.drag(from, to, cx));
    fixture.settle(cx);
    assert_eq!(fixture.selected_keys(cx), [1, 2, 3, 4]);
    assert_eq!(clicks(&fixture), 0);

    // Out past the threshold and back before the release: still a drag.
    fixture.input(cx, |window, cx| {
        mouse_down_at(window, from, MouseButton::Left, Modifiers::default(), 1, cx);
        drag_move_to(window, from + point(px(0.), px(10.)), cx);
        drag_move_to(window, from, cx);
        mouse_up_at(window, from, MouseButton::Left, Modifiers::default(), 1, cx);
    });
    fixture.settle(cx);
    assert_eq!(clicks(&fixture), 0);

    // A move within the threshold is a click.
    fixture.input(cx, |window, cx| {
        mouse_down_at(window, from, MouseButton::Left, Modifiers::default(), 1, cx);
        drag_move_to(window, from + point(px(1.), px(1.)), cx);
        mouse_up_at(
            window,
            from + point(px(1.), px(1.)),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
    });
    fixture.settle(cx);
    assert_eq!(clicks(&fixture), 1);

    // A column move: the order changes, no sort, no click.
    fixture.input(cx, |window, cx| {
        window.drag(header(Id), point(px(190.), px(ROW / 2.)), cx)
    });
    fixture.settle(cx);
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.columns().collect::<Vec<_>>()),
        [Price, Id]
    );
    let events = take_events(&fixture);
    assert!(events.is_empty(), "{events:?}");

    // A resize, and a press on a resize handle that does not move: no
    // click (Price is now x 0..100, its handle at 100).
    let handle = point(px(100.), px(ROW / 2.));
    fixture.input(cx, |window, cx| {
        window.drag(handle, handle + point(px(40.), px(0.)), cx)
    });
    fixture.settle(cx);
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.column_width(Price)),
        px(140.)
    );
    let handle = point(px(140.), px(ROW / 2.));
    fixture.click(cx, handle);
    assert_eq!(clicks(&fixture), 0);
}

#[gpui_kit::test]
fn test_menu_presses_are_no_clicks(cx: &mut TestAppContext) {
    let fixture = open_rows(cx, 4);
    // The menu opens at the press, x 150 on; the presses that close it are
    // left of it.
    let open_menu = |fixture: &Fixture, cx: &mut TestAppContext| {
        fixture.press_button(cx, cell(1, Price), MouseButton::Right);
        assert!(fixture.is_menu_open(cx));
    };

    // A context press selects its row and opens the menu: the menu is its
    // result, not a click.
    open_menu(&fixture, cx);
    assert_eq!(
        take_events(&fixture),
        [Seen::Selection(vec![1]), Seen::Focus(Some(1))]
    );

    // The press that closes the menu does nothing else: on a row, on a
    // header (no sort either), with another button.
    fixture.click(cx, cell(3, Id));
    assert!(!fixture.is_menu_open(cx));
    open_menu(&fixture, cx);
    fixture.click(cx, header(Id));
    assert!(!fixture.is_menu_open(cx));
    open_menu(&fixture, cx);
    fixture.press_button(cx, cell(3, Id), MouseButton::Middle);
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(take_events(&fixture), [], "no selection, sort or click");
    assert_eq!(fixture.selected_keys(cx), [1]);

    // With the menu closed, a press clicks again.
    fixture.click(cx, cell(3, Id));
    assert_eq!(
        take_events(&fixture),
        [
            Seen::Selection(vec![3]),
            Seen::Focus(Some(3)),
            click(cell_target(3, Id), 1),
        ]
    );
}

#[gpui_kit::test]
fn test_totals_click(cx: &mut TestAppContext) {
    use gpui_kit_grid::{Aggregate, EveryGroupFooter, GroupBy};

    use crate::common::{GroupedColumn, group_path, grouped_cell, grouped_rows, open_grouped};
    use GroupedColumn::{Instrument, Qty, Side};

    // On screen: AAPL, 0, 4, 8, AAPL's footer (line 4), ...; the totals on
    // line 9.
    let fixture = open_grouped(cx, grouped_rows(12), |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(GroupedColumn::Id, Ascending)])
                .with_group_by([GroupBy::column(Instrument)])
                .with_aggregates([(Qty, Aggregate::Sum)])
                .with_group_footer(EveryGroupFooter),
        )
        .with_tuning(unthrottled())
        .with_totals(PinnedEdge::Bottom)
    });
    // Row 0 selected and focused first: the clicks leave both alone.
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(0), cx));
    fixture.settle(cx);
    fixture.events.borrow_mut().clear();
    let click_at = |cx: &mut TestAppContext, position: Point<Pixels>| {
        cx.update_window(fixture.window, |_, window, cx| {
            press_at(
                window,
                position,
                MouseButton::Left,
                Modifiers::default(),
                1,
                cx,
            )
        })
        .expect("window is open");
        fixture.settle(cx);
        std::mem::take(&mut *fixture.events.borrow_mut())
            .into_iter()
            .filter_map(|event| match event {
                LiveGridEvent::Click {
                    target,
                    button,
                    click_count,
                    ..
                } => Some(Ok((target, button, click_count))),
                event @ (LiveGridEvent::SelectionChange { .. }
                | LiveGridEvent::FocusChange { .. }
                | LiveGridEvent::Confirm { .. }) => Some(Err(format!("{event:?}"))),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    // The totals cell under the press, then the footer's: a click each,
    // after the release, and nothing else.
    assert_eq!(
        click_at(cx, grouped_cell(9, Qty)),
        [Ok((
            GridTarget::Totals {
                edge: PinnedEdge::Bottom,
                column: Qty,
            },
            MouseButton::Left,
            1
        ))]
    );
    assert_eq!(
        click_at(cx, grouped_cell(4, Side)),
        [Ok((
            GridTarget::GroupFooter {
                group: group_path(&[(Instrument, "AAPL")]),
                column: Side,
            },
            MouseButton::Left,
            1
        ))]
    );
    let focused = fixture.grid.read_with(cx, |grid, _| {
        grid.focused_cell().and_then(|cell| cell.row().copied())
    });
    assert_eq!(focused, Some(0));
}
