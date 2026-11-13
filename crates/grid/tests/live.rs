//! Headless UI tests of live data on screen
//! (`.agents/COMPONENT_TEST_RULES.md`): the direction of a change from the
//! store to the arrow and to the cell's description, a removed row fading out
//! on its line, and the changes of group captions.
//!
//! The highlight lasts 800 ms in 8 steps, so the global tick is 100 ms; the
//! store's removal fade is set where a test needs it, which the shared
//! fixtures do not do, so the grids open here.

mod common;

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui_kit::component::{ActiveTheme as _, Sizable as _, Size, menu::PopupMenu};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Focusable as _, Hsla,
    IntoElement, Modifiers, MouseButton, ParentElement as _, Pixels, Point, Render, Styled as _,
    TestAppContext, Window, WindowBounds, WindowOptions, div, px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    Aggregate, AggregateLine, AggregateStyleContext, CellChange, CellStyle, CellStyleContext,
    CellTooltip, CellTooltipContext, ChangeDirection, ChangePaint, ChangePainter, ChangePalette,
    ColumnIx, DirectionalChanges, EveryGroupFooter, ExpandDepth, GridColumn, GridColumns as _,
    GridMenuContext, GridRow, GridSchema, GridStats, GridTarget, GridWriter, GroupBy, GroupPath,
    HighlightPolicy, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, LiveReorder,
    PinnedEdge, RowChange, SelectionMode, SelectionOrigin, SortDirection, StoreOptions,
    UniformChanges, ViewSpec,
};

use crate::common::{
    GroupedColumn, GroupedRow, Item, ItemColumn, Items, ROW, Side, WINDOW_HEIGHT, by_price,
    group_path, grouped_rows, grouped_schema, move_pointer_to, press_at, unthrottled,
};

/// A highlight of 800 ms in 8 steps: the global tick is 100 ms.
fn policy() -> HighlightPolicy {
    HighlightPolicy::new(Duration::from_millis(800), 8)
}

/// The store options of a test: [`policy`], with a removal fade of `fade`.
fn options(fade: Duration) -> StoreOptions {
    StoreOptions::default().with_highlight(policy().with_removal_fade(fade))
}

struct Root<D: LiveGridDelegate> {
    grid: Entity<LiveGridState<D>>,
}

impl<D: LiveGridDelegate> Render for Root<D> {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            LiveGrid::new(&self.grid)
                .with_size(Size::XSmall)
                .bordered(false),
        )
    }
}

/// An `xsmall` grid in a window of the header and ten rows, and its events.
struct Live<D: LiveGridDelegate> {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<D>>,
    events: Rc<RefCell<Vec<LiveGridEvent<<D::Row as GridRow>::Key, <D::Row as GridRow>::Column>>>>,
}

/// A grid of `schema` over a store with `options`, loaded with `rows` and
/// configured by `configure`.
fn open_live<D: LiveGridDelegate>(
    cx: &mut TestAppContext,
    schema: GridSchema<<D::Row as GridRow>::Column>,
    width: f32,
    delegate: D,
    options: StoreOptions,
    rows: Vec<D::Row>,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> Live<D> {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(width), px(WINDOW_HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            configure(LiveGridState::new_standalone(schema, delegate, options, cx))
                        }),
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(&grid, move |_, event: &LiveGridEvent<_, _>, _| {
            events.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all(rows.into_iter().map(Arc::new));
    let live = Live {
        window,
        grid,
        events,
    };
    live.settle(cx);
    live
}

/// A grid of `count` items (`price = id * 10`), two columns 100 px wide.
fn open_items<D: LiveGridDelegate<Row = Item>>(
    cx: &mut TestAppContext,
    count: u32,
    delegate: D,
    options: StoreOptions,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> Live<D> {
    let schema = GridSchema::new()
        .column(GridColumn::new(ItemColumn::Id, "Id"))
        .column(GridColumn::new(ItemColumn::Price, "Price"));
    let rows = (0..count)
        .map(|id| Item {
            id,
            price: i64::from(id) * 10,
        })
        .collect();
    open_live(cx, schema, 400., delegate, options, rows, configure)
}

impl<D: LiveGridDelegate> Live<D> {
    /// Runs ingest, pulls and background jobs, then draws a frame.
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx, Duration::from_millis(20));
        }
    }

    /// Advances the clock by `elapsed`, runs what is due, draws a frame.
    fn frame(&self, cx: &mut TestAppContext, elapsed: Duration) {
        cx.executor().advance_clock(elapsed);
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    }

    /// Sends input to the window.
    fn input(&self, cx: &mut TestAppContext, input: impl FnOnce(&mut Window, &mut App)) {
        cx.update_window(self.window, |_, window, cx| input(window, cx))
            .expect("window is open");
    }

    /// Gives the grid the keyboard focus.
    fn focus(&self, cx: &mut TestAppContext) {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
        });
    }

    fn writer(&self, cx: &mut TestAppContext) -> GridWriter<D::Row> {
        self.grid.read_with(cx, |grid, _| grid.writer())
    }

    fn stats(&self, cx: &mut TestAppContext) -> GridStats {
        self.grid.read_with(cx, |grid, _| grid.stats())
    }

    fn take_events(
        &self,
    ) -> Vec<LiveGridEvent<<D::Row as GridRow>::Key, <D::Row as GridRow>::Column>> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
}

/// The centre of a body cell of an item grid on screen line `line`.
fn item_cell(line: usize, column: ItemColumn) -> Point<Pixels> {
    let x = match column {
        ItemColumn::Id => 50.,
        ItemColumn::Price => 150.,
    };
    Point::new(px(x), px(ROW * (line as f32 + 1.5)))
}

/// A cell as `cell_style` saw it, with the style the change paint gave it.
#[derive(Clone, Copy, Debug)]
struct SeenCell {
    key: u32,
    column: ItemColumn,
    display_ix: usize,
    change: Option<CellChange>,
    background: Option<Hsla>,
    foreground: Option<Hsla>,
    trend: Option<ChangeDirection>,
}

/// Records the cells styled, the menus opened and the tooltips asked for.
#[derive(Clone, Default)]
struct Recorder {
    cells: Rc<RefCell<Vec<SeenCell>>>,
    menus: Rc<RefCell<Vec<GridTarget<u32, ItemColumn>>>>,
    tooltips: Rc<RefCell<Vec<u32>>>,
}

impl Recorder {
    /// Forgets every cell seen so far.
    fn clear(&self) {
        self.cells.borrow_mut().clear();
    }

    /// The cells of `key` seen since the last [`Self::clear`].
    fn cells_of(&self, key: u32) -> Vec<SeenCell> {
        self.cells
            .borrow()
            .iter()
            .filter(|cell| cell.key == key)
            .copied()
            .collect()
    }

    /// The last time a cell was seen.
    fn last(&self, key: u32, column: ItemColumn) -> SeenCell {
        self.cells
            .borrow()
            .iter()
            .rev()
            .find(|cell| (cell.key, cell.column) == (key, column))
            .copied()
            .expect("the cell was painted")
    }
}

impl LiveGridDelegate for Recorder {
    type Row = Item;

    fn cell_style(&self, cell: &CellStyleContext<'_, Item>, style: &mut CellStyle, _: &App) {
        self.cells.borrow_mut().push(SeenCell {
            key: cell.row().id,
            column: cell.column(),
            display_ix: cell.display_ix(),
            change: cell.change(),
            background: style.background(),
            foreground: style.foreground(),
            trend: style.trend(),
        });
    }

    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, Item>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        self.menus.borrow_mut().push(menu_context.target().clone());
        menu_context.standard_items(menu, window, cx)
    }

    fn tooltip(&self, cell: &CellTooltipContext<'_, Item>, _: &App) -> Option<CellTooltip> {
        self.tooltips.borrow_mut().push(cell.row().id);
        cell.default_tooltip()
    }
}

#[gpui_kit::test]
fn test_direction_reaches_style(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    let live = open_items(cx, 10, recorder.clone(), options(Duration::ZERO), |grid| {
        grid.with_tuning(unthrottled())
            .with_change_paint(ChangePaint::Directional(DirectionalChanges))
    });
    let (success, danger) = cx.update(|cx| (cx.theme().success, cx.theme().danger));

    // A rise, from the producer's write to the arrow: the store stamps the
    // direction, the cell's change carries it, the change paint shows it.
    live.writer(cx).upsert(Item { id: 2, price: 25 });
    live.settle(cx);
    let cell = recorder.last(2, ItemColumn::Price);
    let change = cell.change.expect("the price changed");
    assert_eq!(change.change(), RowChange::Updated);
    assert_eq!(change.direction(), Some(ChangeDirection::Up));
    assert_eq!(cell.trend, Some(ChangeDirection::Up));
    assert_eq!(
        cell.background,
        Some(success.opacity(0.45 * change.intensity()))
    );
    // The id did not change: no direction, no arrow.
    assert_eq!(recorder.last(2, ItemColumn::Id).change, None);
    assert_eq!(live.stats(cx).icons(), 1, "one arrow");

    // Then a fall.
    live.writer(cx).upsert(Item { id: 2, price: 5 });
    live.settle(cx);
    let cell = recorder.last(2, ItemColumn::Price);
    let change = cell.change.expect("the price changed");
    assert_eq!(change.direction(), Some(ChangeDirection::Down));
    assert_eq!(cell.trend, Some(ChangeDirection::Down));
    assert_eq!(
        cell.background,
        Some(danger.opacity(0.45 * change.intensity()))
    );
    assert_eq!(live.stats(cx).icons(), 1);

    // The uniform paint, while the change is hot: the next frame shows no
    // arrow, nothing is rebuilt, and the direction is still recorded.
    let before = live.stats(cx);
    live.grid.update(cx, |grid, cx| {
        grid.set_change_paint(ChangePaint::Uniform(UniformChanges), cx)
    });
    live.frame(cx, Duration::ZERO);
    let after = live.stats(cx);
    assert!(after.frames() > before.frames(), "repainted");
    assert_eq!(after.rebuilds(), before.rebuilds(), "not rebuilt");
    assert_eq!(after.icons(), 0);
    let cell = recorder.last(2, ItemColumn::Price);
    assert_eq!(cell.trend, None);
    assert_eq!(
        cell.change.and_then(|change| change.direction()),
        Some(ChangeDirection::Down)
    );

    // And back, under the stream.
    live.grid.update(cx, |grid, cx| {
        grid.set_change_paint(ChangePaint::Directional(DirectionalChanges), cx)
    });
    live.writer(cx).upsert(Item { id: 4, price: 41 });
    live.settle(cx);
    assert_eq!(
        recorder.last(4, ItemColumn::Price).trend,
        Some(ChangeDirection::Up)
    );
    assert_eq!(live.stats(cx).icons(), 2, "rows 2 and 4");
}

#[gpui_kit::test]
fn test_fading_line_on_screen(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    let live = open_items(
        cx,
        10,
        recorder.clone(),
        options(Duration::from_millis(400)),
        |grid| {
            grid.with_tuning(unthrottled())
                .with_selection_mode(SelectionMode::RowSet)
        },
    );
    live.grid
        .update(cx, |grid, cx| grid.set_selected_keys([3, 5], cx));
    live.settle(cx);
    live.take_events();
    let id = live
        .grid
        .read_with(cx, |grid, _| grid.snapshot().row_id(&3))
        .expect("row 3 is in the store");

    // The selection lets go of the removed row at once.
    live.writer(cx).remove(3);
    live.settle(cx);
    let selection_changes: Vec<_> = live
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::SelectionChange {
                selection, origin, ..
            } => Some((selection.iter().copied().collect::<Vec<_>>(), origin)),
            _ => None,
        })
        .collect();
    assert_eq!(selection_changes, [(vec![5], SelectionOrigin::DataRemoved)]);

    // Row 3 keeps its line, every cell removed: the change paint's danger
    // background and muted text, its last values.
    recorder.clear();
    live.frame(cx, Duration::ZERO);
    let (danger, muted) = cx.update(|cx| (cx.theme().danger, cx.theme().muted_foreground));
    let cells = recorder.cells_of(3);
    assert_eq!(cells.len(), 2, "both cells of row 3");
    for cell in cells {
        assert_eq!(cell.display_ix, 3);
        let change = cell.change.expect("the row fades");
        assert_eq!(change.change(), RowChange::Removed);
        assert_eq!(change.direction(), None);
        assert_eq!(
            cell.background,
            Some(danger.opacity(0.45 * change.intensity()))
        );
        assert_eq!(cell.foreground, Some(muted));
        assert_eq!(cell.trend, None);
    }
    let stats = live.stats(cx);
    assert_eq!((stats.fading_lines(), stats.tombstones()), (1, 0));
    assert_eq!(
        live.grid.read_with(cx, |grid, _| grid.visible_keys()),
        [0, 1, 2, 4, 5, 6, 7, 8, 9],
        "a fading row is not a row of the grid"
    );

    // Input on it lands on empty space: a press selects nothing, the click
    // is `Empty`, the menu's target too; hovering asks for no tooltip.
    live.input(cx, |window, cx| {
        press_at(
            window,
            item_cell(3, ItemColumn::Price),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    live.settle(cx);
    let selected: Vec<u32> = live
        .grid
        .read_with(cx, |grid, _| grid.selection().iter().copied().collect());
    assert_eq!(selected, [5]);
    let clicks: Vec<_> = live
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::Click { target, .. } => Some(target),
            LiveGridEvent::SelectionChange { .. } => panic!("the selection changed"),
            _ => None,
        })
        .collect();
    assert_eq!(clicks, [GridTarget::Empty]);
    live.input(cx, |window, cx| {
        press_at(
            window,
            item_cell(3, ItemColumn::Id),
            MouseButton::Right,
            Modifiers::default(),
            1,
            cx,
        )
    });
    live.settle(cx);
    assert_eq!(recorder.menus.borrow().last(), Some(&GridTarget::Empty));
    live.input(cx, |window, cx| window.press("escape", cx));
    live.input(cx, |window, cx| {
        move_pointer_to(window, item_cell(3, ItemColumn::Id), cx)
    });
    live.settle(cx);
    assert!(!recorder.tooltips.borrow().contains(&3));

    // The fade ends on the global tick: the line is skipped, the rows below
    // move up in the same frame, a press there selects row 4.
    live.frame(cx, Duration::from_millis(500));
    let stats = live.stats(cx);
    assert_eq!((stats.fading_lines(), stats.tombstones()), (0, 1));
    live.input(cx, |window, cx| {
        press_at(
            window,
            item_cell(3, ItemColumn::Price),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    live.settle(cx);
    let selected: Vec<u32> = live
        .grid
        .read_with(cx, |grid, _| grid.selection().iter().copied().collect());
    assert_eq!(selected, [4]);

    // The store drops the row when its ingest wakes next; the compaction
    // drops its id from the order and shows it.
    live.take_events();
    live.grid.update(cx, |grid, cx| grid.flush(cx)).detach();
    live.settle(cx);
    let changes: Vec<usize> = live
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::ProjectionChange { row_count, .. } => Some(row_count),
            _ => None,
        })
        .collect();
    assert_eq!(changes, [9]);
    live.grid.read_with(cx, |grid, _| {
        assert!(grid.snapshot().fading_row(id).is_none());
        assert!(!grid.projection().contains(id));
        assert_eq!(grid.projection().len(), 9);
    });
    assert_eq!(live.stats(cx).tombstones(), 0);
}

#[gpui_kit::test]
fn test_fading_line_under_live_reorder(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    let live = open_items(
        cx,
        10,
        recorder.clone(),
        options(Duration::from_millis(400)),
        |grid| {
            grid.with_view(by_price(SortDirection::Ascending))
                .with_tuning(unthrottled())
                .with_reorder_policy(LiveReorder)
        },
    );
    let id = live
        .grid
        .read_with(cx, |grid, _| grid.snapshot().row_id(&3))
        .expect("row 3 is in the store");
    live.writer(cx).remove(3);
    live.settle(cx);

    // The stream moves row 0 below the removed row's price, 30: the order
    // is rebuilt, and the fading row stands by its last price.
    let rebuilds = live.stats(cx).rebuilds();
    live.writer(cx).upsert(Item { id: 0, price: 35 });
    live.settle(cx);
    assert!(live.stats(cx).rebuilds() > rebuilds, "rebuilt");
    assert_eq!(
        live.grid
            .read_with(cx, |grid, _| grid.projection().display_ix(id)),
        Some(2),
        "after 10 and 20"
    );
    assert_eq!(
        live.grid.read_with(cx, |grid, _| grid.visible_keys()),
        [1, 2, 0, 4, 5, 6, 7, 8, 9]
    );
    recorder.clear();
    live.frame(cx, Duration::ZERO);
    let cells = recorder.cells_of(3);
    assert!(!cells.is_empty());
    for cell in cells {
        assert_eq!(cell.display_ix, 2);
        assert_eq!(
            cell.change.map(|change| change.change()),
            Some(RowChange::Removed)
        );
    }
    assert_eq!(recorder.last(0, ItemColumn::Price).display_ix, 3);

    // Rebuilt again before the fade ends, it is still there.
    live.writer(cx).upsert(Item { id: 9, price: 15 });
    live.settle(cx);
    assert_eq!(
        live.grid
            .read_with(cx, |grid, _| grid.projection().display_ix(id)),
        Some(3),
        "after 10, 15 and 20"
    );
    assert_eq!(live.stats(cx).fading_lines(), 1);

    // Then it goes.
    live.frame(cx, Duration::from_millis(400));
    assert_eq!(live.stats(cx).fading_lines(), 0);
    assert_eq!(
        live.grid.read_with(cx, |grid, _| grid.visible_keys()),
        [1, 9, 2, 0, 4, 5, 6, 7, 8]
    );
}

/// Paints as [`UniformChanges`] and records every change it paints: on
/// captions-only screens, the captions' lanes and aggregate cells.
#[derive(Default)]
struct PaintRecorder(
    Mutex<Vec<(RowChange, Option<ChangeDirection>)>>,
    Mutex<Vec<Option<ColumnIx>>>,
);

impl PaintRecorder {
    fn take(&self) -> Vec<(RowChange, Option<ChangeDirection>)> {
        std::mem::take(&mut *self.0.lock().expect("not poisoned"))
    }

    /// The columns of the changes painted, in the order painted.
    fn take_columns(&self) -> Vec<Option<ColumnIx>> {
        std::mem::take(&mut *self.1.lock().expect("not poisoned"))
    }
}

impl ChangePainter for PaintRecorder {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        self.0
            .lock()
            .expect("not poisoned")
            .push((change.change(), change.direction()));
        self.1
            .lock()
            .expect("not poisoned")
            .push(change.column_ix());
        UniformChanges.paint(change, palette, style);
    }
}

/// An aggregate cell of a caption as `aggregate_style` saw it.
type SeenAggregate = (GroupPath, GroupedColumn, Option<CellChange>);

/// Records the aggregate cells of captions.
#[derive(Clone, Default)]
struct CaptionRecorder(Rc<RefCell<Vec<SeenAggregate>>>);

impl CaptionRecorder {
    fn take(&self) -> Vec<SeenAggregate> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

impl LiveGridDelegate for CaptionRecorder {
    type Row = GroupedRow;

    fn aggregate_style(
        &self,
        cell: &AggregateStyleContext<'_, GroupedRow>,
        _: &mut CellStyle,
        _: &App,
    ) {
        if let AggregateLine::Caption(group) = cell.line() {
            self.0
                .borrow_mut()
                .push((group.path().clone(), cell.column(), cell.change()));
        }
    }
}

fn instrument(name: &str) -> GroupPath {
    group_path(&[(GroupedColumn::Instrument, name)])
}

/// The change of the Sum(Qty) cell of `name`'s caption in the frame seen.
fn qty_change(seen: &[SeenAggregate], name: &str) -> Option<CellChange> {
    let path = instrument(name);
    seen.iter()
        .find(|(seen, column, _)| *seen == path && *column == GroupedColumn::Qty)
        .expect("the caption's sum is painted")
        .2
}

#[gpui_kit::test]
fn test_caption_highlight(cx: &mut TestAppContext) {
    use GroupedColumn::{Id, Instrument, Qty};

    // Three groups of three trades, every group collapsed: the captions'
    // changes are all the frame paints.
    let rows: Vec<GroupedRow> = grouped_rows(12)
        .into_iter()
        .filter(|row| row.instrument != "TSLA")
        .collect();
    let painter = Arc::new(PaintRecorder::default());
    let recorder = CaptionRecorder::default();
    let live = open_live(
        cx,
        grouped_schema(),
        480.,
        recorder.clone(),
        options(Duration::ZERO),
        rows,
        {
            let painter = painter.clone();
            move |grid| {
                grid.with_view(
                    ViewSpec::new()
                        .with_sort([(Id, SortDirection::Ascending)])
                        .with_group_by([GroupBy::column(Instrument)])
                        .with_aggregates([(Qty, Aggregate::Sum)]),
                )
                .with_expand_depth(ExpandDepth::Levels(0))
                .with_tuning(unthrottled())
                .with_change_paint(ChangePaint::Custom(painter))
            }
        },
    );
    let frame = |cx: &mut TestAppContext| {
        recorder.take();
        painter.take();
        live.frame(cx, Duration::ZERO);
        (recorder.take(), painter.take())
    };
    let (seen, painted) = frame(cx);
    assert_eq!(seen.len(), 3, "three captions, one sum each");
    assert!(seen.iter().all(|(_, _, change)| change.is_none()));
    assert!(painted.is_empty(), "nothing changed since the load");

    // A trade of the collapsed MSFT group grows: its sum rises, the
    // caption's row count stays.
    let trade = |id: u32, instrument: &'static str, qty: i64| GroupedRow {
        id,
        instrument,
        side: Side::Buy,
        qty,
        price: 100.,
        trader: "Ann",
        parent: None,
    };
    live.writer(cx).upsert(trade(1, "MSFT", 900));
    live.settle(cx);
    painter.take_columns();
    let (seen, painted) = frame(cx);
    // The sum's cell, in its column.
    assert_eq!(painter.take_columns(), [Some(Qty.ix())]);
    let change = qty_change(&seen, "MSFT").expect("the sum changed");
    assert_eq!(change.change(), RowChange::Updated);
    assert_eq!(change.direction(), Some(ChangeDirection::Up));
    assert_eq!(qty_change(&seen, "AAPL"), None);
    assert_eq!(qty_change(&seen, "NVDA"), None);
    assert_eq!(
        painted,
        [(RowChange::Updated, Some(ChangeDirection::Up))],
        "the sum's cell alone"
    );

    // A new MSFT trade: the row count changes too, and the caption's lane
    // shows it, an update without a direction.
    live.writer(cx).upsert(trade(13, "MSFT", 100));
    live.settle(cx);
    let msft = live.grid.read_with(cx, |grid, _| {
        let projection = grid.projection();
        let group = projection
            .groups()
            .find(&instrument("MSFT"))
            .expect("MSFT")
            .id();
        projection.caption_stamp(group).map(|stamp| stamp.change())
    });
    assert_eq!(msft, Some(RowChange::Updated));
    let (seen, mut painted) = frame(cx);
    assert_eq!(
        qty_change(&seen, "MSFT").and_then(|change| change.direction()),
        Some(ChangeDirection::Up)
    );
    painted.sort_by_key(|(change, direction)| (format!("{change:?}"), direction.is_some()));
    assert_eq!(
        painted,
        [
            (RowChange::Updated, None),
            (RowChange::Updated, Some(ChangeDirection::Up)),
        ]
    );

    // A trade of a new instrument: a new group, whose caption flashes as
    // inserted; its sum has no change of its own.
    live.writer(cx).upsert(trade(3, "TSLA", 400));
    live.settle(cx);
    let (seen, painted) = frame(cx);
    assert_eq!(seen.len(), 4);
    assert_eq!(qty_change(&seen, "TSLA"), None);
    assert_eq!(
        painted
            .iter()
            .filter(|(change, _)| *change == RowChange::Inserted)
            .count(),
        1,
        "the new caption's lane"
    );

    // When the highlight ends, the captions show no change.
    live.frame(cx, Duration::from_millis(900));
    let (seen, painted) = frame(cx);
    assert!(seen.iter().all(|(_, _, change)| change.is_none()));
    assert!(painted.is_empty());
}

#[gpui_kit::test]
fn test_trend_description(cx: &mut TestAppContext) {
    let live = open_items(cx, 10, Items, options(Duration::ZERO), |grid| {
        grid.with_tuning(unthrottled())
            .with_change_paint(ChangePaint::Directional(DirectionalChanges))
    });
    live.grid.update(cx, |grid, _| grid.set_a11y_forced(true));
    live.focus(cx);
    live.grid.update(cx, |grid, cx| {
        assert!(grid.set_focused_cell(&2, Some(ItemColumn::Price), cx));
    });
    live.settle(cx);
    let description = |cx: &mut TestAppContext| {
        live.grid.read_with(cx, |grid, _| {
            grid.a11y_focus_description().map(str::to_owned)
        })
    };
    assert_eq!(description(cx).as_deref(), Some("Price"));

    // A rising cell says so after its column title; a falling one too.
    live.writer(cx).upsert(Item { id: 2, price: 25 });
    live.settle(cx);
    assert_eq!(description(cx).as_deref(), Some("Price. rising"));
    live.writer(cx).upsert(Item { id: 2, price: 5 });
    live.settle(cx);
    assert_eq!(description(cx).as_deref(), Some("Price. falling"));

    // Without the arrow, no word: the uniform paint, or the end of the
    // highlight.
    live.grid.update(cx, |grid, cx| {
        grid.set_change_paint(ChangePaint::Uniform(UniformChanges), cx)
    });
    live.frame(cx, Duration::ZERO);
    assert_eq!(description(cx).as_deref(), Some("Price"));
    live.grid.update(cx, |grid, cx| {
        grid.set_change_paint(ChangePaint::Directional(DirectionalChanges), cx)
    });
    live.frame(cx, Duration::ZERO);
    assert_eq!(description(cx).as_deref(), Some("Price. falling"));
    live.frame(cx, Duration::from_millis(900));
    assert_eq!(description(cx).as_deref(), Some("Price"));
}

/// An aggregate cell of a footer or the totals row, as `aggregate_style`
/// saw it: the footer's group (`None` for the totals), the column, the change.
type SeenSummary = (Option<GroupPath>, GroupedColumn, Option<CellChange>);

/// Records the aggregate cells of footers and of the totals row.
#[derive(Clone, Default)]
struct SummaryRecorder(Rc<RefCell<Vec<SeenSummary>>>);

impl SummaryRecorder {
    fn take(&self) -> Vec<SeenSummary> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

impl LiveGridDelegate for SummaryRecorder {
    type Row = GroupedRow;

    fn aggregate_style(
        &self,
        cell: &AggregateStyleContext<'_, GroupedRow>,
        _: &mut CellStyle,
        _: &App,
    ) {
        let group = match cell.line() {
            AggregateLine::Footer(group) => Some(group.path().clone()),
            AggregateLine::Totals(_) => None,
            _ => return,
        };
        if cell.value().is_some() {
            self.0
                .borrow_mut()
                .push((group, cell.column(), cell.change()));
        }
    }
}

#[gpui_kit::test]
fn test_totals_and_footer_highlight(cx: &mut TestAppContext) {
    use GroupedColumn::{Id, Instrument, Qty};

    // Two groups of two trades, footers on, totals at the bottom.
    let rows: Vec<GroupedRow> = grouped_rows(8)
        .into_iter()
        .filter(|row| matches!(row.instrument, "AAPL" | "MSFT"))
        .collect();
    let painter = Arc::new(PaintRecorder::default());
    let recorder = SummaryRecorder::default();
    let live = open_live(
        cx,
        grouped_schema(),
        480.,
        recorder.clone(),
        options(Duration::ZERO),
        rows,
        {
            let painter = painter.clone();
            move |grid| {
                grid.with_view(
                    ViewSpec::new()
                        .with_sort([(Id, SortDirection::Ascending)])
                        .with_group_by([GroupBy::column(Instrument)])
                        .with_aggregates([(Qty, Aggregate::Sum)])
                        .with_group_footer(EveryGroupFooter),
                )
                .with_totals(PinnedEdge::Bottom)
                .with_tuning(unthrottled())
                .with_change_paint(ChangePaint::Custom(painter))
            }
        },
    );
    let frame = |cx: &mut TestAppContext| {
        recorder.take();
        live.frame(cx, Duration::ZERO);
        recorder.take()
    };
    let change_of = |seen: &[SeenSummary], group: Option<GroupPath>| {
        seen.iter()
            .find(|(seen, column, _)| *seen == group && *column == Qty)
            .expect("the summary's sum is painted")
            .2
    };
    let seen = frame(cx);
    assert!(seen.iter().all(|(_, _, change)| change.is_none()));

    // A trade of MSFT grows: MSFT's footer and the totals rise, AAPL's
    // footer does not change.
    let msft = grouped_rows(2)[1].clone();
    live.writer(cx).upsert(GroupedRow {
        qty: msft.qty + 500,
        ..msft
    });
    live.settle(cx);
    painter.take();
    let seen = frame(cx);
    for group in [Some(instrument("MSFT")), None] {
        let change = change_of(&seen, group.clone()).expect("the sum changed");
        assert_eq!(change.change(), RowChange::Updated, "{group:?}");
        assert_eq!(change.direction(), Some(ChangeDirection::Up), "{group:?}");
    }
    assert_eq!(change_of(&seen, Some(instrument("AAPL"))), None);
    // The change paint painted them: the caption's sum, the footer's and
    // the totals', each a rise.
    assert!(
        painter
            .take()
            .iter()
            .filter(|painted| **painted == (RowChange::Updated, Some(ChangeDirection::Up)))
            .count()
            >= 2
    );

    // When the highlight ends, no change.
    live.frame(cx, Duration::from_millis(900));
    let seen = frame(cx);
    assert!(seen.iter().all(|(_, _, change)| change.is_none()));
}
