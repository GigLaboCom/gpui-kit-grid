//! Headless UI tests of dragging rows. The drag: which press drags, the
//! threshold, the payload, Escape, and the gestures a row drag must not take
//! over (a drag-selection, an edit). The drop: drops on rows, between rows, on
//! captions and below the rows; refused targets; the application's own type;
//! the indicator, live data and auto-scroll.

mod common;

use gpui_kit::component::Size;
use gpui_kit::{
    App, AppContext as _, Modifiers, MouseButton, Pixels, Point, TestAppContext, point, px,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    DragPayload, LiveGridDelegate, LiveGridEvent, LiveGridState, RowDragContext, SelectedRowsDrag,
    SelectionMode,
};

use crate::common::{
    EditFixture, Fixture, Item, Items, OrderColumn, ROW, click_with, drag_move_to, mouse_down_at,
    mouse_up_at, open_editable, open_with, order_cell,
};

/// The centre of line `line` of the rows of an `xsmall` grid of items.
fn line(line: usize) -> Point<Pixels> {
    point(px(100.), px(ROW * (line as f32 + 1.5)))
}

/// `at` moved down by `dy`.
fn below(at: Point<Pixels>, dy: f32) -> Point<Pixels> {
    point(at.x, at.y + px(dy))
}

fn has_drag(cx: &mut TestAppContext) -> bool {
    cx.update(|cx| cx.has_active_drag())
}

/// A grid of 20 items, its rows dragged by `SelectedRowsDrag` unless
/// `configure` says otherwise.
fn open<D: LiveGridDelegate<Row = Item>>(
    cx: &mut TestAppContext,
    delegate: D,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> Fixture<D> {
    open_with(cx, 20, Some(Size::XSmall), delegate, configure)
}

/// The gestures of a test on a [`Fixture`].
trait Pointer {
    fn click(&self, cx: &mut TestAppContext, at: Point<Pixels>, modifiers: Modifiers);
    fn down(&self, cx: &mut TestAppContext, at: Point<Pixels>);
    fn drag_to(&self, cx: &mut TestAppContext, at: Point<Pixels>);
    fn up(&self, cx: &mut TestAppContext, at: Point<Pixels>);
}

impl<D: LiveGridDelegate<Row = Item>> Pointer for Fixture<D> {
    fn click(&self, cx: &mut TestAppContext, at: Point<Pixels>, modifiers: Modifiers) {
        cx.update_window(self.window, |_, window, cx| {
            click_with(window, at, modifiers, cx)
        })
        .expect("window is open");
    }

    fn down(&self, cx: &mut TestAppContext, at: Point<Pixels>) {
        cx.update_window(self.window, |_, window, cx| {
            mouse_down_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx)
        })
        .expect("window is open");
    }

    fn drag_to(&self, cx: &mut TestAppContext, at: Point<Pixels>) {
        cx.update_window(self.window, |_, window, cx| drag_move_to(window, at, cx))
            .expect("window is open");
    }

    fn up(&self, cx: &mut TestAppContext, at: Point<Pixels>) {
        cx.update_window(self.window, |_, window, cx| {
            mouse_up_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx)
        })
        .expect("window is open");
    }
}

/// Selects the rows on `lines` with clicks: the first plain, the others
/// with the platform toggle.
fn select_lines<D: LiveGridDelegate<Row = Item>>(
    fixture: &Fixture<D>,
    cx: &mut TestAppContext,
    lines: &[usize],
) {
    for (ix, each) in lines.iter().enumerate() {
        let modifiers = if ix == 0 {
            Modifiers::default()
        } else {
            Modifiers::secondary_key()
        };
        fixture.click(cx, line(*each), modifiers);
    }
    fixture.settle(cx);
}

fn clicks<D: LiveGridDelegate<Row = Item>>(fixture: &Fixture<D>) -> usize {
    fixture
        .events
        .borrow()
        .iter()
        .filter(|event| matches!(event, LiveGridEvent::Click { .. }))
        .count()
}

#[gpui_kit::test]
fn test_row_drag_off_by_default(cx: &mut TestAppContext) {
    let fixture = open(cx, Items, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    select_lines(&fixture, cx, &[2, 4, 6]);
    // A press on a selected row of several: no drag, and the selection stays.
    fixture.down(cx, line(4));
    fixture.drag_to(cx, below(line(4), 15.));
    fixture.drag_to(cx, below(line(4), 30.));
    assert!(!has_drag(cx));
    assert!(
        !fixture
            .grid
            .read_with(cx, |grid, _| grid.is_dragging_rows())
    );
    fixture.up(cx, below(line(4), 30.));
    fixture.settle(cx);
    assert_eq!(fixture.selected_keys(cx), [2, 4, 6]);
    // One selected row: the press drag-selects.
    select_lines(&fixture, cx, &[7]);
    fixture.down(cx, line(7));
    fixture.drag_to(cx, line(9));
    fixture.up(cx, line(9));
    fixture.settle(cx);
    assert!(!has_drag(cx));
    assert_eq!(fixture.selected_keys(cx), [7, 8, 9]);
}

#[gpui_kit::test]
fn test_row_drag_starts_on_selected(cx: &mut TestAppContext) {
    let fixture = open(cx, Items, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_row_drag_gesture(SelectedRowsDrag::new())
    });
    select_lines(&fixture, cx, &[7, 2, 5]);
    fixture.events.borrow_mut().clear();
    let dragging = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_dragging_rows())
    };
    fixture.down(cx, line(5));
    // 5 px: no drag, no drag-selection.
    fixture.drag_to(cx, below(line(5), 5.));
    assert!(!dragging(cx) && !has_drag(cx));
    assert_eq!(fixture.selected_keys(cx), [2, 5, 7]);
    // 12 px: armed; the next move starts GPUI's drag.
    fixture.drag_to(cx, below(line(5), 12.));
    assert!(dragging(cx));
    fixture.drag_to(cx, below(line(5), 15.));
    assert!(has_drag(cx));
    let (keys, source) = fixture.grid.read_with(cx, |grid, _| {
        let drag = grid.row_drag().expect("a row drag");
        (drag.keys().to_vec(), drag.grid())
    });
    assert_eq!(keys, [2, 5, 7], "the shown order");
    assert_eq!(source, fixture.grid.entity_id());
    assert_eq!(
        cx.update(|cx| cx.active_drag_cursor_style()),
        Some(gpui_kit::CursorStyle::ClosedHand)
    );
    // The selection and the focus stay; the release clicks nothing.
    fixture.drag_to(cx, line(12));
    fixture.up(cx, line(12));
    fixture.settle(cx);
    assert!(!dragging(cx) && !has_drag(cx));
    assert_eq!(fixture.selected_keys(cx), [2, 5, 7]);
    let focused = fixture.grid.read_with(cx, |grid, _| {
        grid.focused_cell().and_then(|focus| focus.row().copied())
    });
    assert_eq!(focused, Some(5));
    assert_eq!(clicks(&fixture), 0);
    // A press on a selected row released where it went down collapses the
    // selection, as before.
    fixture.down(cx, line(5));
    fixture.up(cx, line(5));
    fixture.settle(cx);
    assert_eq!(fixture.selected_keys(cx), [5]);
}

#[gpui_kit::test]
fn test_row_drag_unselected_press_selects(cx: &mut TestAppContext) {
    let fixture = open(cx, Items, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_row_drag_gesture(SelectedRowsDrag::new())
    });
    select_lines(&fixture, cx, &[2]);
    fixture.down(cx, line(4));
    fixture.drag_to(cx, line(6));
    fixture.drag_to(cx, line(7));
    fixture.up(cx, line(7));
    fixture.settle(cx);
    assert!(!has_drag(cx));
    assert_eq!(fixture.selected_keys(cx), [4, 5, 6, 7]);
}

#[gpui_kit::test]
fn test_row_drag_single_selected(cx: &mut TestAppContext) {
    let fixture = open(cx, Items, |grid| {
        grid.with_row_drag_gesture(SelectedRowsDrag::new())
    });
    select_lines(&fixture, cx, &[3]);
    fixture.down(cx, line(3));
    fixture.drag_to(cx, below(line(3), 12.));
    fixture.drag_to(cx, below(line(3), 40.));
    assert!(has_drag(cx));
    let keys = fixture.grid.read_with(cx, |grid, _| {
        grid.row_drag().map(|drag| drag.keys().to_vec())
    });
    assert_eq!(keys, Some(vec![3]));
    assert_eq!(fixture.selected_keys(cx), [3], "no drag-selection");
    fixture.up(cx, below(line(3), 40.));
}

/// Refuses odd rows; labels even ones.
struct EvenRows;

impl LiveGridDelegate for EvenRows {
    type Row = Item;

    fn drag_payload(&self, drag: &RowDragContext<'_, Item>, _: &App) -> Option<DragPayload> {
        (drag.pressed() % 2 == 0).then(|| {
            DragPayload::new()
                .with_data(drag.keys().len())
                .with_label("even rows")
        })
    }
}

#[gpui_kit::test]
fn test_row_drag_refused(cx: &mut TestAppContext) {
    let fixture = open(cx, EvenRows, |grid| {
        grid.with_row_drag_gesture(SelectedRowsDrag::new())
    });
    // Refused: no drag, and no drag-selection either.
    select_lines(&fixture, cx, &[3]);
    fixture.down(cx, line(3));
    fixture.drag_to(cx, below(line(3), 12.));
    fixture.drag_to(cx, line(6));
    assert!(!has_drag(cx));
    fixture.up(cx, line(6));
    fixture.settle(cx);
    assert_eq!(fixture.selected_keys(cx), [3]);
    // Taken: the payload rides along.
    select_lines(&fixture, cx, &[4]);
    fixture.down(cx, line(4));
    fixture.drag_to(cx, below(line(4), 12.));
    fixture.drag_to(cx, line(6));
    assert!(has_drag(cx));
    let (label, data) = fixture.grid.read_with(cx, |grid, _| {
        let payload = grid.row_drag().expect("a row drag").payload();
        (
            payload.label().map(ToString::to_string),
            payload.data::<usize>().copied(),
        )
    });
    assert_eq!(label.as_deref(), Some("even rows"));
    assert_eq!(data, Some(1));
    fixture.up(cx, line(6));
}

#[gpui_kit::test]
fn test_row_drag_escape(cx: &mut TestAppContext) {
    let fixture = open(cx, Items, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_row_drag_gesture(SelectedRowsDrag::new())
    });
    select_lines(&fixture, cx, &[2, 3]);
    fixture.down(cx, line(2));
    fixture.drag_to(cx, below(line(2), 12.));
    fixture.drag_to(cx, line(6));
    assert!(has_drag(cx));
    cx.update_window(fixture.window, |_, window, cx| window.press("escape", cx))
        .expect("window is open");
    fixture.settle(cx);
    assert!(!has_drag(cx));
    assert!(
        !fixture
            .grid
            .read_with(cx, |grid, _| grid.is_dragging_rows())
    );
    assert_eq!(
        fixture.selected_keys(cx),
        [2, 3],
        "Escape kept the selection"
    );
    fixture.up(cx, line(6));
    fixture.settle(cx);
    assert_eq!(fixture.selected_keys(cx), [2, 3]);
}

#[gpui_kit::test]
fn test_drag_and_edit(cx: &mut TestAppContext) {
    use OrderColumn::{Note, Price};
    let fixture: EditFixture<_> = open_editable(cx, |grid| {
        grid.with_row_drag_gesture(SelectedRowsDrag::new())
    });
    let dragging = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_dragging_rows())
    };
    let press = |cx: &mut TestAppContext, at: Point<Pixels>| {
        cx.update_window(fixture.window, |_, window, cx| {
            mouse_down_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx)
        })
        .expect("window is open");
    };
    let move_to = |cx: &mut TestAppContext, at: Point<Pixels>| {
        cx.update_window(fixture.window, |_, window, cx| drag_move_to(window, at, cx))
            .expect("window is open");
    };
    let release = |cx: &mut TestAppContext, at: Point<Pixels>| {
        cx.update_window(fixture.window, |_, window, cx| {
            mouse_up_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx)
        })
        .expect("window is open");
    };
    // A click on the selected row: no drag, and no edit (Excel's gesture
    // edits on a double click).
    fixture.press_cell(cx, 2, Price, 1);
    fixture.settle(cx);
    let at = order_cell(2, Price);
    press(cx, at);
    release(cx, at);
    fixture.settle(cx);
    assert!(!dragging(cx) && !has_drag(cx));
    assert_eq!(fixture.editing(cx), None);
    // 12 px: a drag, no edit.
    press(cx, at);
    move_to(cx, below(at, 12.));
    move_to(cx, below(at, 20.));
    assert!(has_drag(cx));
    assert_eq!(fixture.editing(cx), None);
    release(cx, below(at, 20.));
    fixture.settle(cx);
    // A press in an open field selects its text: no row drag.
    fixture.focus_cell(cx, 3, Note);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    let field = order_cell(2, Note);
    press(cx, field);
    move_to(cx, point(field.x + px(20.), field.y));
    move_to(cx, point(field.x + px(30.), field.y));
    assert!(!has_drag(cx) && !dragging(cx));
    release(cx, point(field.x + px(30.), field.y));
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Note)));
}

// Dropping.

use gpui_kit::{CursorStyle, Entity};
use gpui_kit_grid::{
    DropBetweenRows, DropOnRows, DropPlace, DropTarget, GridColumns as _, GroupBy, GroupPath,
    OwnedValue, ReorderContext, ReorderDecision, ReorderPolicy, SortDirection, ViewSpec,
};

use crate::common::{DroppedSource, ItemColumn, Pair, Tag, open_pair, unthrottled};

/// Grid A drags its selected rows.
fn source(grid: LiveGridState<Items>) -> LiveGridState<Items> {
    grid.with_selection_mode(SelectionMode::RowSet)
        .with_row_drag_gesture(SelectedRowsDrag::new())
}

fn onto(key: u32) -> DropTarget<u32> {
    DropTarget::Row {
        key,
        place: DropPlace::Onto,
    }
}

fn paint_order(cx: &mut TestAppContext, grid: &Entity<LiveGridState<Items>>) -> Vec<&'static str> {
    grid.read_with(cx, |grid, _| grid.last_paint_order().to_vec())
}

#[gpui_kit::test]
fn test_drop_on_rows(cx: &mut TestAppContext) {
    let pair = open_pair(cx, 20, 5, source, |grid| {
        grid.with_row_drop_policy(DropOnRows)
    });
    let a = pair.a.entity_id();
    // On a data row of B.
    pair.drag_rows(cx, &[4, 2], pair.line(true, 3));
    pair.drag_to(cx, below(pair.line(true, 3), 2.));
    assert!(paint_order(cx, &pair.b).contains(&"drop"), "the indicator");
    assert_eq!(
        cx.update(|cx| cx.active_drag_cursor_style()),
        Some(CursorStyle::ClosedHand)
    );
    pair.up(cx, below(pair.line(true, 3), 2.));
    pair.settle(cx);
    assert_eq!(
        Pair::drops(&pair.events_b),
        [(DroppedSource::Rows(vec![2, 4], a), onto(3))]
    );
    assert!(Pair::drops(&pair.events_a).is_empty());
    assert!(!paint_order(cx, &pair.b).contains(&"drop"));
    // Below B's rows: `Empty`.
    pair.events_b.borrow_mut().clear();
    pair.drag_rows(cx, &[6], pair.line(true, 8));
    pair.drag_to(cx, below(pair.line(true, 8), 2.));
    pair.up(cx, below(pair.line(true, 8), 2.));
    pair.settle(cx);
    assert_eq!(
        Pair::drops(&pair.events_b),
        [(DroppedSource::Rows(vec![6], a), DropTarget::Empty)]
    );
}

#[gpui_kit::test]
fn test_drop_on_caption(cx: &mut TestAppContext) {
    // B grouped by price: a caption on line 0, its row on line 1.
    let pair = open_pair(cx, 20, 5, source, |grid| {
        grid.with_row_drop_policy(DropOnRows)
            .with_view(ViewSpec::new().with_group_by([GroupBy::column(ItemColumn::Price)]))
    });
    pair.drag_rows(cx, &[1], pair.line(true, 2));
    pair.drag_to(cx, pair.line(true, 0));
    pair.up(cx, pair.line(true, 0));
    pair.settle(cx);
    let path = GroupPath::new([(ItemColumn::Price.column_id(), OwnedValue::Int(0))]);
    let drops = Pair::drops(&pair.events_b);
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].1, DropTarget::Group(path));
}

#[gpui_kit::test]
fn test_drop_between_rows(cx: &mut TestAppContext) {
    let pair = open_pair(cx, 20, 10, source, |grid| {
        grid.with_row_drop_policy(DropBetweenRows)
    });
    let at = |half: f32| below(pair.line(true, 3), half);
    // The upper half: before; the lower half: after.
    pair.drag_rows(cx, &[1], at(-6.));
    pair.drag_to(cx, at(-5.));
    pair.up(cx, at(-5.));
    pair.drag_rows(cx, &[1], at(6.));
    pair.drag_to(cx, at(5.));
    pair.up(cx, at(5.));
    pair.settle(cx);
    let targets: Vec<_> = Pair::drops(&pair.events_b)
        .into_iter()
        .map(|(_, target)| target)
        .collect();
    assert_eq!(
        targets,
        [
            DropTarget::Row {
                key: 3,
                place: DropPlace::Before
            },
            DropTarget::Row {
                key: 3,
                place: DropPlace::After
            },
        ]
    );
    // A sorted flat view refuses: the place would not hold.
    let pair = open_pair(cx, 20, 10, source, |grid| {
        grid.with_row_drop_policy(DropBetweenRows)
            .with_view(ViewSpec::new().with_sort([(ItemColumn::Price, SortDirection::Descending)]))
    });
    pair.drag_rows(cx, &[1], pair.line(true, 3));
    pair.drag_to(cx, below(pair.line(true, 3), 2.));
    assert_eq!(
        cx.update(|cx| cx.active_drag_cursor_style()),
        Some(CursorStyle::OperationNotAllowed)
    );
    pair.up(cx, below(pair.line(true, 3), 2.));
    pair.settle(cx);
    assert!(Pair::drops(&pair.events_b).is_empty());
}

#[gpui_kit::test]
fn test_drop_refused_targets(cx: &mut TestAppContext) {
    // B takes no drop: the cursor says so, no event.
    let pair = open_pair(cx, 20, 10, source, |grid| grid);
    pair.drag_rows(cx, &[1], pair.line(true, 3));
    pair.drag_to(cx, below(pair.line(true, 3), 2.));
    assert_eq!(
        cx.update(|cx| cx.active_drag_cursor_style()),
        Some(CursorStyle::OperationNotAllowed)
    );
    // Back over A, its own rows: the closed hand.
    pair.drag_to(cx, pair.line(false, 6));
    assert_eq!(
        cx.update(|cx| cx.active_drag_cursor_style()),
        Some(CursorStyle::ClosedHand)
    );
    pair.drag_to(cx, pair.line(true, 3));
    pair.up(cx, pair.line(true, 3));
    pair.settle(cx);
    assert!(Pair::drops(&pair.events_b).is_empty());
    // B takes drops, but not on its header.
    let pair = open_pair(cx, 20, 10, source, |grid| {
        grid.with_row_drop_policy(DropOnRows)
    });
    let header = point(px(500.), px(ROW / 2.));
    pair.drag_rows(cx, &[1], header);
    pair.drag_to(cx, below(header, 1.));
    pair.up(cx, below(header, 1.));
    pair.settle(cx);
    assert!(Pair::drops(&pair.events_b).is_empty());
}

#[gpui_kit::test]
fn test_drop_own_grid(cx: &mut TestAppContext) {
    let pair = open_pair(
        cx,
        20,
        5,
        |grid| source(grid).with_row_drop_policy(DropOnRows),
        |grid| grid,
    );
    pair.drag_rows(cx, &[2, 3], pair.line(false, 7));
    pair.drag_to(cx, below(pair.line(false, 7), 2.));
    pair.up(cx, below(pair.line(false, 7), 2.));
    pair.settle(cx);
    assert_eq!(
        Pair::drops(&pair.events_a),
        [(DroppedSource::Rows(vec![2, 3], pair.a.entity_id()), onto(7))]
    );
    let selected = pair.a.read_with(cx, |grid, _| {
        let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
        keys.sort_unstable();
        keys
    });
    assert_eq!(selected, [2, 3]);
}

/// Drags the window's `"tag"` element to `to`.
fn drag_tag(pair: &Pair, cx: &mut TestAppContext, to: Point<Pixels>) {
    let tag = point(px(12.), px(6.));
    pair.down(cx, tag);
    pair.drag_to(cx, point(px(30.), px(40.)));
    pair.drag_to(cx, to);
    pair.drag_to(cx, below(to, 1.));
    pair.up(cx, below(to, 1.));
    pair.settle(cx);
}

#[gpui_kit::test]
fn test_drop_registered_payload(cx: &mut TestAppContext) {
    let pair = open_pair(
        cx,
        5,
        10,
        |grid| grid,
        |grid| {
            grid.with_row_drop_policy(DropOnRows)
                .with_drop_payload::<Tag>()
        },
    );
    drag_tag(&pair, cx, pair.line(true, 2));
    assert_eq!(
        Pair::drops(&pair.events_b),
        [(DroppedSource::Tag(Some(Tag(7))), onto(2))]
    );
    // Without the registration: nothing.
    let pair = open_pair(
        cx,
        5,
        10,
        |grid| grid,
        |grid| grid.with_row_drop_policy(DropOnRows),
    );
    drag_tag(&pair, cx, pair.line(true, 2));
    assert!(Pair::drops(&pair.events_b).is_empty());
}

#[gpui_kit::test]
fn test_drop_target_follows_key(cx: &mut TestAppContext) {
    let pair = open_pair(cx, 20, 10, source, |grid| {
        grid.with_row_drop_policy(DropOnRows)
            .with_tuning(unthrottled())
            .with_view(ViewSpec::new().with_sort([(ItemColumn::Price, SortDirection::Ascending)]))
    });
    pair.drag_rows(cx, &[1], pair.line(true, 3));
    pair.drag_to(cx, below(pair.line(true, 3), 2.));
    assert!(paint_order(cx, &pair.b).contains(&"drop"));
    // The stream moves row 3 to line 5: the indicator goes with it.
    let writer = pair.b.read_with(cx, |grid, _| grid.writer());
    writer.upsert(Item { id: 3, price: 55 });
    pair.settle(cx);
    assert_eq!(
        pair.b.read_with(cx, |grid, _| grid.visible_keys())[..6],
        [0, 1, 2, 4, 5, 3]
    );
    assert!(paint_order(cx, &pair.b).contains(&"drop"));
    // The drop lands on the row under the pointer now.
    pair.up(cx, below(pair.line(true, 3), 2.));
    pair.settle(cx);
    assert_eq!(Pair::drops(&pair.events_b)[0].1, onto(4));
}

#[gpui_kit::test]
fn test_drag_auto_scroll(cx: &mut TestAppContext) {
    let pair = open_pair(cx, 20, 40, source, |grid| {
        grid.with_row_drop_policy(DropOnRows)
    });
    let first = |cx: &mut TestAppContext| pair.b.read_with(cx, |grid, _| grid.visible_keys()[0]);
    assert_eq!(first(cx), 0);
    // Near the bottom of B's rows.
    let edge = point(px(500.), px(crate::common::WINDOW_HEIGHT - 4.));
    pair.drag_rows(cx, &[1], edge);
    pair.drag_to(cx, below(edge, 1.));
    for _ in 0..10 {
        pair.frame(cx, std::time::Duration::from_millis(16));
    }
    assert!(first(cx) > 0, "B scrolled");
    let last = pair
        .b
        .read_with(cx, |grid, _| *grid.visible_keys().last().expect("rows"));
    pair.up(cx, below(edge, 1.));
    pair.settle(cx);
    assert_eq!(Pair::drops(&pair.events_b)[0].1, onto(last));
}

#[gpui_kit::test]
fn test_drop_indicator_cleared(cx: &mut TestAppContext) {
    let pair = open_pair(cx, 20, 10, source, |grid| {
        grid.with_row_drop_policy(DropOnRows)
    });
    pair.drag_rows(cx, &[1], pair.line(true, 3));
    pair.drag_to(cx, below(pair.line(true, 3), 2.));
    assert!(paint_order(cx, &pair.b).contains(&"drop"));
    // Out of the window: the indicator goes, and the release drops nothing.
    let away = point(px(900.), px(100.));
    pair.drag_to(cx, away);
    pair.settle(cx);
    assert!(!paint_order(cx, &pair.b).contains(&"drop"));
    pair.up(cx, away);
    pair.settle(cx);
    assert!(Pair::drops(&pair.events_b).is_empty());
    // Escape over B: the drag and the indicator go.
    pair.drag_rows(cx, &[1], pair.line(true, 3));
    pair.drag_to(cx, below(pair.line(true, 3), 2.));
    assert!(paint_order(cx, &pair.b).contains(&"drop"));
    cx.update_window(pair.window, |_, window, cx| window.press("escape", cx))
        .expect("window is open");
    pair.settle(cx);
    assert!(!has_drag(cx));
    assert!(!paint_order(cx, &pair.b).contains(&"drop"));
    pair.up(cx, pair.line(true, 3));
    pair.settle(cx);
    assert!(Pair::drops(&pair.events_b).is_empty());
}

/// Holds a new order while a drag is over the rows.
struct HoldWhileDragOver;

impl ReorderPolicy for HoldWhileDragOver {
    fn decide(&self, context: &ReorderContext) -> ReorderDecision {
        if context.is_drag_over() && !context.is_user_action() {
            ReorderDecision::Hold
        } else {
            ReorderDecision::Apply
        }
    }
}

#[gpui_kit::test]
fn test_reorder_while_drag_over(cx: &mut TestAppContext) {
    let pair = open_pair(cx, 20, 10, source, |grid| {
        grid.with_row_drop_policy(DropOnRows)
            .with_reorder_policy(HoldWhileDragOver)
            .with_tuning(unthrottled())
            .with_view(ViewSpec::new().with_sort([(ItemColumn::Price, SortDirection::Ascending)]))
    });
    pair.drag_rows(cx, &[1], pair.line(true, 3));
    pair.drag_to(cx, below(pair.line(true, 3), 2.));
    let writer = pair.b.read_with(cx, |grid, _| grid.writer());
    writer.upsert(Item { id: 3, price: 55 });
    pair.settle(cx);
    assert!(pair.b.read_with(cx, |grid, _| grid.is_order_held()));
    // The drop ends the drag: the order shows.
    pair.up(cx, below(pair.line(true, 3), 2.));
    pair.settle(cx);
    assert!(!pair.b.read_with(cx, |grid, _| grid.is_order_held()));
}
