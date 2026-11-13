//! Headless UI tests of selection, focus and the keyboard
//! (`.agents/COMPONENT_TEST_RULES.md`): navigation over a virtualized range,
//! pointer gestures with modifiers, Cmd/Ctrl+A under live data, the successor
//! rule, Enter, Escape and Space, focus-visible, drag-selection, the cell modes
//! and the scroll anchor. Assertions read the state and the events, not pixels.

mod common;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, Modifiers, MouseButton, MouseMoveEvent,
    ParentElement as _, Pixels, Point, Render, Styled as _, TestAppContext, Window, WindowBounds,
    WindowOptions, div, point, px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    CellValue, ColumnIx, ColumnSet, ColumnZone, GestureInput, GridColumn, GridColumns, GridRow,
    GridSchema, GridTuning, LayoutOrigin, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState,
    ManualReorder, SelectionEdit, SelectionGesture, SelectionGestureContext, SelectionMode,
    SelectionOrigin, SortDirection, StoreOptions, StoreScope, ViewSpec,
};

use crate::common::{PausableRunner, drag_move_to, mouse_down_at, mouse_up_at, press_at};

const WIDTH: f32 = 400.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Col {
    Id,
    Price,
    Size,
    Note,
    Total,
}

impl GridColumns for Col {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Price, Self::Size, Self::Note, Self::Total]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Price => "price",
            Self::Size => "size",
            Self::Note => "note",
            Self::Total => "total",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Clone, Debug)]
struct Item {
    id: u32,
    price: i64,
}

impl GridRow for Item {
    type Key = u32;
    type Column = Col;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: Col) -> CellValue<'_> {
        match column {
            Col::Id => CellValue::Int(self.id.into()),
            Col::Price => CellValue::Int(self.price),
            Col::Size => CellValue::Int(100),
            Col::Note => CellValue::Text("note"),
            Col::Total => CellValue::Int(0),
        }
    }
}

struct Items;

impl LiveGridDelegate for Items {
    type Row = Item;
}

/// Id frozen left (60 px); Price 100, Size 100, Note 200 scrolling; Total
/// frozen right (60). In the 400 px window the scrolling zone is 60..340,
/// narrower than its 400 px of columns.
fn schema() -> GridSchema<Col> {
    GridSchema::new()
        .column(
            GridColumn::new(Col::Id, "Id")
                .with_width(px(60.))
                .with_zone(ColumnZone::FrozenLeft),
        )
        .column(GridColumn::new(Col::Price, "Price").with_width(px(100.)))
        .column(GridColumn::new(Col::Size, "Size").with_width(px(100.)))
        .column(GridColumn::new(Col::Note, "Note").with_width(px(200.)))
        .column(
            GridColumn::new(Col::Total, "Total")
                .with_width(px(60.))
                .with_zone(ColumnZone::FrozenRight),
        )
}

/// The grid in a window, under a div that counts the `Confirm` and `Cancel`
/// Actions the grid lets through.
struct Root {
    grid: Entity<LiveGridState<Items>>,
    size: Size,
    outer_cancels: Rc<Cell<usize>>,
    outer_confirms: Rc<Cell<usize>>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (cancels, confirms) = (self.outer_cancels.clone(), self.outer_confirms.clone());
        div()
            .size_full()
            .on_action(move |_: &Cancel, _, _| cancels.set(cancels.get() + 1))
            .on_action(move |_: &Confirm, _, _| confirms.set(confirms.get() + 1))
            .child(
                LiveGrid::new(&self.grid)
                    .with_size(self.size)
                    .bordered(false),
            )
    }
}

type Events = Rc<RefCell<Vec<LiveGridEvent<u32, Col>>>>;

struct Fixture {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<Items>>,
    events: Events,
    row: f32,
    outer_cancels: Rc<Cell<usize>>,
    outer_confirms: Rc<Cell<usize>>,
}

fn item(id: u32, price: i64) -> Arc<Item> {
    Arc::new(Item { id, price })
}

/// `count` rows (`price = id * 10`) in a window of a header and ten rows at
/// `size`, configured by `configure`.
fn open(
    cx: &mut TestAppContext,
    count: u32,
    size_: Size,
    configure: impl FnOnce(LiveGridState<Items>) -> LiveGridState<Items> + 'static,
) -> Fixture {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let row = f32::from(size_.table_row_height());
    // A header and ten `xsmall` rows: six `large` ones.
    let height = 26. * 11.;
    let (outer_cancels, outer_confirms) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
    let (window, root) = cx
        .update(|cx| {
            let (outer_cancels, outer_confirms) = (outer_cancels.clone(), outer_confirms.clone());
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(WIDTH), px(height)),
                    })),
                    ..Default::default()
                },
                cx,
                move |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            configure(
                                LiveGridState::new_standalone(
                                    schema(),
                                    Items,
                                    StoreOptions::default(),
                                    cx,
                                )
                                .with_tuning(
                                    GridTuning::default().with_min_rebuild_interval(Duration::ZERO),
                                ),
                            )
                        }),
                        size: size_,
                        outer_cancels,
                        outer_confirms,
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events = Events::default();
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(&grid, move |_, event: &LiveGridEvent<u32, Col>, _| {
            events.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all((0..count).map(|id| item(id, i64::from(id) * 10)));
    let fixture = Fixture {
        window,
        grid,
        events,
        row,
        outer_cancels,
        outer_confirms,
    };
    fixture.settle(cx);
    fixture.events.borrow_mut().clear();
    fixture
}

fn xsmall(
    cx: &mut TestAppContext,
    count: u32,
    configure: impl FnOnce(LiveGridState<Items>) -> LiveGridState<Items> + 'static,
) -> Fixture {
    open(cx, count, Size::XSmall, configure)
}

fn by_price(direction: SortDirection) -> ViewSpec<Item> {
    ViewSpec::new().with_sort([(Col::Price, direction)])
}

fn shift_secondary() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::secondary_key()
    }
}

#[derive(Debug, PartialEq)]
enum Seen {
    Selection(Vec<u32>, SelectionOrigin),
    Focus(Option<(u32, Col)>, SelectionOrigin),
    Confirm(Vec<u32>, bool),
    Layout(LayoutOrigin),
    Other,
}

impl Fixture {
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx);
        }
    }

    fn frame(&self, cx: &mut TestAppContext) {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        self.input(cx, |window, cx| window.render_frame(cx));
    }

    fn input<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut Window, &mut gpui_kit::App) -> R,
    ) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("window is open")
    }

    /// Focuses the grid without a pointer press.
    fn focus(&self, cx: &mut TestAppContext) {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.render_frame(cx);
        });
    }

    fn press(&self, cx: &mut TestAppContext, keys: &str) {
        for key in keys.split(' ') {
            self.input(cx, |window, cx| window.press(key, cx));
        }
    }

    /// The center of a body cell: screen line `line`, column `column`.
    fn cell(&self, line: usize, column: Col) -> Point<Pixels> {
        let x = match column {
            Col::Id => 30.,
            Col::Price => 110.,
            Col::Size => 210.,
            Col::Note => 300.,
            Col::Total => 370.,
        };
        point(px(x), px(self.row * (line as f32 + 1.5)))
    }

    fn click(&self, cx: &mut TestAppContext, line: usize, modifiers: Modifiers) {
        let position = self.cell(line, Col::Price);
        self.input(cx, |window, cx| {
            press_at(window, position, MouseButton::Left, modifiers, 1, cx)
        });
    }

    fn right_click(&self, cx: &mut TestAppContext, line: usize) {
        let position = self.cell(line, Col::Price);
        self.input(cx, |window, cx| {
            press_at(
                window,
                position,
                MouseButton::Right,
                Modifiers::default(),
                1,
                cx,
            )
        });
    }

    fn visible_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| grid.visible_keys())
    }

    fn selected(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| {
            let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
            keys.sort_unstable();
            keys
        })
    }

    fn scoped(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid
            .read_with(cx, |grid, _| grid.scoped_selection().keys().to_vec())
    }

    fn focused(&self, cx: &mut TestAppContext) -> Option<(u32, Col)> {
        self.grid.read_with(cx, |grid, _| {
            grid.focused_cell()
                .and_then(|cell| Some((*cell.row()?, cell.column())))
        })
    }

    fn columns_of(&self, cx: &mut TestAppContext, key: u32) -> ColumnSet {
        self.grid
            .read_with(cx, |grid, _| grid.selection().columns(&key))
    }

    fn take_events(&self) -> Vec<Seen> {
        self.events
            .borrow_mut()
            .drain(..)
            .map(|event| match event {
                LiveGridEvent::SelectionChange { selection, origin } => {
                    let mut keys: Vec<u32> = selection.iter().copied().collect();
                    keys.sort_unstable();
                    Seen::Selection(keys, origin)
                }
                LiveGridEvent::FocusChange { cell, origin } => Seen::Focus(
                    cell.and_then(|cell| Some((*cell.row()?, cell.column()))),
                    origin,
                ),
                LiveGridEvent::Confirm { keys, secondary } => {
                    Seen::Confirm(keys.to_vec(), secondary)
                }
                LiveGridEvent::LayoutChange { origin, .. } => Seen::Layout(origin),
                _ => Seen::Other,
            })
            .filter(|seen| *seen != Seen::Other)
            .collect()
    }
}

fn set(columns: &[Col]) -> ColumnSet {
    columns.iter().fold(ColumnSet::EMPTY, |mut set, column| {
        set.insert(column.ix());
        set
    })
}

#[gpui_kit::test]
fn test_keyboard_navigation_over_virtualized_range(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 100, |grid| grid);
    fixture.focus(cx);
    // The first move focuses the first cell; fourteen more go down.
    fixture.press(cx, &["down"; 15].join(" "));
    assert_eq!(fixture.focused(cx), Some((14, Col::Id)));
    assert_eq!(fixture.visible_keys(cx), (5..15).collect::<Vec<_>>());
    // A page is the rows that fit (ten `xsmall` rows): the body scrolls by a
    // page, so the focus keeps its screen line.
    fixture.press(cx, "pagedown");
    assert_eq!(fixture.focused(cx), Some((24, Col::Id)));
    assert_eq!(fixture.visible_keys(cx)[9], 24);
    fixture.press(cx, "pageup pageup");
    assert_eq!(fixture.focused(cx), Some((4, Col::Id)));
    assert_eq!(fixture.visible_keys(cx)[..5], [0, 1, 2, 3, 4]);

    // Cmd/Ctrl+End: the last row and the last column, both in view.
    fixture.press(cx, "secondary-end");
    assert_eq!(fixture.focused(cx), Some((99, Col::Total)));
    assert_eq!(fixture.visible_keys(cx).last(), Some(&99));
    // No wrap at the edges.
    fixture.press(cx, "down right");
    assert_eq!(fixture.focused(cx), Some((99, Col::Total)));
    fixture.press(cx, "secondary-home");
    assert_eq!(fixture.focused(cx), Some((0, Col::Id)));
    assert_eq!(fixture.visible_keys(cx)[0], 0);
    fixture.press(cx, "up left");
    assert_eq!(fixture.focused(cx), Some((0, Col::Id)));

    // Across the zones: frozen left, the scrolling ones, frozen right; the
    // current column follows the focus.
    fixture.press(cx, "right right right");
    assert_eq!(fixture.focused(cx), Some((0, Col::Note)));
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Note)
    );
    fixture.press(cx, "right");
    assert_eq!(fixture.focused(cx), Some((0, Col::Total)));
    fixture.press(cx, "home");
    assert_eq!(fixture.focused(cx), Some((0, Col::Id)));
    fixture.press(cx, "end secondary-down");
    assert_eq!(fixture.focused(cx), Some((99, Col::Total)));
    fixture.press(cx, "secondary-left secondary-up");
    assert_eq!(fixture.focused(cx), Some((0, Col::Id)));
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_focus_visible())
    );

    // `large` rows: six fit, so a page is six.
    let fixture = open(cx, 100, Size::Large, |grid| grid);
    fixture.focus(cx);
    fixture.press(cx, "down down down pagedown");
    assert_eq!(fixture.focused(cx), Some((8, Col::Id)));
    assert_eq!(fixture.visible_keys(cx)[2], 8, "same screen line");
}

#[gpui_kit::test]
fn test_navigation_selects_and_shift_extends(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 30, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    fixture.focus(cx);
    fixture.press(cx, "down");
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Selection(vec![0], SelectionOrigin::User),
            Seen::Focus(Some((0, Col::Id)), SelectionOrigin::User),
        ]
    );
    fixture.press(cx, "down");
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Selection(vec![1], SelectionOrigin::User),
            Seen::Focus(Some((1, Col::Id)), SelectionOrigin::User),
        ]
    );
    fixture.press(cx, "shift-down");
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Selection(vec![1, 2], SelectionOrigin::User),
            Seen::Focus(Some((2, Col::Id)), SelectionOrigin::User),
        ]
    );
    fixture.press(cx, "shift-down");
    assert_eq!(fixture.selected(cx), [1, 2, 3]);
    fixture.press(cx, "shift-up shift-up shift-up");
    assert_eq!(fixture.selected(cx), [0, 1], "the anchor stays at 1");
    // A move that changes nothing emits nothing.
    fixture.take_events();
    fixture.press(cx, "shift-up");
    assert!(fixture.take_events().is_empty());
}

#[gpui_kit::test]
fn test_mouse_modifiers(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 100, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_view(by_price(SortDirection::Ascending))
    });
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(40, cx));
    fixture.settle(cx);
    fixture.click(cx, 2, Modifiers::default());
    assert_eq!(fixture.selected(cx), [42]);
    assert_eq!(fixture.focused(cx), Some((42, Col::Price)));

    // The stream reverses the order; the focused row keeps its line.
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    for id in 0..100u32 {
        writer.upsert(Item {
            id,
            price: -i64::from(id) * 10,
        });
    }
    fixture.settle(cx);
    let keys = fixture.visible_keys(cx);
    assert_eq!(keys[2], 42);
    assert_eq!(keys[3], 41, "the new order is on screen");

    // Shift extends by the order on screen now.
    fixture.click(cx, 5, Modifiers::shift());
    let mut range = keys[2..=5].to_vec();
    range.sort_unstable();
    assert_eq!(fixture.selected(cx), range);
    // Cmd/Ctrl toggles; it sets the anchor.
    fixture.click(cx, 3, Modifiers::secondary_key());
    let mut toggled = range.clone();
    toggled.retain(|key| *key != keys[3]);
    assert_eq!(fixture.selected(cx), toggled);
    fixture.click(cx, 8, Modifiers::secondary_key());
    toggled.push(keys[8]);
    toggled.sort_unstable();
    assert_eq!(fixture.selected(cx), toggled);
    // Shift with Cmd/Ctrl adds the range from the anchor.
    fixture.click(cx, 6, shift_secondary());
    let mut added = toggled.clone();
    added.extend(&keys[6..=8]);
    added.sort_unstable();
    added.dedup();
    assert_eq!(fixture.selected(cx), added);
    assert_eq!(
        fixture.scoped(cx),
        keys.iter()
            .copied()
            .filter(|key| added.contains(key))
            .collect::<Vec<_>>(),
        "commands get display order"
    );
    // A press below the rows changes nothing.
    let fixture = xsmall(cx, 3, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    fixture.click(cx, 1, Modifiers::default());
    fixture.click(cx, 7, Modifiers::default());
    assert_eq!(fixture.selected(cx), [1]);
}

#[gpui_kit::test]
fn test_select_all_materializes_keys(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 20, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    fixture.focus(cx);
    fixture.press(cx, "secondary-a");
    assert_eq!(fixture.selected(cx), (0..20).collect::<Vec<_>>());
    assert_eq!(
        fixture.take_events(),
        [Seen::Selection((0..20).collect(), SelectionOrigin::User)],
        "the focus stays where it was (none)"
    );

    // Rows that arrive later are not selected.
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    writer.upsert(Item { id: 100, price: 0 });
    fixture.settle(cx);
    assert!(!fixture.selected(cx).contains(&100));
    // A removed key leaves; inserted again, it is a new row, not selected.
    writer.remove(3);
    fixture.settle(cx);
    let mut expected: Vec<u32> = (0..20).filter(|id| *id != 3).collect();
    assert_eq!(fixture.selected(cx), expected);
    assert_eq!(
        fixture.take_events(),
        [Seen::Selection(
            expected.clone(),
            SelectionOrigin::DataRemoved
        )]
    );
    writer.upsert(Item { id: 3, price: 30 });
    fixture.settle(cx);
    assert_eq!(fixture.selected(cx), expected);

    // A filter hides a row: the selection keeps it, commands do not get it.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_view(
            ViewSpec::new().with_filter(ColumnSet::first(1), |item: &Item| item.id != 5),
            cx,
        )
    });
    fixture.settle(cx);
    assert!(fixture.selected(cx).contains(&5));
    assert!(!fixture.scoped(cx).contains(&5));
    expected.retain(|id| *id != 5);
    assert_eq!(fixture.scoped(cx), expected);

    // A single mode has no "select all": the key goes on.
    let fixture = xsmall(cx, 20, |grid| grid);
    fixture.focus(cx);
    fixture.press(cx, "secondary-a");
    assert!(fixture.selected(cx).is_empty());
}

#[gpui_kit::test]
fn test_selection_over_pending_rebuild(cx: &mut TestAppContext) {
    let runner = PausableRunner::default();
    let fixture = xsmall(cx, 30, {
        let runner = runner.clone();
        move |grid| {
            grid.with_selection_mode(SelectionMode::RowSet)
                .with_view(by_price(SortDirection::Ascending))
                .with_task_runner(runner)
                .with_reorder_policy(ManualReorder)
        }
    });
    let shown = fixture.visible_keys(cx);
    assert_eq!(shown, (0..10).collect::<Vec<_>>());

    // New rows and a reversed order, waiting in a paused rebuild.
    runner.pause();
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    for id in 0..30u32 {
        writer.upsert(Item {
            id,
            price: -i64::from(id),
        });
    }
    writer.upsert(Item {
        id: 100,
        price: -1_000,
    });
    fixture.settle(cx);
    fixture.focus(cx);
    fixture.press(cx, "secondary-a");
    assert_eq!(fixture.selected(cx), (0..30).collect::<Vec<_>>());
    fixture.click(cx, 1, Modifiers::default());
    fixture.click(cx, 4, Modifiers::shift());
    assert_eq!(fixture.selected(cx), [1, 2, 3, 4], "the order on screen");

    // The rebuild finishes, and the manual policy holds its order back.
    runner.resume();
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    assert_eq!(fixture.visible_keys(cx), shown);
    fixture.click(cx, 6, Modifiers::shift());
    assert_eq!(fixture.selected(cx), [1, 2, 3, 4, 5, 6]);
    fixture.press(cx, "secondary-a");
    assert!(
        !fixture.selected(cx).contains(&100),
        "only the order on screen"
    );
}

#[gpui_kit::test]
fn test_focus_successor(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 20, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    fixture.click(cx, 3, Modifiers::default());
    fixture.take_events();
    // Removed: the next row by the old order.
    writer.remove(3);
    fixture.settle(cx);
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Selection(vec![], SelectionOrigin::DataRemoved),
            Seen::Focus(Some((4, Col::Price)), SelectionOrigin::DataRemoved),
        ]
    );
    // The last row removed: the one before it.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_focused_cell(&19, None, cx));
    writer.remove(19);
    fixture.settle(cx);
    assert_eq!(fixture.focused(cx), Some((18, Col::Price)));
    // A filter hides the focused row: its successor, `Filtered`; the
    // selection does not change.
    fixture.click(cx, 2, Modifiers::default());
    let focused = fixture.focused(cx).expect("a focus").0;
    fixture.take_events();
    fixture.grid.update(cx, |grid, cx| {
        grid.set_view(
            ViewSpec::new().with_filter(ColumnSet::first(1), move |item: &Item| item.id != focused),
            cx,
        )
    });
    fixture.settle(cx);
    assert_eq!(
        fixture.take_events(),
        [Seen::Focus(
            Some((focused + 1, Col::Price)),
            SelectionOrigin::Filtered
        )]
    );
    assert_eq!(fixture.selected(cx), [focused]);

    // In a single mode the selection does not follow the focus.
    let fixture = xsmall(cx, 20, |grid| grid);
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    fixture.click(cx, 5, Modifiers::default());
    writer.remove(5);
    fixture.settle(cx);
    assert_eq!(fixture.focused(cx), Some((6, Col::Price)));
    assert!(fixture.selected(cx).is_empty());
}

#[gpui_kit::test]
fn test_right_click_selects_unselected_first(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 20, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    fixture.click(cx, 1, Modifiers::default());
    fixture.click(cx, 3, Modifiers::shift());
    assert_eq!(fixture.selected(cx), [1, 2, 3]);
    fixture.take_events();
    fixture.right_click(cx, 2);
    assert_eq!(fixture.selected(cx), [1, 2, 3]);
    assert_eq!(fixture.focused(cx), Some((2, Col::Price)));
    assert_eq!(
        fixture.take_events(),
        [Seen::Focus(Some((2, Col::Price)), SelectionOrigin::User)]
    );
    fixture.right_click(cx, 7);
    assert_eq!(fixture.selected(cx), [7]);
    assert_eq!(fixture.focused(cx), Some((7, Col::Price)));
}

#[gpui_kit::test]
fn test_click_target_captured_at_mouse_down(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 30, |grid| {
        grid.with_view(by_price(SortDirection::Descending))
    });
    assert_eq!(fixture.visible_keys(cx)[..3], [29, 28, 27]);
    let position = fixture.cell(2, Col::Price);
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    let reverse = |sign: i64| {
        for id in 0..30u32 {
            writer.upsert(Item {
                id,
                price: sign * i64::from(id) * 10,
            });
        }
    };

    fixture.input(cx, |window, cx| {
        mouse_down_at(
            window,
            position,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    reverse(-1);
    fixture.settle(cx);
    assert_ne!(
        fixture.visible_keys(cx)[2],
        27,
        "another row is under the pointer"
    );
    fixture.input(cx, |window, cx| {
        mouse_up_at(
            window,
            position,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    assert_eq!(fixture.selected(cx), [27]);
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Selection(vec![27], SelectionOrigin::User),
            Seen::Focus(Some((27, Col::Price)), SelectionOrigin::User),
        ],
        "one gesture, one event of each"
    );

    // The order changes again before the second press of the series: the
    // double click confirms the row of the first press.
    reverse(1);
    fixture.settle(cx);
    fixture.input(cx, |window, cx| {
        press_at(
            window,
            position,
            MouseButton::Left,
            Modifiers::default(),
            2,
            cx,
        )
    });
    assert_eq!(fixture.take_events(), [Seen::Confirm(vec![27], false)]);
    assert_eq!(fixture.selected(cx), [27], "the second press edits nothing");
}

#[gpui_kit::test]
fn test_deferred_collapse(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 20, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    fixture.click(cx, 1, Modifiers::default());
    fixture.click(cx, 3, Modifiers::shift());
    fixture.take_events();
    let position = fixture.cell(2, Col::Price);
    fixture.input(cx, |window, cx| {
        mouse_down_at(
            window,
            position,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    assert_eq!(fixture.selected(cx), [1, 2, 3], "not before the release");
    assert_eq!(
        fixture.focused(cx),
        Some((2, Col::Price)),
        "the focus moves now"
    );
    fixture.input(cx, |window, cx| {
        mouse_up_at(
            window,
            position,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    assert_eq!(fixture.selected(cx), [2]);
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Selection(vec![2], SelectionOrigin::User),
            Seen::Focus(Some((2, Col::Price)), SelectionOrigin::User),
        ]
    );
}

#[gpui_kit::test]
fn test_enter_escape_space(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 20, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    fixture.focus(cx);
    // Nothing selected: Enter goes on (a dialog's default button).
    fixture.press(cx, "enter");
    assert!(fixture.take_events().is_empty());
    assert_eq!(fixture.outer_confirms.get(), 1);

    fixture.click(cx, 1, Modifiers::default());
    fixture.click(cx, 2, Modifiers::shift());
    fixture.take_events();
    fixture.press(cx, "enter");
    assert_eq!(fixture.take_events(), [Seen::Confirm(vec![1, 2], false)]);
    fixture.press(cx, "secondary-enter");
    assert_eq!(fixture.take_events(), [Seen::Confirm(vec![1, 2], true)]);
    assert_eq!(fixture.outer_confirms.get(), 1, "handled by the grid");

    // Escape clears the selection, the focus stays; the next one goes on.
    fixture.press(cx, "escape");
    assert!(fixture.selected(cx).is_empty());
    assert_eq!(fixture.focused(cx), Some((2, Col::Price)));
    assert_eq!(fixture.outer_cancels.get(), 0);
    fixture.press(cx, "escape");
    assert_eq!(fixture.outer_cancels.get(), 1);

    // Space toggles the focused row in a set mode.
    fixture.press(cx, "space");
    assert_eq!(fixture.selected(cx), [2]);
    fixture.press(cx, "down");
    assert_eq!(fixture.selected(cx), [3], "navigation selects");
    fixture.press(cx, "space");
    assert!(fixture.selected(cx).is_empty());
    fixture.press(cx, "shift-up space");
    assert_eq!(fixture.selected(cx), [3], "Space toggled the focused row 2");

    // In a single mode Space selects.
    let fixture = xsmall(cx, 20, |grid| grid);
    fixture.focus(cx);
    fixture.press(cx, "down escape space space");
    assert_eq!(fixture.selected(cx), [0]);
}

#[gpui_kit::test]
fn test_focus_visible_after_keyboard_only(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 20, |grid| grid);
    let visible = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_focus_visible())
    };
    fixture.click(cx, 1, Modifiers::default());
    assert!(!visible(cx), "a click focuses without the outline");
    fixture.press(cx, "down");
    assert!(visible(cx));
    let position = fixture.cell(4, Col::Size);
    fixture.input(cx, |window, cx| {
        window.dispatch_event(
            gpui_kit::InputEvent::to_platform_input(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        window.render_frame(cx);
    });
    assert!(!visible(cx), "the pointer hides it again");
}

#[gpui_kit::test]
fn test_drag_select_with_autoscroll(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 100, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    let (from, to) = (fixture.cell(1, Col::Price), fixture.cell(5, Col::Size));
    fixture.input(cx, |window, cx| window.drag(from, to, cx));
    assert_eq!(fixture.selected(cx), [1, 2, 3, 4, 5]);
    assert_eq!(fixture.focused(cx), Some((5, Col::Size)));
    let events = fixture.take_events();
    assert_eq!(
        events,
        [
            Seen::Selection(vec![1, 2, 3, 4, 5], SelectionOrigin::User),
            Seen::Focus(Some((5, Col::Size)), SelectionOrigin::User),
        ]
    );

    // Held below the body, the rows scroll and the range follows.
    let below = point(px(110.), px(26. * 11. + 30.));
    let start = fixture.cell(7, Col::Price);
    let past_threshold = fixture.cell(8, Col::Price);
    fixture.input(cx, |window, cx| {
        mouse_down_at(
            window,
            start,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
        drag_move_to(window, past_threshold, cx);
        drag_move_to(window, below, cx);
    });
    for _ in 0..10 {
        fixture.frame(cx);
    }
    assert!(fixture.visible_keys(cx)[0] > 0, "the body scrolled");
    let last = *fixture.visible_keys(cx).last().expect("rows");
    fixture.input(cx, |window, cx| {
        mouse_up_at(
            window,
            below,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    let selected = fixture.selected(cx);
    assert_eq!(selected.first(), Some(&7));
    assert!(*selected.last().expect("a range") >= last - 1);
    assert!(selected.len() > 10, "past the first screen: {selected:?}");
    assert_eq!(
        fixture
            .take_events()
            .iter()
            .filter(|seen| matches!(seen, Seen::Selection(..)))
            .count(),
        1
    );
}

#[gpui_kit::test]
fn test_cell_modes(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 20, |grid| {
        grid.with_selection_mode(SelectionMode::CellSet)
    });
    fixture.focus(cx);
    fixture.press(cx, "down right");
    assert_eq!(fixture.focused(cx), Some((0, Col::Price)));
    assert_eq!(fixture.columns_of(cx, 0), set(&[Col::Price]));
    fixture.press(cx, "shift-right shift-down");
    assert_eq!(fixture.selected(cx), [0, 1]);
    assert_eq!(fixture.columns_of(cx, 0), set(&[Col::Price, Col::Size]));
    assert_eq!(fixture.columns_of(cx, 1), set(&[Col::Price, Col::Size]));
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_cell_selected(&1, Col::Size)
                && !grid.is_cell_selected(&1, Col::Id))
    );
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Size)
    );

    // Hiding the focused column moves the focus right, after the layout
    // event and with its origin.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_column_visible(Col::Note, false, cx));
    fixture.take_events();
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_column_visible(Col::Size, false, cx));
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Layout(LayoutOrigin::Programmatic),
            Seen::Focus(Some((1, Col::Total)), SelectionOrigin::Programmatic),
        ]
    );
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Total)
    );
    // The last visible column hidden: the focus moves left.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_visible(Col::Total, false, cx)
    });
    assert_eq!(fixture.focused(cx), Some((1, Col::Price)));
    // Cmd/Ctrl+A selects the visible cells.
    fixture.press(cx, "secondary-a");
    assert_eq!(fixture.selected(cx).len(), 20);
    assert_eq!(fixture.columns_of(cx, 7), set(&[Col::Id, Col::Price]));
    // The cell modes paint and select by cell; a mode change to rows clears.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_selection_mode(SelectionMode::RowSet, cx)
    });
    assert!(fixture.selected(cx).is_empty());
}

#[gpui_kit::test]
fn test_scroll_anchor_under_live_reorder(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 100, |grid| {
        grid.with_view(by_price(SortDirection::Descending))
    });
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(40, cx));
    fixture.settle(cx);
    fixture.click(cx, 2, Modifiers::default());
    let focused = fixture.visible_keys(cx)[2];
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    for id in 0..100u32 {
        writer.upsert(Item {
            id,
            price: -i64::from(id) * 10,
        });
    }
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx)[2], focused, "same screen line");
    assert_eq!(fixture.visible_keys(cx)[3], focused + 1, "the new order");

    // At the very top with no focus, new rows on top come into view.
    let fixture = xsmall(cx, 100, |grid| {
        grid.with_view(by_price(SortDirection::Descending))
    });
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    writer.upsert(Item {
        id: 500,
        price: 1_000_000,
    });
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx)[..2], [500, 99]);
    // Scrolled down without a focus, the first painted row keeps its line.
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(10, cx));
    fixture.settle(cx);
    let first = fixture.visible_keys(cx)[0];
    writer.upsert(Item {
        id: 501,
        price: 2_000_000,
    });
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx)[0], first);
}

/// As AG Grid: navigation only moves the focus; Space selects.
struct FocusOnlyNavigation;

impl SelectionGesture for FocusOnlyNavigation {
    fn edit(&self, context: &SelectionGestureContext) -> SelectionEdit {
        match context.input() {
            GestureInput::Keyboard => SelectionEdit::Keep,
            _ => SelectionEdit::Replace,
        }
    }
}

#[gpui_kit::test]
fn test_selection_builder(cx: &mut TestAppContext) {
    let fixture = xsmall(cx, 20, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_selection_scope(StoreScope)
            .with_selection_gesture(FocusOnlyNavigation)
    });
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.selection_mode()),
        SelectionMode::RowSet
    );
    fixture.focus(cx);
    fixture.press(cx, "down down");
    assert!(fixture.selected(cx).is_empty(), "the gesture keeps");
    assert_eq!(fixture.focused(cx), Some((1, Col::Id)));
    fixture.press(cx, "space down down space");
    assert_eq!(fixture.selected(cx), [1, 3]);
    // The store scope keeps a selected row a filter hides, after the shown.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_view(
            ViewSpec::new().with_filter(ColumnSet::first(1), |item: &Item| item.id != 1),
            cx,
        )
    });
    fixture.settle(cx);
    assert_eq!(fixture.scoped(cx), [3, 1]);

    // The programmatic API: `Programmatic` events, keys the store lacks
    // dropped.
    fixture.take_events();
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_selected_keys([4, 5, 999], cx));
    assert_eq!(fixture.selected(cx), [4, 5]);
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(6), cx));
    assert_eq!(fixture.selected(cx), [6]);
    assert_eq!(fixture.focused(cx), Some((6, Col::Id)));
    fixture.grid.update(cx, |grid, cx| grid.clear_selection(cx));
    fixture.grid.update(cx, |grid, cx| grid.select_all(cx));
    assert_eq!(fixture.selected(cx).len(), 19, "the rows on screen");
    assert!(fixture.take_events().iter().all(|seen| matches!(
        seen,
        Seen::Selection(_, SelectionOrigin::Programmatic)
            | Seen::Focus(_, SelectionOrigin::Programmatic)
    )));
}
