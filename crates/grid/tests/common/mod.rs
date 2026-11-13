//! The shared fixture of the headless UI tests: a two-column grid of
//! `Item`s in a window, pointer input with positions, buttons and modifiers
//! (`TestWindowExt` clicks element centers without modifiers), and a task
//! runner that can hold jobs back. For stage 2, a grid of trades to group
//! (`GroupedFixture`), group paths and caption lines, the log of the
//! grouping events (`GroupSeen`), and the element ids of the accessibility
//! rows of totals and group footers. For stage 3, a grid of orders with
//! every kind of cell value (`EditFixture`).

// Every test binary compiles this module and uses only part of it.
#![allow(dead_code)]

use std::{
    cell::{Cell, RefCell},
    hash::{Hash, Hasher as _},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, TimeZone as _, Utc};
use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, ElementId, Entity, Focusable as _,
    InputEvent, InteractiveElement as _, IntoElement, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Task, TestAppContext, Window, WindowBounds,
    WindowOptions, div,
    prelude::FluentBuilder as _,
    px, size,
    test::{TestSupportExt as _, TestWindowExt as _},
};
use gpui_kit_grid::{
    CellValue, ColumnIx, DisplayRow, DropSource, DropTarget, EditError, EditValidateContext,
    EditableContext, ExpandTarget, GridColumn, GridColumns, GridRow, GridSchema, GridTuning,
    GridWriter, GroupOrigin, GroupPath, IconValue, LiveGrid, LiveGridDelegate, LiveGridEvent,
    LiveGridState, NumberEditor, OwnedValue, PinnedEdge, SelectionOrigin, SortDirection,
    SortOrigin, StoreOptions, TaskRunner, TextEditor, ViewSpec,
};

pub const ROW: f32 = 26.;
/// Header plus ten rows at the `xsmall` size.
pub const WINDOW_HEIGHT: f32 = ROW * 11.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemColumn {
    Id,
    Price,
}

impl GridColumns for ItemColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Price]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Price => "price",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: u32,
    pub price: i64,
}

impl GridRow for Item {
    type Key = u32;
    type Column = ItemColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: ItemColumn) -> CellValue<'_> {
        match column {
            ItemColumn::Id => CellValue::Int(self.id.into()),
            ItemColumn::Price => CellValue::Int(self.price),
        }
    }
}

pub struct Items;

impl LiveGridDelegate for Items {
    type Row = Item;
}

struct Root<D: LiveGridDelegate> {
    grid: Entity<LiveGridState<D>>,
    /// The `LiveGrid` size, or `None` to embed the state view directly.
    size: Option<Size>,
}

impl<D: LiveGridDelegate> Render for Root<D> {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .map(|this| match self.size {
                Some(size) => this.child(LiveGrid::new(&self.grid).with_size(size).bordered(false)),
                None => this.child(self.grid.clone()),
            })
            // A spot over the header, outside the body: hovering it moves
            // the pointer off the rows.
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

/// A grid in a window and its events; `D` is `Items` unless a test
/// records the delegate's hooks ([`open_with`]).
pub struct Fixture<D: LiveGridDelegate = Items> {
    pub window: AnyWindowHandle,
    pub grid: Entity<LiveGridState<D>>,
    pub events: Rc<RefCell<Vec<LiveGridEvent<u32, ItemColumn>>>>,
}

/// An `xsmall` grid of `count` rows (`price = id * 10`) with `view`.
pub fn open(
    cx: &mut TestAppContext,
    count: u32,
    view: ViewSpec<Item>,
    min_rebuild_interval: Duration,
) -> Fixture {
    open_configured(cx, count, Some(Size::XSmall), move |grid| {
        grid.with_view(view)
            .with_tuning(GridTuning::default().with_min_rebuild_interval(min_rebuild_interval))
    })
}

/// A grid of `count` rows (`price = id * 10`) in a window of the `xsmall`
/// height, configured by `configure`.
pub fn open_configured(
    cx: &mut TestAppContext,
    count: u32,
    grid_size: Option<Size>,
    configure: impl FnOnce(LiveGridState<Items>) -> LiveGridState<Items> + 'static,
) -> Fixture {
    open_with(cx, count, grid_size, Items, configure)
}

/// The events a grid of `R` rows emits.
type EventLog<R> = Rc<RefCell<Vec<LiveGridEvent<<R as GridRow>::Key, <R as GridRow>::Column>>>>;

/// A grid of `schema` in a window `width` wide and of the `xsmall` height,
/// configured by `configure`, and the log of its events.
fn open_grid<D: LiveGridDelegate>(
    cx: &mut TestAppContext,
    schema: GridSchema<<D::Row as GridRow>::Column>,
    width: f32,
    grid_size: Option<Size>,
    delegate: D,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> (AnyWindowHandle, Entity<LiveGridState<D>>, EventLog<D::Row>) {
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
                            configure(LiveGridState::new_standalone(
                                schema,
                                delegate,
                                StoreOptions::default(),
                                cx,
                            ))
                        }),
                        size: grid_size,
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events: EventLog<D::Row> = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(&grid, move |_, event: &LiveGridEvent<_, _>, _| {
            events.borrow_mut().push(event.clone());
        })
        .detach();
    });
    (window, grid, events)
}

/// [`open_configured`] with another delegate of `Item` rows, such as one
/// that records its hooks.
pub fn open_with<D: LiveGridDelegate<Row = Item>>(
    cx: &mut TestAppContext,
    count: u32,
    grid_size: Option<Size>,
    delegate: D,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> Fixture<D> {
    let schema = GridSchema::new()
        .column(GridColumn::new(ItemColumn::Id, "Id"))
        .column(GridColumn::new(ItemColumn::Price, "Price"));
    let (window, grid, events) = open_grid(cx, schema, 400., grid_size, delegate, configure);
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all((0..count).map(|id| {
        Arc::new(Item {
            id,
            price: i64::from(id) * 10,
        })
    }));
    let fixture = Fixture {
        window,
        grid,
        events,
    };
    fixture.settle(cx);
    fixture
}

pub fn by_price(direction: SortDirection) -> ViewSpec<Item> {
    ViewSpec::new().with_sort([(ItemColumn::Price, direction)])
}

pub fn unthrottled() -> GridTuning {
    GridTuning::default().with_min_rebuild_interval(Duration::ZERO)
}

impl<D: LiveGridDelegate<Row = Item>> Fixture<D> {
    /// Runs ingest, pulls and background jobs, then draws a frame.
    pub fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx, Duration::from_millis(20));
        }
    }

    pub fn frame(&self, cx: &mut TestAppContext, elapsed: Duration) {
        cx.executor().advance_clock(elapsed);
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    }

    pub fn hover(&self, cx: &mut TestAppContext, id: &'static str) {
        cx.update_window(self.window, |_, window, cx| window.hover(id, cx))
            .expect("window is open");
    }

    pub fn visible_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| grid.visible_keys())
    }

    /// The selected keys, sorted.
    pub fn selected_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| {
            let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
            keys.sort_unstable();
            keys
        })
    }

    pub fn is_order_held(&self, cx: &mut TestAppContext) -> bool {
        self.grid.read_with(cx, |grid, _| grid.is_order_held())
    }

    pub fn writer(&self, cx: &mut TestAppContext) -> GridWriter<Item> {
        self.grid.read_with(cx, |grid, _| grid.writer())
    }

    /// Writes `price = sign * id * 10` to rows `0..count`: flipping the sign
    /// reverses the price order.
    pub fn write_prices(&self, cx: &mut TestAppContext, count: u32, sign: i64) {
        let writer = self.writer(cx);
        for id in 0..count {
            writer.upsert(Item {
                id,
                price: sign * i64::from(id) * 10,
            });
        }
    }

    /// `held` of every `OrderHeldChange` event so far.
    pub fn held_events(&self) -> Vec<bool> {
        self.events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::OrderHeldChange { held } => Some(*held),
                _ => None,
            })
            .collect()
    }
}

/// Clicks the left button at a window position with `modifiers` held, such
/// as `Modifiers::shift()`. Draws a frame after every event, as
/// `TestWindowExt::click_at` does.
pub fn click_with(
    window: &mut Window,
    position: Point<Pixels>,
    modifiers: Modifiers,
    cx: &mut App,
) {
    press_at(window, position, MouseButton::Left, modifiers, 1, cx);
}

/// Moves the pointer to a window position, then presses and releases
/// `button` there once. `click_count` numbers the press within a series:
/// `2` is the second press of a double click.
pub fn press_at(
    window: &mut Window,
    position: Point<Pixels>,
    button: MouseButton,
    modifiers: Modifiers,
    click_count: usize,
    cx: &mut App,
) {
    mouse_down_at(window, position, button, modifiers, click_count, cx);
    mouse_up_at(window, position, button, modifiers, click_count, cx);
}

/// Moves the pointer to a window position and presses `button` without
/// releasing it, so a test can change the data before [`mouse_up_at`].
pub fn mouse_down_at(
    window: &mut Window,
    position: Point<Pixels>,
    button: MouseButton,
    modifiers: Modifiers,
    click_count: usize,
    cx: &mut App,
) {
    dispatch(
        window,
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        },
        cx,
    );
    dispatch(
        window,
        MouseDownEvent {
            button,
            position,
            modifiers,
            click_count,
            first_mouse: false,
        },
        cx,
    );
}

/// Releases `button` at a window position. The pointer is not moved first:
/// release where [`mouse_down_at`] pressed.
pub fn mouse_up_at(
    window: &mut Window,
    position: Point<Pixels>,
    button: MouseButton,
    modifiers: Modifiers,
    click_count: usize,
    cx: &mut App,
) {
    dispatch(
        window,
        MouseUpEvent {
            button,
            position,
            modifiers,
            click_count,
        },
        cx,
    );
}

/// Sends one platform event and draws a frame.
fn dispatch(window: &mut Window, event: impl InputEvent, cx: &mut App) {
    window.dispatch_event(event.to_platform_input(), cx);
    window.render_frame(cx);
}

/// Runs jobs inline, or queues them while paused, so a test can act while
/// a job is in flight.
#[derive(Clone, Default)]
pub struct PausableRunner {
    paused: Rc<Cell<bool>>,
    queued: Rc<RefCell<Vec<Box<dyn FnOnce() + Send>>>>,
}

impl PausableRunner {
    pub fn pause(&self) {
        self.paused.set(true);
    }

    pub fn resume(&self) {
        self.paused.set(false);
        for job in self.queued.take() {
            job();
        }
    }
}

impl TaskRunner for PausableRunner {
    fn run(&self, job: Box<dyn FnOnce() + Send>, _: &App) -> Task<()> {
        if self.paused.get() {
            self.queued.borrow_mut().push(job);
        } else {
            job();
        }
        Task::ready(())
    }
}

/// Moves the pointer to a window position with the left button held, as a
/// drag does between [`mouse_down_at`] and [`mouse_up_at`]. Draws a frame.
pub fn drag_move_to(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    dispatch(
        window,
        MouseMoveEvent {
            position,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        },
        cx,
    );
}

/// Moves the pointer to a window position with no button held. Draws a
/// frame.
pub fn move_pointer_to(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    dispatch(
        window,
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        },
        cx,
    );
}

// A grid of trades to group (GroupedFixture).

/// The side of a trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "Buy",
            Self::Sell => "Sell",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GroupedColumn {
    Id,
    Instrument,
    Side,
    Qty,
    Price,
    Trader,
}

impl GridColumns for GroupedColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Id,
            Self::Instrument,
            Self::Side,
            Self::Qty,
            Self::Price,
            Self::Trader,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Instrument => "instrument",
            Self::Side => "side",
            Self::Qty => "qty",
            Self::Price => "price",
            Self::Trader => "trader",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A trade: rows to group by instrument, side or trader, or to arrange as
/// a tree by `parent`.
#[derive(Clone, Debug)]
pub struct GroupedRow {
    pub id: u32,
    pub instrument: &'static str,
    pub side: Side,
    pub qty: i64,
    pub price: f64,
    pub trader: &'static str,
    pub parent: Option<u32>,
}

impl GridRow for GroupedRow {
    type Key = u32;
    type Column = GroupedColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: GroupedColumn) -> CellValue<'_> {
        match column {
            GroupedColumn::Id => CellValue::Int(self.id.into()),
            GroupedColumn::Instrument => CellValue::Text(self.instrument),
            GroupedColumn::Side => CellValue::Text(self.side.as_str()),
            GroupedColumn::Qty => CellValue::Int(self.qty),
            GroupedColumn::Price => CellValue::Float(self.price),
            GroupedColumn::Trader => CellValue::Text(self.trader),
        }
    }

    fn parent_key(&self) -> Option<&u32> {
        self.parent.as_ref()
    }
}

pub const INSTRUMENTS: [&str; 4] = ["AAPL", "MSFT", "NVDA", "TSLA"];
pub const TRADERS: [&str; 3] = ["Ann", "Bob", "Cid"];

/// `count` trades with ids `0..count`: the instrument cycles every row, the
/// side every four rows, the trader every eight, so 24 rows hold every
/// instrument × side × trader once. Quantity and price follow the id. Every
/// trade is a root of a tree.
pub fn grouped_rows(count: u32) -> Vec<GroupedRow> {
    (0..count)
        .map(|id| GroupedRow {
            id,
            instrument: INSTRUMENTS[id as usize % INSTRUMENTS.len()],
            side: if (id / 4) % 2 == 0 {
                Side::Buy
            } else {
                Side::Sell
            },
            qty: i64::from(id % 5 + 1) * 100,
            price: 100.0 + f64::from(id % 7) * 2.5,
            trader: TRADERS[(id / 8) as usize % TRADERS.len()],
            parent: None,
        })
        .collect()
}

/// Columns of [`grouped_schema`], left to right, with their widths.
pub const GROUPED_COLUMNS: [(GroupedColumn, f32); 6] = [
    (GroupedColumn::Id, 50.),
    (GroupedColumn::Instrument, 80.),
    (GroupedColumn::Side, 60.),
    (GroupedColumn::Qty, 70.),
    (GroupedColumn::Price, 80.),
    (GroupedColumn::Trader, 70.),
];

/// Every column with a fixed width, in a window wide enough for all.
pub fn grouped_schema() -> GridSchema<GroupedColumn> {
    GROUPED_COLUMNS
        .into_iter()
        .fold(GridSchema::new(), |schema, (column, width)| {
            schema.column(GridColumn::new(column, column.id()).with_width(px(width)))
        })
}

/// The window position of the centre of a body cell of an `xsmall`
/// [`GroupedFixture`]: `line` counts the shown lines from the top.
pub fn grouped_cell(line: usize, column: GroupedColumn) -> Point<Pixels> {
    let mut x = 0.;
    for (each, width) in GROUPED_COLUMNS {
        if each == column {
            return Point::new(px(x + width / 2.), px(ROW * (line as f32 + 1.5)));
        }
        x += width;
    }
    unreachable!("every column is in the schema")
}

pub struct Grouped;

impl LiveGridDelegate for Grouped {
    type Row = GroupedRow;
}

/// An `xsmall` grid of [`GroupedRow`]s in a window and its events.
pub struct GroupedFixture<D: LiveGridDelegate<Row = GroupedRow> = Grouped> {
    pub window: AnyWindowHandle,
    pub grid: Entity<LiveGridState<D>>,
    pub events: EventLog<GroupedRow>,
}

/// An `xsmall` grid of `rows` ([`grouped_schema`]), configured by
/// `configure`.
pub fn open_grouped(
    cx: &mut TestAppContext,
    rows: Vec<GroupedRow>,
    configure: impl FnOnce(LiveGridState<Grouped>) -> LiveGridState<Grouped> + 'static,
) -> GroupedFixture {
    open_grouped_with(cx, rows, Grouped, configure)
}

/// [`open_grouped`] with another delegate, such as one that records its
/// hooks.
pub fn open_grouped_with<D: LiveGridDelegate<Row = GroupedRow>>(
    cx: &mut TestAppContext,
    rows: Vec<GroupedRow>,
    delegate: D,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> GroupedFixture<D> {
    let (window, grid, events) = open_grid(
        cx,
        grouped_schema(),
        480.,
        Some(Size::XSmall),
        delegate,
        configure,
    );
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all(rows.into_iter().map(Arc::new));
    let fixture = GroupedFixture {
        window,
        grid,
        events,
    };
    fixture.settle(cx);
    fixture
}

impl<D: LiveGridDelegate<Row = GroupedRow>> GroupedFixture<D> {
    /// Runs ingest, pulls and background jobs, then draws a frame.
    pub fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx, Duration::from_millis(20));
        }
    }

    pub fn frame(&self, cx: &mut TestAppContext, elapsed: Duration) {
        cx.executor().advance_clock(elapsed);
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    }

    pub fn writer(&self, cx: &mut TestAppContext) -> GridWriter<GroupedRow> {
        self.grid.read_with(cx, |grid, _| grid.writer())
    }

    pub fn visible_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| grid.visible_keys())
    }

    /// The shown lines of the grid's projection.
    pub fn lines(&self, cx: &mut TestAppContext) -> Vec<LineKind> {
        self.grid.read_with(cx, |grid, _| lines(grid))
    }

    /// Sends input to the window, then draws a frame.
    pub fn input(&self, cx: &mut TestAppContext, input: impl FnOnce(&mut Window, &mut App)) {
        cx.update_window(self.window, |_, window, cx| input(window, cx))
            .expect("window is open");
    }

    /// The display position of the caption of `path` in the grid's shown
    /// projection.
    pub fn caption_line(&self, cx: &mut TestAppContext, path: &GroupPath) -> Option<usize> {
        self.grid.read_with(cx, |grid, _| caption_line(grid, path))
    }

    /// The grouping, expansion, sort, hold, projection, selection and focus
    /// events so far, in order. Forgets every event.
    pub fn take_seen(&self) -> Vec<GroupSeen> {
        std::mem::take(&mut *self.events.borrow_mut())
            .into_iter()
            .filter_map(|event| match event {
                LiveGridEvent::GroupChange { origin } => Some(GroupSeen::Group(origin)),
                LiveGridEvent::ExpansionChange { target, origin } => {
                    Some(GroupSeen::Expansion(target, origin))
                }
                LiveGridEvent::SortChange { origin, .. } => Some(GroupSeen::Sort(origin)),
                LiveGridEvent::OrderHeldChange { held } => Some(GroupSeen::Held(held)),
                LiveGridEvent::ProjectionChange { .. } => Some(GroupSeen::Projection),
                LiveGridEvent::SelectionChange { origin, .. } => Some(GroupSeen::Selection(origin)),
                LiveGridEvent::FocusChange { origin, .. } => Some(GroupSeen::Focus(origin)),
                _ => None,
            })
            .collect()
    }
}

/// What a grouping test follows of an event ([`GroupedFixture::take_seen`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupSeen {
    Group(GroupOrigin),
    Expansion(Option<ExpandTarget<u32>>, GroupOrigin),
    Sort(SortOrigin),
    Held(bool),
    Projection,
    Selection(SelectionOrigin),
    Focus(SelectionOrigin),
}

/// The path of a group of [`GroupedRow`]s: a text value per level, the top
/// level first, as `[(Instrument, "AAPL"), (Side, "Buy")]`.
pub fn group_path(levels: &[(GroupedColumn, &str)]) -> GroupPath {
    GroupPath::new(levels.iter().map(|(column, value)| {
        (
            column.column_id(),
            OwnedValue::from_cell(CellValue::Text(value)),
        )
    }))
}

/// The display position of the caption of `path` in a grid's shown
/// projection; `None` without that group, or under a collapsed ancestor.
pub fn caption_line<D: LiveGridDelegate>(
    grid: &LiveGridState<D>,
    path: &GroupPath,
) -> Option<usize> {
    let projection = grid.projection();
    let group = projection.groups().find(path)?.id();
    projection.group_line(group)
}

/// What a shown line is: a data row by key, a group caption or footer by
/// path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineKind {
    Data(u32),
    Caption(GroupPath),
    Footer(GroupPath),
}

/// The shown lines of a grid's projection, top to bottom. A data row
/// removed since the projection was built is left out.
pub fn lines<D: LiveGridDelegate<Row = GroupedRow>>(grid: &LiveGridState<D>) -> Vec<LineKind> {
    let (projection, snapshot) = (grid.projection(), grid.snapshot());
    let path = |group| {
        projection
            .groups()
            .get(group)
            .expect("a shown group is in the table")
            .path()
            .clone()
    };
    (0..projection.len())
        .filter_map(|ix| match projection.line(ix)? {
            DisplayRow::Data { row, .. } => snapshot.row(row).map(|row| LineKind::Data(row.id)),
            DisplayRow::GroupCaption { group, .. } => Some(LineKind::Caption(path(group))),
            DisplayRow::GroupFooter { group, .. } => Some(LineKind::Footer(path(group))),
            line => panic!("an unknown line {line:?}"),
        })
        .collect()
}

/// The window position of the middle of the disclosure chevron on screen
/// line `line` of an `xsmall` [`GroupedFixture`]: a group caption's at
/// `depth`, or a tree row's at `depth` in the first column. Chevrons line
/// up from the body's left edge: 6 px of cell padding, 16 px per level,
/// then the 20 px lane.
pub fn chevron_point(line: usize, depth: u16) -> Point<Pixels> {
    Point::new(
        px(6. + 16. * f32::from(depth) + 10.),
        px(ROW * (line as f32 + 1.5)),
    )
}

/// The window position of a group caption's text on screen line `line`,
/// clear of its chevron and of the aggregate cells (over the Instrument
/// column).
pub fn caption_point(line: usize) -> Point<Pixels> {
    Point::new(px(100.), px(ROW * (line as f32 + 1.5)))
}

/// Presses the disclosure chevron of the group caption or tree row at
/// `depth` on screen line `line` with `modifiers` held, such as
/// `Modifiers::alt()` for everything below it too.
pub fn press_chevron(
    window: &mut Window,
    line: usize,
    depth: u16,
    modifiers: Modifiers,
    cx: &mut App,
) {
    press_at(
        window,
        chevron_point(line, depth),
        MouseButton::Left,
        modifiers,
        1,
        cx,
    );
}

// Totals and group footers.

/// The hash the grid gives a tagged identity in element ids (`FxHasher`).
fn tagged_hash(value: &impl Hash) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

/// The hash of the totals row at `edge` in element ids, as the grid makes
/// it: its accessibility row and its cells' tooltip divs.
pub fn totals_hash(edge: PinnedEdge) -> u64 {
    let band = match edge {
        PinnedEdge::Top => "top",
        PinnedEdge::Bottom => "bottom",
    };
    tagged_hash(&("live-grid-totals", band))
}

/// The hash of the footer of the group at `path` in element ids.
pub fn footer_hash(path: &GroupPath) -> u64 {
    tagged_hash(&("live-grid-group-footer", path))
}

/// The element id of the accessibility row of the totals row at `edge`.
pub fn totals_row_id(edge: PinnedEdge) -> ElementId {
    ElementId::NamedInteger("live-grid-totals-row".into(), totals_hash(edge))
}

/// The element id of the accessibility row of the footer of `path`.
pub fn footer_row_id(path: &GroupPath) -> ElementId {
    ElementId::NamedInteger("live-grid-group-footer-row".into(), footer_hash(path))
}

/// The accessibility rows of the last frame of `window` (planned with
/// `set_a11y_forced`), top to bottom: the last part of their element ids
/// and their top edges. The header row comes first.
pub fn a11y_row_tops(cx: &mut TestAppContext, window: AnyWindowHandle) -> Vec<(ElementId, Pixels)> {
    cx.update_window(window, |_, window, _| {
        let mut rows: Vec<(ElementId, Pixels)> = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|element| element.role() == Some(Role::Row))
            .map(|element| {
                (
                    element.path().last().cloned().expect("an id"),
                    element.bounds().origin.y,
                )
            })
            .collect();
        rows.sort_by(|a, b| a.1.partial_cmp(&b.1).expect("finite"));
        rows
    })
    .expect("window is open")
}

/// The labels of the visible items of the open menus of `window`, top to
/// bottom.
pub fn menu_labels(cx: &mut TestAppContext, window: AnyWindowHandle) -> Vec<String> {
    cx.update_window(window, |_, window, _| {
        let mut items: Vec<_> = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|item| item.role() == Some(Role::MenuItem) && item.visible())
            .collect();
        items.sort_by(|a, b| {
            (a.path().len(), a.bounds().origin.y)
                .partial_cmp(&(b.path().len(), b.bounds().origin.y))
                .expect("finite")
        });
        items
            .iter()
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    })
    .expect("window is open")
}

// A grid of orders with every kind of value (EditFixture).

/// The columns of an [`Order`]: one of every kind of cell value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderColumn {
    /// `Int`.
    Id,
    /// `Text`.
    Instrument,
    /// `Text`.
    Side,
    /// `Int`.
    Quantity,
    /// `Float`.
    Price,
    /// `Icon`.
    Status,
    /// `Timestamp`.
    Placed,
    /// `Bool`.
    Active,
    /// `Text`.
    Note,
}

impl GridColumns for OrderColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Id,
            Self::Instrument,
            Self::Side,
            Self::Quantity,
            Self::Price,
            Self::Status,
            Self::Placed,
            Self::Active,
            Self::Note,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Instrument => "instrument",
            Self::Side => "side",
            Self::Quantity => "quantity",
            Self::Price => "price",
            Self::Status => "status",
            Self::Placed => "placed",
            Self::Active => "active",
            Self::Note => "note",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// Every order column with its width; the window is wide enough for all.
pub const ORDER_COLUMNS: [(OrderColumn, f32); 9] = [
    (OrderColumn::Id, 50.),
    (OrderColumn::Instrument, 90.),
    (OrderColumn::Side, 50.),
    (OrderColumn::Quantity, 70.),
    (OrderColumn::Price, 80.),
    (OrderColumn::Status, 80.),
    (OrderColumn::Placed, 150.),
    (OrderColumn::Active, 60.),
    (OrderColumn::Note, 120.),
];

/// The width of an [`EditFixture`]'s window.
pub const ORDER_WINDOW_WIDTH: f32 = 800.;

/// The status of an order: an icon with its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderStatus {
    Working,
    Filled,
    Cancelled,
}

impl OrderStatus {
    /// The icon's asset path and the text that stands for it.
    pub fn icon(self) -> IconValue<'static> {
        match self {
            Self::Working => IconValue::new("icons/clock.svg", "Working"),
            Self::Filled => IconValue::new("icons/check.svg", "Filled"),
            Self::Cancelled => IconValue::new("icons/x.svg", "Cancelled"),
        }
    }
}

/// An order: a row with every kind of value.
#[derive(Clone, Debug)]
pub struct Order {
    pub id: u32,
    pub instrument: &'static str,
    pub side: Side,
    pub quantity: i64,
    pub price: f64,
    pub status: OrderStatus,
    pub placed: DateTime<Utc>,
    pub active: bool,
    pub note: Arc<str>,
}

impl GridRow for Order {
    type Key = u32;
    type Column = OrderColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: OrderColumn) -> CellValue<'_> {
        match column {
            OrderColumn::Id => CellValue::Int(self.id.into()),
            OrderColumn::Instrument => CellValue::Text(self.instrument),
            OrderColumn::Side => CellValue::Text(self.side.as_str()),
            OrderColumn::Quantity => CellValue::Int(self.quantity),
            OrderColumn::Price => CellValue::Float(self.price),
            OrderColumn::Status => CellValue::Icon(self.status.icon()),
            OrderColumn::Placed => CellValue::Timestamp(self.placed),
            OrderColumn::Active => CellValue::Bool(self.active),
            OrderColumn::Note => CellValue::Text(&self.note),
        }
    }
}

/// `count` orders (60 in the stage 3 tests), the same every run: ids from
/// 1, instruments, sides and statuses in turn, placed a minute apart from
/// 2026-11-04 09:00 UTC.
pub fn orders(count: u32) -> Vec<Order> {
    const INSTRUMENTS: [&str; 4] = ["AAPL", "MSFT", "SBER", "GAZP"];
    const STATUSES: [OrderStatus; 3] = [
        OrderStatus::Working,
        OrderStatus::Filled,
        OrderStatus::Cancelled,
    ];
    let open = Utc
        .with_ymd_and_hms(2026, 11, 4, 9, 0, 0)
        .single()
        .expect("a valid time");
    (1..=count)
        .map(|id| Order {
            id,
            instrument: INSTRUMENTS[id as usize % INSTRUMENTS.len()],
            side: if id % 2 == 0 { Side::Buy } else { Side::Sell },
            quantity: i64::from(id) * 100,
            price: 100. + f64::from(id) / 4.,
            status: STATUSES[id as usize % STATUSES.len()],
            placed: open + chrono::Duration::minutes(i64::from(id)),
            active: id % 3 != 0,
            note: format!("Order {id}").into(),
        })
        .collect()
}

/// Every order column with its title and width.
pub fn order_schema() -> GridSchema<OrderColumn> {
    ORDER_COLUMNS
        .into_iter()
        .fold(GridSchema::new(), |schema, (column, width)| {
            schema.column(GridColumn::new(column, column.id()).with_width(px(width)))
        })
}

/// The element id of the row element of the order `key`: its
/// accessibility row's, and the id scope of its cell hosts.
pub fn order_row_id(key: u32) -> ElementId {
    ElementId::NamedInteger("live-grid-row".into(), tagged_hash(&key))
}

/// The element id of the row element of the order `key` while its removed
/// version fades out: it holds the hosts of its element cells.
pub fn fading_row_id(key: u32) -> ElementId {
    ElementId::NamedInteger("live-grid-fading-row".into(), tagged_hash(&key))
}

/// The element id of a cell host of `column` in its row's id scope:
/// `name` is `live-grid-cell` for a widget, `live-grid-editor` for an editor.
pub fn cell_host_id(name: &'static str, column: OrderColumn) -> ElementId {
    ElementId::NamedChild(Arc::new(ElementId::Name(name.into())), column.id().into())
}

/// The window position of the centre of a body cell of an `xsmall`
/// [`EditFixture`]: `line` counts the shown lines from the top.
pub fn order_cell(line: usize, column: OrderColumn) -> Point<Pixels> {
    let mut x = 0.;
    for (each, width) in ORDER_COLUMNS {
        if each == column {
            return Point::new(px(x + width / 2.), px(ROW * (line as f32 + 1.5)));
        }
        x += width;
    }
    unreachable!("every column is in the schema")
}

pub struct Orders;

impl LiveGridDelegate for Orders {
    type Row = Order;
}

/// An `xsmall` grid of [`Order`]s in a window, and its events: edit events
/// among them.
pub struct EditFixture<D: LiveGridDelegate<Row = Order> = Orders> {
    pub window: AnyWindowHandle,
    pub grid: Entity<LiveGridState<D>>,
    pub events: EventLog<Order>,
}

/// An `xsmall` grid of `rows` ([`order_schema`]), configured by
/// `configure`: with editors, edit gestures and policies.
pub fn open_orders(
    cx: &mut TestAppContext,
    rows: Vec<Order>,
    configure: impl FnOnce(LiveGridState<Orders>) -> LiveGridState<Orders> + 'static,
) -> EditFixture {
    open_orders_with(cx, rows, Orders, configure)
}

/// [`open_orders`] with another delegate, such as one with edit hooks.
pub fn open_orders_with<D: LiveGridDelegate<Row = Order>>(
    cx: &mut TestAppContext,
    rows: Vec<Order>,
    delegate: D,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> EditFixture<D> {
    open_orders_in(cx, order_schema(), rows, delegate, configure)
}

/// [`open_orders_with`] with another schema of the order columns, such as
/// one with frozen columns or `Element` and `Control` columns.
pub fn open_orders_in<D: LiveGridDelegate<Row = Order>>(
    cx: &mut TestAppContext,
    schema: GridSchema<OrderColumn>,
    rows: Vec<Order>,
    delegate: D,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> EditFixture<D> {
    let (window, grid, events) = open_grid(
        cx,
        schema,
        ORDER_WINDOW_WIDTH,
        Some(Size::XSmall),
        delegate,
        configure,
    );
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all(rows.into_iter().map(Arc::new));
    let fixture = EditFixture {
        window,
        grid,
        events,
    };
    fixture.settle(cx);
    fixture
}

impl<D: LiveGridDelegate<Row = Order>> EditFixture<D> {
    /// Runs ingest, pulls and background jobs, then draws a frame.
    pub fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx, Duration::from_millis(20));
        }
    }

    pub fn frame(&self, cx: &mut TestAppContext, elapsed: Duration) {
        cx.executor().advance_clock(elapsed);
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    }

    pub fn writer(&self, cx: &mut TestAppContext) -> GridWriter<Order> {
        self.grid.read_with(cx, |grid, _| grid.writer())
    }

    pub fn visible_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| grid.visible_keys())
    }

    /// The selected keys, in ascending order.
    pub fn selected_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| {
            let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
            keys.sort_unstable();
            keys
        })
    }

    /// Presses and releases the left button on the cell of `column` on
    /// shown line `line`, `click_count` 2 for the second press of a double
    /// click.
    pub fn press_cell(
        &self,
        cx: &mut TestAppContext,
        line: usize,
        column: OrderColumn,
        click_count: usize,
    ) {
        cx.update_window(self.window, |_, window, cx| {
            press_at(
                window,
                order_cell(line, column),
                MouseButton::Left,
                Modifiers::default(),
                click_count,
                cx,
            );
        })
        .expect("window is open");
    }

    /// Types `text` into the focused element, as the keyboard would, then
    /// draws a frame.
    pub fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        cx.update_window(self.window, |_, window, cx| type_text(window, text, cx))
            .expect("window is open");
    }

    /// Presses a key by its keystroke (`"f2"`, `"enter"`, `"shift-tab"`),
    /// then draws a frame.
    pub fn press_key(&self, cx: &mut TestAppContext, keystroke: &str) {
        cx.update_window(self.window, |_, window, cx| {
            window.press(keystroke, cx);
            window.render_frame(cx);
        })
        .expect("window is open");
    }

    /// The events so far; forgets them.
    pub fn take_events(&self) -> Vec<LiveGridEvent<u32, OrderColumn>> {
        self.events.take()
    }
}

/// Types `text` into the focused element of `window`, as the keyboard
/// would, then draws a frame.
pub fn type_text(window: &mut Window, text: &str, cx: &mut App) {
    window.input(text, cx);
    window.render_frame(cx);
}

// Editors on the order columns, edit hooks that record their calls, and
// what the open editor shows.

/// The edit hooks of an [`EditFixture`]: the orders `is_editable` refuses
/// (filled ones), the highest price `validate_edit` takes, and every hook
/// call in order.
#[derive(Clone, Default)]
pub struct EditHooks {
    pub filled: Rc<RefCell<Vec<u32>>>,
    pub max_price: Rc<Cell<Option<f64>>>,
    pub calls: Rc<RefCell<Vec<&'static str>>>,
}

impl LiveGridDelegate for EditHooks {
    type Row = Order;

    fn is_editable(&self, cell: &EditableContext<'_, Order>, _: &App) -> bool {
        self.calls.borrow_mut().push("is_editable");
        !self.filled.borrow().contains(cell.key())
    }

    fn validate_edit(
        &self,
        edit: &EditValidateContext<'_, Order>,
        _: &App,
    ) -> Result<(), EditError> {
        self.calls.borrow_mut().push("validate_edit");
        match (edit.column(), edit.value(), self.max_price.get()) {
            (OrderColumn::Price, OwnedValue::Float(price), Some(max)) if *price > max => {
                Err(EditError::new(format!("Enter a price up to {max}")))
            }
            _ => Ok(()),
        }
    }
}

/// The editors of the order columns: text for the instrument and the
/// note, numbers for the quantity and the price; the other columns are
/// read-only.
pub fn with_order_editors<D: LiveGridDelegate<Row = Order>>(
    grid: LiveGridState<D>,
) -> LiveGridState<D> {
    grid.with_editor(OrderColumn::Instrument, TextEditor::new())
        .with_editor(OrderColumn::Note, TextEditor::new())
        .with_editor(OrderColumn::Quantity, NumberEditor::new())
        .with_editor(OrderColumn::Price, NumberEditor::new())
}

/// 60 orders with [`with_order_editors`] and recording [`EditHooks`],
/// configured further by `configure`.
pub fn open_editable(
    cx: &mut TestAppContext,
    configure: impl FnOnce(LiveGridState<EditHooks>) -> LiveGridState<EditHooks> + 'static,
) -> EditFixture<EditHooks> {
    open_orders_with(cx, orders(60), EditHooks::default(), move |grid| {
        configure(with_order_editors(grid))
    })
}

impl<D: LiveGridDelegate<Row = Order>> EditFixture<D> {
    /// Gives the grid the keyboard focus on the cell of `key` and `column`.
    pub fn focus_cell(&self, cx: &mut TestAppContext, key: u32, column: OrderColumn) {
        cx.update_window(self.window, |_, window, cx| {
            let handle = self.grid.focus_handle(cx);
            window.focus(&handle, cx);
        })
        .expect("window is open");
        self.grid.update(cx, |grid, cx| {
            assert!(
                grid.set_focused_cell(&key, Some(column), cx),
                "{key} is shown"
            );
        });
        self.settle(cx);
    }

    /// The focused cell: its key and column.
    pub fn focused(&self, cx: &mut TestAppContext) -> Option<(u32, OrderColumn)> {
        self.grid.read_with(cx, |grid, _| {
            let focus = grid.focused_cell()?;
            Some((*focus.row()?, focus.column()))
        })
    }

    /// The open editor's cell.
    pub fn editing(&self, cx: &mut TestAppContext) -> Option<(u32, OrderColumn)> {
        self.grid.read_with(cx, |grid, _| {
            grid.editing_cell().map(|(key, column)| (*key, column))
        })
    }

    /// The text of the open editor's field, as its accessibility value
    /// gives it.
    pub fn editor_text(&self, cx: &mut TestAppContext) -> Option<String> {
        self.editor_input(cx)
            .map(|input| input.value().unwrap_or_default().to_string())
    }

    /// The open editor's field, observed in the last frame.
    pub fn editor_input(
        &self,
        cx: &mut TestAppContext,
    ) -> Option<gpui_kit::base::test_support::ElementSnapshot> {
        let host = ElementId::Name("live-grid-editor".into());
        cx.update_window(self.window, |_, window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .find(|element| {
                    element.role() == Some(Role::TextInput)
                        && element.path().iter().any(
                            |id| matches!(id, ElementId::NamedChild(name, _) if **name == host),
                        )
                })
        })
        .expect("window is open")
    }
}

// Two grids of items side by side, rows dragged between them
// (`open_pair`), and an element of the window that drags a `Tag`.

/// The width of each grid of a [`Pair`].
pub const PAIR_GRID_WIDTH: f32 = 400.;

/// A value of the application's own that the window's `"tag"` element drags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag(pub u32);

struct PairRoot {
    a: Entity<LiveGridState<Items>>,
    b: Entity<LiveGridState<Items>>,
}

impl Render for PairRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let grid = |state: &Entity<LiveGridState<Items>>| {
            div()
                .w(px(PAIR_GRID_WIDTH))
                .h_full()
                .child(LiveGrid::new(state).with_size(Size::XSmall).bordered(false))
        };
        div()
            .size_full()
            .flex()
            .child(grid(&self.a))
            .child(grid(&self.b))
            // Over grid A's header: it drags a `Tag(7)`.
            .child(
                div()
                    .id("tag")
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(24.))
                    .h(px(12.))
                    .occlude()
                    .on_drag(Tag(7), |_, _, _, cx| cx.new(|_| gpui_kit::Empty)),
            )
    }
}

/// Two `xsmall` grids of items side by side in one window, A on the left
/// and B on the right, and the logs of their events.
pub struct Pair {
    pub window: AnyWindowHandle,
    pub a: Entity<LiveGridState<Items>>,
    pub b: Entity<LiveGridState<Items>>,
    pub events_a: EventLog<Item>,
    pub events_b: EventLog<Item>,
}

/// A [`Pair`]: `rows_a` and `rows_b` items (`price = id * 10`), each grid
/// configured by its closure.
pub fn open_pair(
    cx: &mut TestAppContext,
    rows_a: u32,
    rows_b: u32,
    configure_a: impl FnOnce(LiveGridState<Items>) -> LiveGridState<Items> + 'static,
    configure_b: impl FnOnce(LiveGridState<Items>) -> LiveGridState<Items> + 'static,
) -> Pair {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let schema = || {
        GridSchema::new()
            .column(GridColumn::new(ItemColumn::Id, "Id"))
            .column(GridColumn::new(ItemColumn::Price, "Price"))
    };
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(PAIR_GRID_WIDTH * 2.), px(WINDOW_HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| PairRoot {
                        a: cx.new(|cx| {
                            configure_a(LiveGridState::new_standalone(
                                schema(),
                                Items,
                                StoreOptions::default(),
                                cx,
                            ))
                        }),
                        b: cx.new(|cx| {
                            configure_b(LiveGridState::new_standalone(
                                schema(),
                                Items,
                                StoreOptions::default(),
                                cx,
                            ))
                        }),
                    })
                },
            )
        })
        .expect("open window");
    let (a, b) = root.read_with(cx, |root, _| (root.a.clone(), root.b.clone()));
    let log = |cx: &mut TestAppContext, grid: &Entity<LiveGridState<Items>>| {
        let events: EventLog<Item> = Rc::default();
        cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(grid, move |_, event: &LiveGridEvent<_, _>, _| {
                events.borrow_mut().push(event.clone());
            })
            .detach();
        });
        events
    };
    let (events_a, events_b) = (log(cx, &a), log(cx, &b));
    for (grid, count) in [(&a, rows_a), (&b, rows_b)] {
        let writer = grid.read_with(cx, |grid, _| grid.writer());
        writer.replace_all((0..count).map(|id| {
            Arc::new(Item {
                id,
                price: i64::from(id) * 10,
            })
        }));
    }
    let pair = Pair {
        window,
        a,
        b,
        events_a,
        events_b,
    };
    pair.settle(cx);
    pair
}

impl Pair {
    /// Runs ingest, pulls and background jobs, then draws a frame.
    pub fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx, Duration::from_millis(20));
        }
    }

    pub fn frame(&self, cx: &mut TestAppContext, elapsed: Duration) {
        cx.executor().advance_clock(elapsed);
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    }

    /// The centre of line `line` of grid A's rows (`b`: grid B's).
    pub fn line(&self, b: bool, line: usize) -> Point<Pixels> {
        let x = if b { PAIR_GRID_WIDTH + 100. } else { 100. };
        Point::new(px(x), px(ROW * (line as f32 + 1.5)))
    }

    pub fn down(&self, cx: &mut TestAppContext, at: Point<Pixels>) {
        cx.update_window(self.window, |_, window, cx| {
            mouse_down_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx)
        })
        .expect("window is open");
    }

    pub fn drag_to(&self, cx: &mut TestAppContext, at: Point<Pixels>) {
        cx.update_window(self.window, |_, window, cx| drag_move_to(window, at, cx))
            .expect("window is open");
    }

    pub fn up(&self, cx: &mut TestAppContext, at: Point<Pixels>) {
        cx.update_window(self.window, |_, window, cx| {
            mouse_up_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx)
        })
        .expect("window is open");
    }

    /// Clicks at a window position with `modifiers`.
    pub fn click(&self, cx: &mut TestAppContext, at: Point<Pixels>, modifiers: Modifiers) {
        cx.update_window(self.window, |_, window, cx| {
            click_with(window, at, modifiers, cx)
        })
        .expect("window is open");
    }

    /// Selects the rows of grid A on `lines`: the first click plain, the
    /// others with the platform toggle; then starts dragging them from the
    /// first line, past the gesture's threshold, to `to`.
    pub fn drag_rows(&self, cx: &mut TestAppContext, lines: &[usize], to: Point<Pixels>) {
        for (ix, line) in lines.iter().enumerate() {
            let modifiers = if ix == 0 {
                Modifiers::default()
            } else {
                Modifiers::secondary_key()
            };
            self.click(cx, self.line(false, *line), modifiers);
        }
        self.settle(cx);
        let from = self.line(false, lines[0]);
        self.down(cx, from);
        self.drag_to(cx, Point::new(from.x, from.y + px(12.)));
        self.drag_to(cx, Point::new(from.x + px(20.), from.y + px(12.)));
        self.drag_to(cx, to);
    }

    /// The `Drop` events of a log: source rows' keys (or the payload's
    /// `Tag`), and targets.
    pub fn drops(events: &EventLog<Item>) -> Vec<(DroppedSource, DropTarget<u32>)> {
        events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::Drop { source, target, .. } => Some((
                    match source {
                        DropSource::Rows(drag) => {
                            DroppedSource::Rows(drag.keys().to_vec(), drag.grid())
                        }
                        DropSource::Payload(payload) => {
                            DroppedSource::Tag(payload.data::<Tag>().cloned())
                        }
                        _ => DroppedSource::Other,
                    },
                    target.clone(),
                )),
                _ => None,
            })
            .collect()
    }
}

/// What a `Drop` brought, as a test compares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DroppedSource {
    Rows(Vec<u32>, gpui_kit::EntityId),
    Tag(Option<Tag>),
    Other,
}
