//! The trade blotter's workflow end to end on test rows
//! (`.agents/COMPONENT_TEST_RULES.md`): what `examples/blotter` does, in a
//! headless window. The first load and its first row selected after
//! `flush`; pinned rows; selection and copy from the keyboard; the reorder
//! policy switched at run time (`set_reorder_policy`); the keyboard context
//! menu with the primary command, and an application command whose handler
//! reads the command scope when it runs; Copy all; a reload under the live
//! stream with progress; the accessibility tree of the last frame.
//!
//! Assertions read the grid, its events, the accessibility rows and menu
//! items (their roles, labels and bounds) and the clipboard of the test
//! platform, not pixels. The test window keeps accessibility off, so the
//! tree is checked through `set_a11y_forced`, as in `tests/ui.rs`.
//!
//! The grouped blotter: two levels with aggregates and the totals row
//! under `ManualReorder` and a stream of prices: a chevron, `+`, `-` and
//! Enter on a caption, Copy all and Copy all expanded, the held order
//! applied; and the grouping and collapsed groups through a saved layout.
//! The grouping tests run on 60 trades of `GroupedRow`. The blotter's
//! `BLOTTER_FOOTERS=1`: group footers under expanded groups, stepped over
//! by the arrows, a press on one, and Copy all expanded with them.

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

mod common;

use std::{
    cell::{Cell, RefCell},
    hash::{Hash, Hasher as _},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::component::{Sizable as _, menu::PopupMenu};
use gpui_kit::test::{ElementSnapshot, TestSupportExt as _, TestWindowExt as _};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, ElementId, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, Modifiers, MouseButton, ParentElement as _, Pixels,
    Point, Render, Role, SharedString, Styled as _, TestAppContext, Window, WindowBounds,
    WindowOptions, div, point, px, size,
};
use gpui_kit_grid::{
    Aggregate, EveryGroupFooter, ExpandDepth, ExpandTarget, ExportScope, FlushOutcome, GridColumn,
    GridColumns as _, GridMenuContext, GridProgress, GridSchema, GridTarget, GridWriter, GroupBy,
    GroupOrigin, GroupPath, HoldReorder, JsonLayoutCodec, LayoutCodec as _, LiveGrid,
    LiveGridDelegate, LiveGridEvent, LiveGridState, LiveReorder, ManualReorder, OwnedValue,
    PinnedEdge, SelectionMode, SortDirection, StoreOptions, ViewSpec, actions::CopyAll,
};

use crate::common::{
    GroupSeen, GroupedColumn, GroupedFixture, GroupedRow, Item, ItemColumn, LineKind,
    PausableRunner, ROW, by_price, caption_point, group_path, grouped_cell, grouped_rows,
    open_grouped, press_at, press_chevron, unthrottled,
};

gpui_kit::actions!(
    blotter_flow,
    [
        /// The application command of the menu, as the blotter's
        /// `CancelTrades`.
        CancelItems,
    ]
);

/// A header, a top band, ten data rows and a bottom band.
const LINES: f32 = 13.;

/// "Open item", "Open 3 items".
fn items_label(verb: &str, count: usize) -> String {
    match count {
        1 => format!("{verb} item"),
        count => format!("{verb} {count} items"),
    }
}

/// The blotter's delegate on test rows: a primary command named after the
/// command scope, and an application command above the standard items of
/// a cell's menu.
struct Blotter {
    /// The primary command labels returned.
    primaries: Rc<RefCell<Vec<String>>>,
}

impl LiveGridDelegate for Blotter {
    type Row = Item;

    fn primary_command(&self, menu: &GridMenuContext<'_, Item>, _: &App) -> Option<SharedString> {
        let label = items_label("Open", menu.selection().len());
        self.primaries.borrow_mut().push(label.clone());
        Some(label.into())
    }

    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, Item>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let menu = match menu_context.target() {
            GridTarget::Cell { .. } => {
                let count = menu_context.selection().len();
                menu.menu_with_disabled(
                    items_label("Cancel", count),
                    Box::new(CancelItems),
                    count == 0,
                )
                .separator()
            }
            _ => menu,
        };
        menu_context.standard_items(menu, window, cx)
    }
}

/// The window content: the grid, and the handler of the application
/// command, which reads the command scope when it runs and removes those
/// rows, as the blotter's `CancelTrades` does.
struct Root {
    grid: Entity<LiveGridState<Blotter>>,
    cancelled: Rc<RefCell<Vec<Vec<u32>>>>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .on_action(cx.listener(|root, _: &CancelItems, _, cx| {
                let grid = root.grid.read(cx);
                let keys = grid.scoped_selection().keys().to_vec();
                let writer = grid.writer();
                for key in &keys {
                    writer.remove(*key);
                }
                root.cancelled.borrow_mut().push(keys);
            }))
            .child(
                LiveGrid::new(&self.grid)
                    .xsmall()
                    .bordered(false)
                    .accessibility_label("Items"),
            )
            // A spot over the header, outside the body.
            .child(
                div()
                    .id("away")
                    .test_support()
                    .absolute()
                    .top_0()
                    .right_0()
                    .size(px(8.)),
            )
    }
}

struct Fixture {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<Blotter>>,
    events: Rc<RefCell<Vec<LiveGridEvent<u32, ItemColumn>>>>,
    primaries: Rc<RefCell<Vec<String>>>,
    cancelled: Rc<RefCell<Vec<Vec<u32>>>>,
    runner: PausableRunner,
}

/// An empty `RowSet` grid sorted by price, highest first, configured by
/// `configure`; jobs run inline until the runner is paused.
fn open(
    cx: &mut TestAppContext,
    configure: impl FnOnce(LiveGridState<Blotter>) -> LiveGridState<Blotter> + 'static,
) -> Fixture {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let runner = PausableRunner::default();
    let primaries = Rc::new(RefCell::new(Vec::new()));
    let cancelled = Rc::new(RefCell::new(Vec::new()));
    let (window, root) = cx
        .update(|cx| {
            let (runner, primaries, cancelled) =
                (runner.clone(), primaries.clone(), cancelled.clone());
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(500.), px(ROW * LINES)),
                    })),
                    ..Default::default()
                },
                cx,
                move |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            let schema = GridSchema::new()
                                .column(GridColumn::new(ItemColumn::Id, "Id"))
                                .column(GridColumn::new(ItemColumn::Price, "Price"));
                            configure(
                                LiveGridState::new_standalone(
                                    schema,
                                    Blotter { primaries },
                                    StoreOptions::default(),
                                    cx,
                                )
                                .with_selection_mode(SelectionMode::RowSet)
                                .with_view(by_price(SortDirection::Descending))
                                .with_tuning(unthrottled())
                                .with_task_runner(runner),
                            )
                        }),
                        cancelled,
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(
            &grid,
            move |_, event: &LiveGridEvent<u32, ItemColumn>, _| {
                events.borrow_mut().push(event.clone());
            },
        )
        .detach();
    });
    let fixture = Fixture {
        window,
        grid,
        events,
        primaries,
        cancelled,
        runner,
    };
    fixture.settle(cx);
    fixture
}

/// Items `0..count` with `price = id * 10`.
fn items(count: u32) -> impl Iterator<Item = Arc<Item>> {
    (0..count).map(|id| {
        Arc::new(Item {
            id,
            price: i64::from(id) * 10,
        })
    })
}

/// The middle of the Id cell on window line `line` (0 is the header).
fn line(line: usize) -> Point<Pixels> {
    point(px(30.), px(ROW * line as f32 + ROW / 2.))
}

/// `NamedInteger(name, hash of value)`, hashed with `FxHasher` as the grid
/// hashes row keys for element ids.
fn hashed_id(name: &'static str, value: impl Hash) -> ElementId {
    let mut hasher = rustc_hash::FxHasher::default();
    value.hash(&mut hasher);
    ElementId::NamedInteger(name.into(), hasher.finish())
}

/// The element id of a data row's accessibility row, as in `tests/ui.rs`.
fn a11y_row_id(key: u32) -> ElementId {
    hashed_id("live-grid-row", key)
}

/// The element id of a pinned row's accessibility row: named after its
/// band, which is hashed with the key, so it never takes a data row's id.
fn a11y_pinned_row_id(edge: PinnedEdge, key: u32) -> ElementId {
    let band = match edge {
        PinnedEdge::Top => "live-grid-pinned-top",
        PinnedEdge::Bottom => "live-grid-pinned-bottom",
    };
    hashed_id(band, (band, key))
}

impl Fixture {
    /// Runs ingest, pulls and background jobs, then draws a frame.
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx, Duration::from_millis(20));
        }
    }

    fn frame(&self, cx: &mut TestAppContext, elapsed: Duration) {
        cx.executor().advance_clock(elapsed);
        cx.run_until_parked();
        self.input(cx, |window, cx| window.render_frame(cx));
    }

    fn input<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("window is open")
    }

    fn writer(&self, cx: &mut TestAppContext) -> GridWriter<Item> {
        self.grid.read_with(cx, |grid, _| grid.writer())
    }

    fn focus(&self, cx: &mut TestAppContext) {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.render_frame(cx);
        });
    }

    /// Presses keys one by one, each in its own update, and settles.
    fn press(&self, cx: &mut TestAppContext, keys: &str) {
        for key in keys.split(' ') {
            self.input(cx, |window, cx| window.press(key, cx));
            self.settle(cx);
        }
    }

    fn right_click(&self, cx: &mut TestAppContext, position: Point<Pixels>) {
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
        self.settle(cx);
    }

    /// Focuses the grid and dispatches `action` to it, as a menu item does.
    fn dispatch(&self, cx: &mut TestAppContext, action: &dyn gpui_kit::Action) {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.dispatch_action(action.boxed_clone(), cx);
        });
        self.settle(cx);
    }

    /// As the blotter after a load: once the order on screen reflects the
    /// writes so far (`flush`), select the first row shown unless some of
    /// the selection survived. The cell has the outcome once resolved.
    fn select_first_after_flush(&self, cx: &mut TestAppContext) -> Rc<Cell<Option<FlushOutcome>>> {
        let flushed = self.grid.update(cx, |grid, cx| grid.flush(cx));
        let grid = self.grid.clone();
        let done = Rc::new(Cell::new(None));
        let resolved = done.clone();
        cx.update(|cx| {
            cx.spawn(async move |cx| {
                let Some(outcome) = flushed.await else {
                    return;
                };
                let _ = grid.update(cx, |grid, cx| {
                    if grid.selection().is_empty() {
                        let first = grid
                            .projection()
                            .row(0)
                            .and_then(|id| grid.snapshot().row(id))
                            .map(|row| row.id);
                        grid.select_key(first, cx);
                    }
                });
                resolved.set(Some(outcome));
            })
            .detach();
        });
        done
    }

    fn visible_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| grid.visible_keys())
    }

    fn selected_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| {
            let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
            keys.sort_unstable();
            keys
        })
    }

    fn focused_key(&self, cx: &mut TestAppContext) -> Option<u32> {
        self.grid.read_with(cx, |grid, _| {
            grid.focused_cell().and_then(|cell| cell.row().copied())
        })
    }

    fn pinned_keys(&self, cx: &mut TestAppContext) -> (Vec<u32>, Vec<u32>) {
        self.grid.read_with(cx, |grid, _| {
            (
                grid.pinned_keys(PinnedEdge::Top),
                grid.pinned_keys(PinnedEdge::Bottom),
            )
        })
    }

    fn progress(&self, cx: &mut TestAppContext) -> Option<GridProgress> {
        self.grid.read_with(cx, |grid, _| grid.progress().cloned())
    }

    fn is_order_held(&self, cx: &mut TestAppContext) -> bool {
        self.grid.read_with(cx, |grid, _| grid.is_order_held())
    }

    fn is_menu_open(&self, cx: &mut TestAppContext) -> bool {
        self.grid.read_with(cx, |grid, _| grid.is_menu_open())
    }

    /// Whether the window shows an element with this id.
    fn shows(&self, cx: &mut TestAppContext, id: &'static str) -> bool {
        self.input(cx, |window, _| window.try_find(id).is_some())
    }

    /// Writes `price = sign * id * 10` to rows `0..count`: flipping the sign
    /// reverses the price order.
    fn write_prices(&self, cx: &mut TestAppContext, count: u32, sign: i64) {
        let writer = self.writer(cx);
        for id in 0..count {
            writer.upsert(Item {
                id,
                price: sign * i64::from(id) * 10,
            });
        }
    }

    /// `held` of every `OrderHeldChange` event so far.
    fn held_events(&self) -> Vec<bool> {
        self.events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::OrderHeldChange { held } => Some(*held),
                _ => None,
            })
            .collect()
    }

    fn confirms(&self) -> Vec<(Vec<u32>, bool)> {
        self.events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::Confirm { keys, secondary } => Some((keys.to_vec(), *secondary)),
                _ => None,
            })
            .collect()
    }

    /// The row counts of the `ProjectionChange` events so far.
    fn orders_shown(&self) -> Vec<usize> {
        self.events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::ProjectionChange { row_count, .. } => Some(*row_count),
                _ => None,
            })
            .collect()
    }

    /// The observed accessibility rows of the last frame, header included,
    /// top to bottom: the last part of their element ids, their selection
    /// and their top edge.
    fn a11y_rows(&self, cx: &mut TestAppContext) -> Vec<(ElementId, Option<bool>, Pixels)> {
        self.input(cx, |window, _| {
            let mut rows: Vec<(ElementId, Option<bool>, Pixels)> =
                gpui_kit::base::test_support::snapshots(window)
                    .into_iter()
                    .filter(|element| element.role() == Some(Role::Row))
                    .map(|element| {
                        (
                            element.path().last().cloned().expect("an id"),
                            element.selected(),
                            element.bounds().origin.y,
                        )
                    })
                    .collect();
            rows.sort_by(|a, b| a.2.partial_cmp(&b.2).expect("finite"));
            rows
        })
    }

    /// The items of the open menus, top to bottom.
    fn menu_items(&self, cx: &mut TestAppContext) -> Vec<ElementSnapshot> {
        self.input(cx, |window, _| {
            let mut items: Vec<ElementSnapshot> = gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .filter(|item| item.role() == Some(Role::MenuItem) && item.visible())
                .collect();
            items.sort_by(|a, b| {
                (a.path().len(), a.bounds().origin.y)
                    .partial_cmp(&(b.path().len(), b.bounds().origin.y))
                    .expect("finite")
            });
            items
        })
    }

    /// The labels of the top-level menu's items (the primary command is a
    /// custom element without one).
    fn menu_labels(&self, cx: &mut TestAppContext) -> Vec<String> {
        let submenu = ElementId::Name("submenu".into());
        self.menu_items(cx)
            .iter()
            .filter(|item| !item.path().contains(&submenu))
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Clicks the menu item labelled `label`.
    fn click_item(&self, cx: &mut TestAppContext, label: &str) {
        let item = self
            .menu_items(cx)
            .into_iter()
            .find(|item| item.label() == Some(label))
            .unwrap_or_else(|| panic!("no menu item {label:?}"));
        self.input(cx, |window, cx| {
            press_at(
                window,
                item.bounds().center(),
                MouseButton::Left,
                Modifiers::default(),
                1,
                cx,
            )
        });
        self.settle(cx);
    }
}

fn clipboard_lines(cx: &mut TestAppContext) -> Vec<String> {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .expect("copied")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[gpui_kit::test]
fn test_trade_blotter_workflow(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    let writer = fixture.writer(cx);

    // The first load, with progress over the empty grid; once the rows are
    // on screen the first one is selected and focused.
    let loading = GridProgress::indeterminate().with_label("Loading items");
    writer.set_progress(Some(loading.clone()));
    writer.replace_all(items(30));
    writer.set_progress(None);
    let loaded = fixture.select_first_after_flush(cx);
    fixture.settle(cx);
    let outcome = loaded.get().expect("the first load resolved");
    assert_eq!(outcome.row_count(), 30);
    assert_eq!(fixture.selected_keys(cx), [29]);
    assert_eq!(fixture.focused_key(cx), Some(29));
    assert_eq!(fixture.progress(cx), None);
    assert!(!fixture.shows(cx, "live-grid-progress"));

    // Index above the rows and Total below them: ten data rows remain.
    writer.set_pinned_rows(
        PinnedEdge::Top,
        [Arc::new(Item {
            id: 1000,
            price: 145,
        })],
    );
    writer.set_pinned_rows(
        PinnedEdge::Bottom,
        [Arc::new(Item {
            id: 2000,
            price: 4350,
        })],
    );
    fixture.settle(cx);
    assert_eq!(fixture.pinned_keys(cx), (vec![1000], vec![2000]));
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());

    // Keyboard: Shift+Down twice extends the selection, Cmd/Ctrl+C copies
    // it in the shown order.
    fixture.focus(cx);
    fixture.press(cx, "shift-down shift-down");
    assert_eq!(fixture.selected_keys(cx), [27, 28, 29]);
    assert_eq!(fixture.focused_key(cx), Some(27));
    fixture.press(cx, "secondary-c");
    assert_eq!(clipboard_lines(cx), ["29\t290", "28\t280", "27\t270"]);

    // `ManualReorder` set at run time; the stream reverses the order: held.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_reorder_policy(ManualReorder, cx));
    assert!(fixture.held_events().is_empty(), "nothing was held");
    fixture.write_prices(cx, 30, -1);
    fixture.settle(cx);
    assert!(fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true]);
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());

    // Shift+F10: the menu at the focused cell, the primary command first,
    // named after the scope; Enter on it confirms the scope in the shown
    // order. The order stays held.
    fixture.press(cx, "shift-f10");
    assert!(fixture.is_menu_open(cx));
    assert_eq!(
        fixture.primaries.borrow().last().cloned(),
        Some("Open 3 items".into())
    );
    assert_eq!(
        fixture.menu_items(cx)[0].label(),
        None,
        "the primary command"
    );
    assert_eq!(
        fixture.menu_labels(cx),
        ["Cancel 3 items", "Copy", "Copy all", "Sort", "Column"]
    );
    fixture.press(cx, "down enter");
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(fixture.confirms(), [(vec![29, 28, 27], false)]);
    assert!(fixture.is_order_held(cx));

    // The application command: a menu on key 28 (window line 3, under the
    // header and the top band), then row 27 leaves the store while the
    // menu is open. The handler reads the scope when it runs: without 27.
    fixture.right_click(cx, line(3));
    assert!(fixture.is_menu_open(cx));
    assert_eq!(fixture.selected_keys(cx), [27, 28, 29], "kept by the press");
    writer.remove(27);
    fixture.settle(cx);
    assert!(fixture.is_menu_open(cx), "another row of the scope left");
    fixture.click_item(cx, "Cancel 3 items");
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(*fixture.cancelled.borrow(), [vec![29, 28]]);
    fixture.settle(cx);
    assert!(fixture.selected_keys(cx).is_empty());
    // Removals compact the shown order; the newer order stays held.
    assert_eq!(fixture.visible_keys(cx), (17..27).rev().collect::<Vec<_>>());
    assert!(fixture.is_order_held(cx));

    // Copy all: the header line, the top band, the rows as on screen with
    // the values of the shown snapshot, the bottom band.
    fixture.dispatch(cx, &CopyAll);
    let expected: Vec<String> = ["Id\tPrice".to_owned(), "1000\t145".to_owned()]
        .into_iter()
        .chain(
            (0..27)
                .rev()
                .map(|id| format!("{id}\t{}", -i64::from(id) * 10)),
        )
        .chain(["2000\t4350".to_owned()])
        .collect();
    assert_eq!(clipboard_lines(cx), expected);

    // A reload under the live stream: the progress shows while the writes
    // are buffered (the shown prices stay), and stays until the new data
    // set is shown, even after the producer cleared it.
    let reloading = GridProgress::indeterminate().with_label("Loading snapshot");
    writer.set_progress(Some(reloading.clone()));
    let reload = writer.begin_reload();
    writer.upsert(Item { id: 5, price: 555 });
    fixture.settle(cx);
    assert_eq!(fixture.progress(cx), Some(reloading.clone()));
    assert!(fixture.shows(cx, "live-grid-progress"));
    assert_eq!(
        writer.snapshot().get(&5).map(|item| item.price),
        Some(-50),
        "buffered"
    );
    fixture.events.borrow_mut().clear();
    fixture.runner.pause();
    reload.finish(items(20));
    writer.set_progress(None);
    let reloaded = fixture.select_first_after_flush(cx);
    fixture.settle(cx);
    assert_eq!(fixture.progress(cx), Some(reloading));
    assert!(fixture.orders_shown().is_empty());
    assert!(reloaded.get().is_none());
    fixture.runner.resume();
    fixture.settle(cx);
    // One order for the new set, with the progress leaving in its frame;
    // the flush resolved after it. A new data set shows under
    // `ManualReorder` and drops the held order. The live write wins over
    // the older snapshot (`ReplayLive`).
    assert_eq!(fixture.orders_shown(), [20]);
    assert_eq!(fixture.progress(cx), None);
    assert!(!fixture.shows(cx, "live-grid-progress"));
    let outcome = reloaded.get().expect("the reload resolved");
    assert_eq!(outcome.row_count(), 20);
    assert!(!outcome.is_order_held());
    assert_eq!(fixture.held_events(), [false]);
    assert_eq!(writer.snapshot().get(&5).map(|item| item.price), Some(555));
    assert_eq!(
        fixture.visible_keys(cx),
        [5, 19, 18, 17, 16, 15, 14, 13, 12, 11]
    );
    // The selection was empty, so the first row is selected.
    assert_eq!(fixture.selected_keys(cx), [5]);
    assert_eq!(fixture.pinned_keys(cx), (vec![1000], vec![2000]));

    // Held again under `ManualReorder`; switching to `LiveReorder` shows it.
    fixture.write_prices(cx, 20, -1);
    fixture.settle(cx);
    assert!(fixture.is_order_held(cx));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_reorder_policy(LiveReorder, cx));
    assert_eq!(fixture.held_events(), [false, true, false]);
    fixture.settle(cx);
    // The focused row 5 keeps its screen line (the scroll anchor); the
    // selection is by key.
    assert_eq!(fixture.visible_keys(cx), (5..15).collect::<Vec<_>>());
    assert_eq!(fixture.selected_keys(cx), [5]);
    assert_eq!(fixture.focused_key(cx), Some(5));

    // A screen reader's view of the same frame (forced on: the test window
    // keeps accessibility off). One row per painted line, at its line: the
    // header, the top band, the ten data rows in the shown order and the
    // bottom band. Only data rows carry the selection: row 5 is marked.
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    fixture.settle(cx);
    let planned = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().a11y_rows());
    assert_eq!(planned, 12, "the two bands and the ten data rows");
    let expected: Vec<(ElementId, Option<bool>, Pixels)> = [
        (ElementId::from("live-grid-header"), None),
        (a11y_pinned_row_id(PinnedEdge::Top, 1000), None),
    ]
    .into_iter()
    .chain(
        fixture
            .visible_keys(cx)
            .into_iter()
            .map(|key| (a11y_row_id(key), Some(key == 5))),
    )
    .chain([(a11y_pinned_row_id(PinnedEdge::Bottom, 2000), None)])
    .enumerate()
    .map(|(line, (id, selected))| (id, selected, px(ROW * line as f32)))
    .collect();
    assert_eq!(expected.len(), LINES as usize);
    assert_eq!(fixture.a11y_rows(cx), expected);

    // The grid is named as `LiveGrid::accessibility_label` sets it.
    let root = fixture.input(cx, |window, _| window.find("live-grid"));
    assert_eq!(root.role(), Some(Role::Grid));
    assert_eq!(root.label(), Some("Items"));
}

#[gpui_kit::test]
fn test_set_reorder_policy_reoffers_held_order(cx: &mut TestAppContext) {
    const HOLD: Duration = Duration::from_millis(500);
    let fixture = open(cx, |grid| grid.with_reorder_policy(ManualReorder));
    fixture.writer(cx).replace_all(items(30));
    fixture.settle(cx);
    let top: Vec<u32> = (18..30).rev().collect();
    assert_eq!(fixture.visible_keys(cx), top);

    // Held by `ManualReorder`; `LiveReorder` shows it at once.
    fixture.write_prices(cx, 30, -1);
    fixture.settle(cx);
    assert_eq!(fixture.held_events(), [true]);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_reorder_policy(LiveReorder, cx));
    assert_eq!(fixture.held_events(), [true, false], "applied in the call");
    assert!(!fixture.is_order_held(cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..12).collect::<Vec<_>>());

    // No order held: a new policy changes nothing else.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_reorder_policy(HoldReorder::new(HOLD), cx)
    });
    assert_eq!(fixture.held_events(), [true, false]);

    // `HoldReorder` holds new data under the pointer.
    fixture.input(cx, |window, cx| window.hover("live-grid", cx));
    fixture.write_prices(cx, 30, 1);
    fixture.settle(cx);
    assert!(fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true, false, true]);
    assert_eq!(fixture.visible_keys(cx), (0..12).collect::<Vec<_>>());

    // `ManualReorder` takes the held order over: `HoldReorder`'s deadline
    // no longer applies.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_reorder_policy(ManualReorder, cx));
    fixture.frame(cx, HOLD * 2);
    fixture.settle(cx);
    assert!(fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true, false, true]);

    // With the context menu open, `LiveReorder` keeps holding; closing the
    // menu shows the order.
    fixture.right_click(cx, line(2));
    assert!(fixture.is_menu_open(cx));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_reorder_policy(LiveReorder, cx));
    assert!(fixture.is_order_held(cx));
    fixture.press(cx, "escape");
    assert!(!fixture.is_menu_open(cx));
    assert!(!fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true, false, true, false]);
    // The new order is shown with the focused row 1 (the menu's) on screen,
    // as near its line as the end of the rows allows.
    assert_eq!(fixture.focused_key(cx), Some(1));
    assert_eq!(fixture.visible_keys(cx), (0..12).rev().collect::<Vec<_>>());
}

// The grouped blotter.

/// The blotter's grouped view on test trades: by instrument, then side,
/// the highest price first within a group; `Sum` of the quantity and `Avg`
/// of the price on the captions and the totals row at the bottom, as the
/// blotter's Volume and Last.
fn grouped_view() -> ViewSpec<GroupedRow> {
    ViewSpec::new()
        .with_sort([(GroupedColumn::Price, SortDirection::Descending)])
        .with_group_by([
            GroupBy::column(GroupedColumn::Instrument),
            GroupBy::column(GroupedColumn::Side),
        ])
        .with_aggregates([
            (GroupedColumn::Qty, Aggregate::Sum),
            (GroupedColumn::Price, Aggregate::Avg),
        ])
}

fn aapl() -> GroupPath {
    group_path(&[(GroupedColumn::Instrument, "AAPL")])
}

fn aapl_side(side: &str) -> GroupPath {
    group_path(&[
        (GroupedColumn::Instrument, "AAPL"),
        (GroupedColumn::Side, side),
    ])
}

/// Presses `key` with the grid focused, then settles.
fn type_key(fixture: &GroupedFixture, cx: &mut TestAppContext, key: &str) {
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
        window.press(key, cx);
    });
    fixture.settle(cx);
}

/// The trades of `rows` that `keep` takes, by id.
fn ids(rows: &[GroupedRow], keep: impl Fn(&GroupedRow) -> bool) -> Vec<u32> {
    rows.iter()
        .filter(|row| keep(row))
        .map(|row| row.id)
        .collect()
}

/// The totals of the projection on screen: the quantity and the price.
fn totals(fixture: &GroupedFixture, cx: &mut TestAppContext) -> (OwnedValue, OwnedValue) {
    fixture.grid.read_with(cx, |grid, _| {
        let totals = grid.projection().totals();
        let value = |column: GroupedColumn| {
            totals
                .get(column.ix())
                .cloned()
                .expect("an aggregate column")
        };
        (value(GroupedColumn::Qty), value(GroupedColumn::Price))
    })
}

/// The sum of the quantities and the average price of the store's trades.
fn store_sums(fixture: &GroupedFixture, cx: &mut TestAppContext) -> (i64, f64) {
    fixture.grid.read_with(cx, |grid, _| {
        let snapshot = grid.snapshot();
        let (qty, price) = snapshot.iter().fold((0, 0.), |(qty, price), (_, row)| {
            (qty + row.qty, price + row.price)
        });
        (qty, price / snapshot.len() as f64)
    })
}

/// The sums of the store match the totals row: exact for the quantity,
/// to rounding for the average price.
fn assert_totals_match_rows(fixture: &GroupedFixture, cx: &mut TestAppContext) {
    let (qty, price) = store_sums(fixture, cx);
    let (total_qty, total_price) = totals(fixture, cx);
    assert_eq!(total_qty, OwnedValue::Int(qty));
    let OwnedValue::Float(total_price) = total_price else {
        panic!("Avg of a float column is a float: {total_price:?}");
    };
    assert!(
        (total_price - price).abs() < 1e-9,
        "average price {total_price}, rows {price}"
    );
}

fn clipboard_text(cx: &mut TestAppContext) -> String {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .expect("copied")
}

/// The TSV line whose cell `column` is `value`, split into cells.
fn tsv_line<'a>(text: &'a str, column: GroupedColumn, value: &str) -> Option<Vec<&'a str>> {
    text.lines()
        .map(|line| line.split('\t').collect::<Vec<_>>())
        .find(|cells| cells.get(column.ix().get()) == Some(&value))
}

#[gpui_kit::test]
fn test_blotter_grouped_flow(cx: &mut TestAppContext) {
    let rows = grouped_rows(60);
    let fixture = open_grouped(cx, rows.clone(), |grid| {
        grid.with_view(grouped_view())
            .with_totals(PinnedEdge::Bottom)
            .with_selection_mode(SelectionMode::RowSet)
            .with_reorder_policy(ManualReorder)
            .with_tuning(unthrottled())
    });
    // AAPL holds every fourth trade: Buy 0, 8, …, 56 and Sell 4, 12, …, 52.
    let aapl_buy = ids(&rows, |row| {
        row.instrument == "AAPL" && row.side.as_str() == "Buy"
    });
    let aapl_sell = ids(&rows, |row| {
        row.instrument == "AAPL" && row.side.as_str() == "Sell"
    });
    assert_eq!((aapl_buy.len(), aapl_sell.len()), (8, 7));
    let lines = fixture.lines(cx);
    assert_eq!(
        lines[..2],
        [
            LineKind::Caption(aapl()),
            LineKind::Caption(aapl_side("Buy"))
        ]
    );
    assert_eq!(
        lines[2 + aapl_buy.len()],
        LineKind::Caption(aapl_side("Sell"))
    );
    assert_totals_match_rows(&fixture, cx);
    fixture.take_seen();

    // The stream lowers every AAPL price: a new order inside the groups,
    // held by `ManualReorder`. The aggregates follow the values at once.
    let writer = fixture.writer(cx);
    for row in rows.iter().filter(|row| row.instrument == "AAPL") {
        writer.upsert(GroupedRow {
            price: row.price - 50.0 - f64::from(row.id),
            ..row.clone()
        });
    }
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    assert_eq!(fixture.take_seen(), [GroupSeen::Held(true)]);
    assert_totals_match_rows(&fixture, cx);
    let held_lines = fixture.lines(cx);

    // A chevron collapses AAPL › Buy at once, with the order still held.
    fixture.input(cx, |window, cx| {
        press_chevron(window, 1, 1, Modifiers::default(), cx)
    });
    fixture.settle(cx);
    assert_eq!(
        fixture.take_seen()[..1],
        [GroupSeen::Expansion(
            Some(ExpandTarget::Group(aapl_side("Buy"))),
            GroupOrigin::User
        )]
    );
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    let collapsed = fixture.lines(cx);
    assert_eq!(collapsed[2], LineKind::Caption(aapl_side("Sell")));
    assert_eq!(collapsed.len(), held_lines.len() - aapl_buy.len());

    // A press on its caption focuses it, not the selection; `+` expands it
    // and `-` collapses it again.
    let selected = fixture.grid.read_with(cx, |grid, _| grid.selection().len());
    fixture.input(cx, |window, cx| {
        press_at(
            window,
            caption_point(1),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture.settle(cx);
    let focused = |fixture: &GroupedFixture, cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            grid.focused_cell().and_then(|cell| cell.group().cloned())
        })
    };
    assert_eq!(focused(&fixture, cx), Some(aapl_side("Buy")));
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.selection().len()),
        selected
    );
    type_key(&fixture, cx, "shift-=->+");
    assert_eq!(fixture.lines(cx).len(), held_lines.len());
    type_key(&fixture, cx, "-");
    assert_eq!(fixture.lines(cx), collapsed);
    fixture.take_seen();

    // Enter on the caption: the primary command over the group's trades,
    // hidden ones too, as many as its caption counts.
    fixture.events.borrow_mut().clear();
    type_key(&fixture, cx, "enter");
    let confirmed: Vec<Vec<u32>> = fixture
        .events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::Confirm { keys, .. } => Some(keys.to_vec()),
            _ => None,
        })
        .collect();
    let caption_rows = fixture.grid.read_with(cx, |grid, _| {
        grid.projection()
            .groups()
            .find(&aapl_side("Buy"))
            .map(|group| group.row_count())
    });
    assert_eq!(confirmed.len(), 1);
    let mut keys = confirmed[0].clone();
    keys.sort_unstable();
    assert_eq!(keys, aapl_buy);
    assert_eq!(caption_rows, Some(keys.len()));

    // Copy all: the header, captions with their key value and aggregates
    // under their columns, the collapsed group as its caption alone, the
    // totals last with the label under Id.
    let (qty, _) = store_sums(&fixture, cx);
    let aapl_qty: i64 = rows
        .iter()
        .filter(|row| row.instrument == "AAPL")
        .map(|row| row.qty)
        .sum();
    fixture.input(cx, |window, cx| {
        window.dispatch_action(Box::new(CopyAll), cx)
    });
    fixture.settle(cx);
    let all = clipboard_text(cx);
    let caption = tsv_line(&all, GroupedColumn::Instrument, "AAPL").expect("the AAPL caption");
    assert_eq!(caption[GroupedColumn::Qty.ix().get()], aapl_qty.to_string());
    let last: Vec<&str> = all.lines().last().expect("lines").split('\t').collect();
    assert_eq!(last[GroupedColumn::Id.ix().get()], "Total");
    assert_eq!(last[GroupedColumn::Qty.ix().get()], qty.to_string());
    let shown_trades = all.lines().count()
        - 2
        - fixture
            .lines(cx)
            .iter()
            .filter(|line| !matches!(line, LineKind::Data(_)))
            .count();
    assert_eq!(shown_trades, rows.len() - aapl_buy.len());

    // Copy all expanded (the blotter's Groups menu): the collapsed group's
    // trades too, under its caption.
    assert!(
        fixture
            .grid
            .update(cx, |grid, cx| grid.copy_scope(ExportScope::Expanded, cx))
    );
    fixture.settle(cx);
    let expanded = clipboard_text(cx);
    assert_eq!(
        expanded.lines().count(),
        all.lines().count() + aapl_buy.len()
    );
    assert_eq!(expanded.lines().last(), all.lines().last());

    // Refresh order: the held order is shown with the expansion of now,
    // and the focus stays on the caption.
    fixture
        .grid
        .update(cx, |grid, cx| grid.apply_held_order(cx));
    fixture.settle(cx);
    assert!(!fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    let shown = fixture.lines(cx);
    assert_eq!(shown[..3], collapsed[..3]);
    assert_ne!(shown, collapsed, "the new order inside AAPL › Sell");
    assert_eq!(focused(&fixture, cx), Some(aapl_side("Buy")));
    assert!(!fixture.grid.read_with(cx, |grid, _| {
        grid.is_expanded(&ExpandTarget::Group(aapl_side("Buy")))
    }));
    assert_totals_match_rows(&fixture, cx);
}

#[gpui_kit::test]
fn test_blotter_layout_with_groups(cx: &mut TestAppContext) {
    // The blotter's `BLOTTER_LAYOUT`: grouped, a group collapsed and the
    // groups of a level descending, written as JSON.
    let fixture = open_grouped(cx, grouped_rows(60), |grid| {
        grid.with_view(grouped_view())
            .with_view_id("quotes")
            .with_tuning(unthrottled())
    });
    let msft = group_path(&[(GroupedColumn::Instrument, "MSFT")]);
    fixture.grid.update(cx, |grid, cx| {
        grid.set_group_by(
            [
                GroupBy::column(GroupedColumn::Instrument)
                    .with_direction(SortDirection::Descending),
                GroupBy::column(GroupedColumn::Side),
            ],
            cx,
        );
        grid.set_expanded(ExpandTarget::Group(msft.clone()), false, cx);
        grid.set_expanded(ExpandTarget::Group(aapl_side("Sell")), false, cx);
    });
    fixture.settle(cx);
    let codec = JsonLayoutCodec::default().pretty(true);
    let text = fixture
        .grid
        .read_with(cx, |grid, _| codec.encode(&grid.layout()))
        .expect("encoded");
    let lines = fixture.lines(cx);

    // A restart: the blotter builds its grid ungrouped, as without
    // `BLOTTER_GROUP`, and passes the saved layout last.
    let saved = codec.decode(&text).expect("decoded");
    let restarted = open_grouped(cx, grouped_rows(60), move |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(GroupedColumn::Price, SortDirection::Descending)])
                .with_aggregates([(GroupedColumn::Qty, Aggregate::Sum)]),
        )
        .with_view_id("quotes")
        .with_tuning(unthrottled())
        .with_layout(saved)
    });
    restarted.grid.read_with(cx, |grid, _| {
        let levels: Vec<(String, SortDirection)> = grid
            .group_by()
            .iter()
            .map(|level| (level.id().as_str().to_owned(), level.direction()))
            .collect();
        assert_eq!(
            levels,
            [
                ("instrument".to_owned(), SortDirection::Descending),
                ("side".to_owned(), SortDirection::Ascending),
            ]
        );
        assert_eq!(grid.expand_depth(), ExpandDepth::All);
        assert!(!grid.is_expanded(&ExpandTarget::Group(msft.clone())));
        assert!(!grid.is_expanded(&ExpandTarget::Group(aapl_side("Sell"))));
        assert!(grid.is_expanded(&ExpandTarget::Group(aapl_side("Buy"))));
    });
    assert_eq!(restarted.lines(cx), lines);
    // TSLA comes first: the instruments descending.
    assert_eq!(
        restarted.lines(cx)[0],
        LineKind::Caption(group_path(&[(GroupedColumn::Instrument, "TSLA")]))
    );
}

#[gpui_kit::test]
fn test_blotter_group_footers(cx: &mut TestAppContext) {
    // The blotter's `BLOTTER_FOOTERS=1`: a footer of the group's aggregates
    // under every expanded group (`EveryGroupFooter`), with the totals row.
    // The Linux e2e `footer.*` scenarios check the same in a window.
    let rows = grouped_rows(12);
    let fixture = open_grouped(cx, rows.clone(), |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(GroupedColumn::Price, SortDirection::Descending)])
                .with_group_by([GroupBy::column(GroupedColumn::Instrument)])
                .with_aggregates([(GroupedColumn::Qty, Aggregate::Sum)])
                .with_group_footer(EveryGroupFooter),
        )
        .with_totals(PinnedEdge::Bottom)
        .with_selection_mode(SelectionMode::RowSet)
        .with_tuning(unthrottled())
    });
    let instrument = |name: &str| group_path(&[(GroupedColumn::Instrument, name)]);
    // AAPL holds trades 0, 4 and 8: its caption, them, its footer; then
    // MSFT's caption.
    let aapl_trades = ids(&rows, |row| row.instrument == "AAPL");
    let lines = fixture.lines(cx);
    assert_eq!(lines[0], LineKind::Caption(aapl()));
    assert!(
        lines[1..4]
            .iter()
            .all(|line| matches!(line, LineKind::Data(id) if aapl_trades.contains(id)))
    );
    assert_eq!(lines[4], LineKind::Footer(aapl()));
    assert_eq!(lines[5], LineKind::Caption(instrument("MSFT")));
    assert_eq!(
        lines.len(),
        4 * 5,
        "a caption, three trades and a footer each"
    );

    // A collapsed group shows no footer.
    fixture.input(cx, |window, cx| {
        press_chevron(window, 5, 0, Modifiers::default(), cx)
    });
    fixture.settle(cx);
    let collapsed = fixture.lines(cx);
    assert_eq!(collapsed[6], LineKind::Caption(instrument("NVDA")));
    assert!(!collapsed.contains(&LineKind::Footer(instrument("MSFT"))));

    // ↓ from AAPL's last trade steps over its footer to MSFT's caption; ↑
    // comes back.
    let LineKind::Data(last) = lines[3] else {
        panic!("line 3 is a trade: {:?}", lines[3]);
    };
    fixture.input(cx, |window, cx| {
        press_at(
            window,
            grouped_cell(3, GroupedColumn::Qty),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture.settle(cx);
    let focus = |fixture: &GroupedFixture, cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            grid.focused_cell()
                .map(|cell| (cell.row().copied(), cell.group().cloned()))
        })
    };
    assert_eq!(focus(&fixture, cx), Some((Some(last), None)));
    type_key(&fixture, cx, "down");
    assert_eq!(focus(&fixture, cx), Some((None, Some(instrument("MSFT")))));
    type_key(&fixture, cx, "up");
    assert_eq!(focus(&fixture, cx), Some((Some(last), None)));

    // A press on the footer: a `Click` on it, the selection and the focus
    // stay.
    fixture.events.borrow_mut().clear();
    fixture.input(cx, |window, cx| {
        press_at(
            window,
            grouped_cell(4, GroupedColumn::Qty),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture.settle(cx);
    let clicks: Vec<GridTarget<u32, GroupedColumn>> = fixture
        .events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::Click { target, .. } => Some(target.clone()),
            LiveGridEvent::SelectionChange { .. } | LiveGridEvent::FocusChange { .. } => {
                panic!("a press on a footer moved the selection or the focus: {event:?}")
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        clicks,
        [GridTarget::GroupFooter {
            group: aapl(),
            column: GroupedColumn::Qty
        }]
    );
    assert_eq!(focus(&fixture, cx), Some((Some(last), None)));

    // Copy all expanded (the Groups menu): every footer, the collapsed
    // group's too, after its trades with its sum under Qty; the totals
    // last.
    assert!(
        fixture
            .grid
            .update(cx, |grid, cx| grid.copy_scope(ExportScope::Expanded, cx))
    );
    fixture.settle(cx);
    let copied = clipboard_text(cx);
    let lines: Vec<Vec<&str>> = copied
        .lines()
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(
        lines.len(),
        1 + 4 * 5 + 1,
        "the header, four groups, the totals"
    );
    let aapl_qty: i64 = rows
        .iter()
        .filter(|row| row.instrument == "AAPL")
        .map(|row| row.qty)
        .sum();
    assert_eq!(
        lines[5][GroupedColumn::Qty.ix().get()],
        aapl_qty.to_string()
    );
    assert_eq!(
        lines.last().expect("lines")[GroupedColumn::Id.ix().get()],
        "Total"
    );
}
