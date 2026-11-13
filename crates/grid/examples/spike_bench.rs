//! Spike benchmark: what the grid costs per frame and in the background under
//! live loads.
//!
//! Renders the grid in a headless window with the real macOS text system and
//! the headless Metal renderer and drives the benchmark scenarios (S1, S2…).
//!
//! How frames are measured: in GPUI's test mode a dirty window is drawn at the
//! end of every update, exactly once, the way the display link would draw it.
//! A transparent `Timed` element around the grid (or the `DataTable`) records
//! the main-thread time of every such draw of its subtree: view render,
//! layout, prepaint and paint. Work outside the subtree (scene sorting, hit
//! test and accessibility trees) is measured separately by forcing frames and
//! comparing the whole draw with the subtree; it is reported as "overhead".
//! Background work (ingest, rebuilds) runs on the deterministic test
//! dispatcher between frames and is measured separately on real threads.
//!
//! ```sh
//! cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
//! SPIKE_SCREENSHOT=target/spike/s1.png SPIKE_SCREENSHOT_TEXT=target/spike/text.png \
//!   cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
//! # S24 alone: the update rate sweep
//! SPIKE_S24_ONLY=1 cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
//! # Typed cells, widgets and images alone: S1, S2 and S3 to compare with,
//! # then S25, S26, S26b and S29
//! SPIKE_CELLS_ONLY=1 cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
//! # Painting and grouping alone: S1, S20, S2, S26, S16, S16b, S21 and the
//! # grouped rebuilds (S16–S18, S21) on real threads
//! SPIKE_PAINT_GROUP_ONLY=1 cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
//! # Pending edits alone: S2 to compare with, then S27 and S28
//! SPIKE_EDIT_ONLY=1 cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
//! # Row dragging alone: S2 to compare with, then S38 and S39
//! SPIKE_DRAG_ONLY=1 cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
//! # S42 alone: the keyed storm on real threads, about 13 minutes
//! # (SPIKE_S42_SECONDS sets the seconds per cell, 10 by default;
//! # SPIKE_S42_CELLS=uniform:100000,zipf:1000,new runs only those groups)
//! SPIKE_S42_ONLY=1 cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
//! ```

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::{Cell, RefCell},
    io::Write as _,
    rc::Rc,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gpui_kit::base::actions::{Cancel, SelectPageDown};
use gpui_kit::{
    Action, AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Element, ElementId,
    Entity, Focusable as _, GlobalElementId, HeadlessAppContext, InspectorElementId, IntoElement,
    LayoutId, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, SharedString, Styled as _, Window, WindowBounds,
    WindowOptions,
    component::{
        ActiveTheme as _, Sizable as _,
        checkbox::Checkbox,
        progress::Progress,
        table::{Column, DataTable, TableDelegate, TableState},
    },
    div, point, px, size,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    Aggregate, AssetImageSource, CellContent, CellEditor, CellElementContext, CellStyle,
    CellStyleContext, CellValue, ChangePaint, ColumnBand, ColumnIx, ColumnSet, ColumnZone,
    ContainsAllTerms, CsvFormat, DirectionalChanges, DisplayRow, DropOnRows, EditId, EditorOpen,
    EditorStart, EveryGroupFooter, ExpandDepth, ExpandTarget, Expansion, ExportError, ExportJob,
    ExportOutcome, ExportRequest, ExportScope, GridColumn, GridColumns, GridProgress, GridRow,
    GridSchema, GroupBy, GroupKey, HighlightPolicy, IconValue, ImageValue, Ingest, LiveGrid,
    LiveGridDelegate, LiveGridEvent, LiveGridHandle, LiveGridState, LiveReorder, Op, OwnedValue,
    PinnedEdge, Projection, ProjectionEngine, QuickFilter, Rebuild, SelectedRowsDrag,
    SelectionMode, SortDirection, StoreOptions, StoreSnapshot, TextEditor, TreeSpec, ViewSpec,
    WriteCoalescing, actions::SelectAll,
};
use rustc_hash::{FxHashMap, FxHashSet};

// `println!` panics on a closed stdout (`| head`), aborting the GPUI run: ignore write errors.
macro_rules! out {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}

const COLUMNS: usize = 40;
const ROWS: u64 = 100_000;
const FRAMES: usize = 300;
const FRAME: Duration = Duration::from_millis(16);
const WINDOW_WIDTH: f32 = 2560.;
const WINDOW_HEIGHT: f32 = 1440.;

// ---------------------------------------------------------------------------
// Allocations of the calling thread, so S11 can show that a quick filter
// rebuild does not allocate per row. A thread-local counter: no contention
// with the other threads, and constant-initialized, so counting never
// allocates itself.

struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count_allocation() {
    let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
}

/// Allocations made by this thread so far.
fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_allocation();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// ---------------------------------------------------------------------------
// Data: 1 id, 20 prices, 10 sizes, 9 text columns. Displayed as id, the text
// columns, then prices and sizes, so every frame paints text cells.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Col(u16);

static ALL_COLUMNS: [Col; COLUMNS] = {
    let mut all = [Col(0); COLUMNS];
    let mut ix = 0;
    while ix < COLUMNS {
        all[ix] = Col(ix as u16);
        ix += 1;
    }
    all
};

static COLUMN_IDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    (0..COLUMNS)
        .map(|ix| &*Box::leak(format!("c{ix}").into_boxed_str()))
        .collect()
});

impl GridColumns for Col {
    fn all() -> &'static [Self] {
        &ALL_COLUMNS
    }

    fn id(self) -> &'static str {
        COLUMN_IDS[self.0 as usize]
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self.0)
    }
}

const SIDE: usize = 32;
const COMMENT: usize = 39;

/// Schema columns in display order.
fn display_order() -> Vec<usize> {
    std::iter::once(0).chain(31..40).chain(1..31).collect()
}

/// Frozen on the right: the last two size columns.
const RIGHT_ZONE: [usize; 2] = [29, 30];
/// S26: a progress bar (`Element`) in "Price 7", on screen after the
/// texts, and a checkbox (`Control`) in "Size 9", frozen at the right edge.
const PROGRESS: usize = 7;
const CHECKBOX: usize = 29;
/// Hidden: the order id text column.
const HIDDEN: usize = 38;
/// Under the "Prices" band: the first six price columns.
const BANDED: std::ops::RangeInclusive<usize> = 1..=6;
/// The quick filter columns of S11 and S11b: five text columns (instrument,
/// side, book, counterparty, trader) and five numbers (three prices, two
/// sizes).
const QUICK_FILTER_COLUMNS: [u16; 10] = [31, 32, 33, 34, 35, 1, 2, 3, 21, 22];
/// The text column S11 and S11b sort by: the counterparty.
const COUNTERPARTY: u16 = 34;

fn quick_filter_columns() -> ColumnSet {
    QUICK_FILTER_COLUMNS
        .iter()
        .fold(ColumnSet::EMPTY, |mut set, ix| {
            set.insert(ColumnIx::new(*ix));
            set
        })
}

#[derive(Clone)]
struct Quote {
    id: u64,
    prices: [f64; 20],
    sizes: [i64; 10],
    texts: [Arc<str>; 9],
}

impl GridRow for Quote {
    type Key = u64;
    type Column = Col;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: Col) -> CellValue<'_> {
        match column.0 as usize {
            0 => CellValue::Int(self.id as i64),
            ix @ 1..=20 => CellValue::Float(self.prices[ix - 1]),
            ix @ 21..=30 => CellValue::Int(self.sizes[ix - 21]),
            STATUS if TYPED_CELLS.load(Ordering::Relaxed) => {
                // S25: the status by the first price, so it changes with it.
                let (path, text) = STATUS_ICONS[(self.prices[0] * 10.) as usize % 5];
                CellValue::Icon(IconValue::new(path, text))
            }
            BOOK if IMAGE_CELLS.load(Ordering::Relaxed) => {
                // S29: one of 16 images by the row, with the book's name.
                let ids = &*IMAGE_IDS;
                CellValue::Image(ImageValue::new(
                    &ids[self.id as usize % ids.len()],
                    &self.texts[BOOK - 31],
                ))
            }
            TIME if TYPED_CELLS.load(Ordering::Relaxed) => CellValue::Timestamp(
                chrono::DateTime::from_timestamp(1_762_259_696 + self.sizes[9] % 86_400, 0)
                    .unwrap_or_default(),
            ),
            ix => CellValue::Text(&self.texts[ix - 31]),
        }
    }
}

/// S25: the status column ("Text 6") shows an icon with its text, the
/// venue column ("Text 7") a time, while set.
static TYPED_CELLS: AtomicBool = AtomicBool::new(false);
/// S29: the book column ("Text 3") shows an image with the book's name,
/// while set.
static IMAGE_CELLS: AtomicBool = AtomicBool::new(false);
const BOOK: usize = 33;
/// S29's images: 16 SVGs of 32 × 32 px in 16 colours, written to the
/// temporary directory once and loaded as files (rasters once decoded).
static IMAGE_IDS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let dir = std::env::temp_dir().join("gpui-kit-grid-s29");
    std::fs::create_dir_all(&dir).expect("a directory for S29's images");
    (0..16)
        .map(|ix| {
            let path = dir.join(format!("image-{ix:02}.svg"));
            let svg = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="hsl({}, 70%, 50%)"/><circle cx="16" cy="16" r="10" fill="#ffffff"/></svg>"##,
                ix * 360 / 16
            );
            std::fs::write(&path, svg).expect("an S29 image");
            path.to_str().expect("a UTF-8 path").to_owned()
        })
        .collect()
});
const STATUS: usize = 36;
const TIME: usize = 37;
/// The icons of S25's statuses (gpui-kit's) and their texts.
const STATUS_ICONS: [(&str, &str); 5] = [
    ("icons/circle-check.svg", "Filled"),
    ("icons/circle-x.svg", "Rejected"),
    ("icons/triangle-alert.svg", "Partial"),
    ("icons/info.svg", "New"),
    ("icons/loader-circle.svg", "Working"),
];

struct Pools {
    pools: Vec<Vec<Arc<str>>>,
}

impl Pools {
    fn new() -> Self {
        let pool = |count: usize, make: &dyn Fn(usize) -> String| -> Vec<Arc<str>> {
            (0..count).map(|ix| Arc::from(make(ix))).collect()
        };
        Self {
            pools: vec![
                pool(200, &|ix| format!("INS{ix:03}")),
                pool(2, &|ix| {
                    if ix == 0 { "Buy".into() } else { "Sell".into() }
                }),
                pool(20, &|ix| format!("BOOK-{ix:02}")),
                pool(500, &|ix| format!("Counterparty {ix}")),
                pool(50, &|ix| format!("trader{ix}")),
                pool(5, &|ix| {
                    ["New", "Filled", "Partial", "Cancelled", "Rejected"][ix].into()
                }),
                pool(8, &|ix| format!("VENUE{ix}")),
                Vec::new(), // order id: unique per row
                pool(30, &|ix| {
                    format!("Comment {ix}: allocation pending confirmation from the middle office")
                }),
            ],
        }
    }

    fn row(&self, rng: &mut fastrand::Rng, id: u64) -> Quote {
        let texts = std::array::from_fn(|ix| {
            let pool = &self.pools[ix];
            if pool.is_empty() {
                Arc::from(format!("ORD{id:08}"))
            } else {
                pool[rng.usize(..pool.len())].clone()
            }
        });
        Quote {
            id,
            prices: std::array::from_fn(|_| rng.f64() * 1_000.),
            sizes: std::array::from_fn(|_| rng.i64(0..1_000_000)),
            texts,
        }
    }
}

fn mutate(rng: &mut fastrand::Rng, quote: &mut Quote) {
    for _ in 0..rng.usize(1..=3) {
        let ix = rng.usize(..20);
        quote.prices[ix] *= 1. + (rng.f64() - 0.5) / 100.;
    }
    if rng.u8(..4) == 0 {
        quote.sizes[rng.usize(..10)] += rng.i64(-100..100);
    }
}

// ---------------------------------------------------------------------------
// Frame timing: a transparent element around the measured subtree.

type Samples = Rc<RefCell<Vec<Duration>>>;

struct Timed {
    child: AnyElement,
    samples: Samples,
    started: Option<Instant>,
}

impl Timed {
    fn new(child: impl IntoElement, samples: Samples) -> Self {
        Self {
            child: child.into_any_element(),
            samples,
            started: None,
        }
    }
}

impl IntoElement for Timed {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Timed {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // Child views render inside their parent's layout, so this covers
        // render, layout, prepaint and paint of the whole subtree.
        self.started = Some(Instant::now());
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
        if let Some(started) = self.started.take() {
            self.samples.borrow_mut().push(started.elapsed());
        }
    }
}

// ---------------------------------------------------------------------------
// Grid under test.

struct Blotter;

impl LiveGridDelegate for Blotter {
    type Row = Quote;

    fn cell_style(&self, cell: &CellStyleContext<'_, Quote>, style: &mut CellStyle, cx: &App) {
        if cell.column().0 as usize == SIDE && cell.value() == CellValue::Text("Sell") {
            style.set_foreground(cx.theme().blue);
        }
    }

    /// S26: the elements of the columns [`widget_schema`] gives them; no
    /// other schema asks.
    fn render_cell(
        &self,
        cell: &CellElementContext<'_, Quote>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        let quote = cell.row();
        match cell.column().0 as usize {
            PROGRESS => Some(
                Progress::new("filled")
                    .with_size(cell.size())
                    .value((quote.prices[PROGRESS - 1] / 10.) as f32)
                    .into_any_element(),
            ),
            CHECKBOX => Some(
                Checkbox::new("active")
                    .with_size(cell.size())
                    .checked(quote.sizes[CHECKBOX - 21] % 2 == 0)
                    .tab_stop(false)
                    .into_any_element(),
            ),
            _ => None,
        }
    }
}

fn column_title_width(ix: usize) -> (String, f32) {
    match ix {
        0 => ("Id".to_string(), 80.),
        1..=20 => (format!("Price {ix}"), 84.),
        21..=30 => (format!("Size {}", ix - 20), 84.),
        COMMENT => ("Comment".to_string(), 220.),
        _ => (format!("Text {}", ix - 30), 110.),
    }
}

/// The display order of `display_order`, with this layout: two columns
/// frozen left (Id, Text 1), the last two sizes frozen right, a "Prices"
/// band over the first six prices, and the order id hidden.
fn schema() -> GridSchema<Col> {
    schema_with(|_| CellContent::Text)
}

/// [`schema`] with a progress bar and a checkbox in two columns (S26).
fn widget_schema() -> GridSchema<Col> {
    schema_with(|ix| match ix {
        PROGRESS => CellContent::Element,
        CHECKBOX => CellContent::Control,
        _ => CellContent::Text,
    })
}

/// [`schema`] with what each column paints.
fn schema_with(content: impl Fn(usize) -> CellContent) -> GridSchema<Col> {
    let column = |ix: usize| {
        let (title, width) = column_title_width(ix);
        GridColumn::new(Col(ix as u16), title)
            .with_width(px(width))
            .with_content(content(ix))
            .with_visible(ix != HIDDEN)
            .with_zone(if RIGHT_ZONE.contains(&ix) {
                ColumnZone::FrozenRight
            } else {
                ColumnZone::Scrolling
            })
    };
    display_order()
        .into_iter()
        .fold(GridSchema::new().with_frozen(2), |schema, ix| {
            if ix == *BANDED.start() {
                schema.band(
                    BANDED.fold(ColumnBand::new("prices", "Prices"), |band, ix| {
                        band.column(column(ix))
                    }),
                )
            } else if BANDED.contains(&ix) {
                schema
            } else {
                schema.column(column(ix))
            }
        })
}

struct GridRoot {
    grid: Entity<LiveGridState<Blotter>>,
    samples: Samples,
    /// S22: the group panel shows.
    group_panel: bool,
}

impl Render for GridRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Timed::new(
            div().size_full().child(
                LiveGrid::new(&self.grid)
                    .xsmall()
                    .bordered(false)
                    .group_panel(self.group_panel),
            ),
            self.samples.clone(),
        )
    }
}

// ---------------------------------------------------------------------------
// DataTable baseline: the same data and column order through gpui-kit's
// DataTable, with the same 26 px rows.

struct TableBench {
    rows: Vec<Quote>,
    columns: Vec<Column>,
    order: Vec<usize>,
    /// `Some` for the "fast" delegate: every row's cells formatted once and
    /// refreshed when the row changes (outside the measured draw), so
    /// `render_td` only clones a `SharedString`. `None` for the typical
    /// delegate that formats in `render_td`.
    cache: Option<FxHashMap<usize, Box<[SharedString]>>>,
}

fn format_cell(quote: &Quote, column: Col) -> SharedString {
    match quote.value(column) {
        CellValue::Int(value) => value.to_string().into(),
        CellValue::Float(value) => format!("{value:.2}").into(),
        // Text shares the row's `Arc<str>`: no copy.
        CellValue::Text(_) => SharedString::from(quote.texts[column.0 as usize - 31].clone()),
        _ => SharedString::default(),
    }
}

fn format_row(quote: &Quote, order: &[usize]) -> Box<[SharedString]> {
    order
        .iter()
        .map(|&ix| format_cell(quote, Col(ix as u16)))
        .collect()
}

impl TableDelegate for TableBench {
    fn columns_count(&self, _: &App) -> usize {
        COLUMNS
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let text = match &mut self.cache {
            Some(cache) => {
                let (rows, order) = (&self.rows, &self.order);
                cache
                    .entry(row_ix)
                    .or_insert_with(|| format_row(&rows[row_ix], order))[col_ix]
                    .clone()
            }
            None => match self.rows[row_ix].value(Col(self.order[col_ix] as u16)) {
                CellValue::Int(value) => value.to_string().into(),
                CellValue::Float(value) => format!("{value:.2}").into(),
                CellValue::Text(text) => SharedString::from(text.to_string()),
                _ => SharedString::default(),
            },
        };
        div().child(text)
    }
}

struct TableRoot {
    table: Entity<TableState<TableBench>>,
    samples: Samples,
}

impl Render for TableRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Timed::new(
            div()
                .size_full()
                .child(DataTable::new(&self.table).bordered(false).xsmall()),
            self.samples.clone(),
        )
    }
}

// ---------------------------------------------------------------------------
// Harness.

fn headless() -> HeadlessAppContext {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    cx
}

fn window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Default::default(),
            size: size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        })),
        focus: false,
        show: false,
        ..Default::default()
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.
}

fn percentile(samples: &[Duration], p: f64) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted = samples.to_vec();
    sorted.sort();
    sorted[((sorted.len() as f64 - 1.) * p).round() as usize]
}

struct Harness {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    samples: Samples,
}

impl Harness {
    /// One frame interval: background work, timers, and the draw GPUI makes
    /// if anything became dirty (recorded by `Timed`).
    fn tick(&mut self) {
        self.cx.advance_clock(FRAME);
        self.cx.run_until_parked();
    }

    fn take(&self) -> Vec<Duration> {
        std::mem::take(&mut *self.samples.borrow_mut())
    }

    /// One frame interval with a display link, which the test platform does
    /// not have: the interval's draws as [`Self::tick`], or, if nothing drew,
    /// the frame an animation asked for. A display draws the animation and
    /// the new data in one frame, so this adds no draw to an interval that
    /// has one.
    fn display_tick(&mut self) {
        let drawn = self.samples.borrow().len();
        self.tick();
        if self.samples.borrow().len() == drawn {
            self.cx
                .update_window(self.window, |_, window, cx| {
                    window.simulate_next_frame(cx);
                })
                .expect("window is open");
        }
    }

    /// Time GPUI spends per frame outside the measured subtree, from forced
    /// full frames: whole draw minus the subtree's share of it.
    fn overhead(&mut self) -> Duration {
        self.take();
        let mut whole = Vec::new();
        for _ in 0..40 {
            let started = Instant::now();
            self.cx
                .update_window(self.window, |_, window, cx| window.render_frame(cx))
                .expect("window is open");
            whole.push(started.elapsed());
        }
        let subtree = self.take();
        percentile(&whole, 0.5).saturating_sub(percentile(&subtree, 0.5))
    }

    fn report(&mut self, name: &str, frames: usize, samples: &[Duration], notes: &str) {
        let overhead = self.overhead();
        out!(
            "| {name} | {} | {:.0} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {notes} |",
            samples.len(),
            samples.len() as f64 / (frames as f64 * FRAME.as_secs_f64()),
            ms(percentile(samples, 0.5)),
            ms(percentile(samples, 0.95)),
            ms(percentile(samples, 0.99)),
            ms(percentile(samples, 1.0)),
            ms(overhead),
        );
    }

    fn screenshot(&mut self, variable: &str) {
        if let Ok(path) = std::env::var(variable) {
            let image = self
                .cx
                .capture_screenshot(self.window)
                .expect("headless Metal renderer");
            image.save(&path).expect("save screenshot");
            out!("screenshot: {path}");
        }
    }
}

struct GridHarness {
    harness: Harness,
    grid: Entity<LiveGridState<Blotter>>,
    model: Vec<Quote>,
    live: Vec<u64>,
    rng: fastrand::Rng,
}

impl GridHarness {
    fn new(pools: &Pools, view: ViewSpec<Quote>) -> Self {
        Self::configured(pools, |grid| grid.with_view(view))
    }

    fn configured(
        pools: &Pools,
        configure: impl FnOnce(LiveGridState<Blotter>) -> LiveGridState<Blotter> + 'static,
    ) -> Self {
        Self::configured_with_panel(pools, false, configure)
    }

    /// [`Self::configured`], with the group panel shown if `group_panel`.
    fn configured_with_panel(
        pools: &Pools,
        group_panel: bool,
        configure: impl FnOnce(LiveGridState<Blotter>) -> LiveGridState<Blotter> + 'static,
    ) -> Self {
        Self::configured_with_store(
            pools,
            StoreOptions::default().with_highlight(HighlightPolicy::default()),
            group_panel,
            configure,
        )
    }

    /// [`Self::configured_with_panel`] over a store with `options`, such as
    /// a removal fade (S21).
    fn configured_with_store(
        pools: &Pools,
        options: StoreOptions,
        group_panel: bool,
        configure: impl FnOnce(LiveGridState<Blotter>) -> LiveGridState<Blotter> + 'static,
    ) -> Self {
        Self::configured_with_schema(pools, schema(), options, group_panel, configure)
    }

    /// [`Self::configured_with_store`] over another `schema`, such as one
    /// with elements in cells (S26).
    fn configured_with_schema(
        pools: &Pools,
        schema: GridSchema<Col>,
        options: StoreOptions,
        group_panel: bool,
        configure: impl FnOnce(LiveGridState<Blotter>) -> LiveGridState<Blotter> + 'static,
    ) -> Self {
        let mut cx = headless();
        let mut rng = fastrand::Rng::with_seed(7);
        let model: Vec<Quote> = (0..ROWS).map(|id| pools.row(&mut rng, id)).collect();
        let samples: Samples = Rc::default();
        let (window, root) = cx
            .update(|cx| {
                gpui_kit::open_window(window_options(), cx, |_, cx| {
                    cx.new(|cx| GridRoot {
                        grid: cx.new(|cx| {
                            configure(LiveGridState::new_standalone(schema, Blotter, options, cx))
                        }),
                        samples: samples.clone(),
                        group_panel,
                    })
                })
            })
            .expect("open headless window");
        let grid = cx.read_entity(&root, |root, _| root.grid.clone());
        let writer = cx.read_entity(&grid, |grid, _| grid.writer());
        writer.replace_all(model.iter().cloned().map(Arc::new));
        let mut harness = Harness {
            cx,
            window,
            samples,
        };
        // Load, rebuild and warm caches, then start measuring.
        for _ in 0..30 {
            harness.tick();
        }
        harness.take();
        Self {
            harness,
            grid,
            model,
            live: (0..ROWS).collect(),
            rng,
        }
    }

    fn visible_keys(&mut self) -> Vec<u64> {
        self.harness
            .cx
            .read_entity(&self.grid, |grid, _| grid.visible_keys())
    }

    fn write(&mut self, ids: impl IntoIterator<Item = u64>) {
        let writer = self
            .harness
            .cx
            .read_entity(&self.grid, |grid, _| grid.writer());
        for id in ids {
            let quote = &mut self.model[id as usize];
            mutate(&mut self.rng, quote);
            writer.upsert(quote.clone());
        }
    }

    fn update_grid(
        &mut self,
        f: impl FnOnce(&mut LiveGridState<Blotter>, &mut Context<LiveGridState<Blotter>>),
    ) {
        self.harness.cx.update(|cx| self.grid.update(cx, f));
    }

    /// Focuses the grid and sends it a key, as a user would: the key's
    /// Action runs and the focus outline shows (keyboard input).
    fn press(&mut self, key: &str) {
        let grid = self.grid.clone();
        self.harness
            .cx
            .update_window(self.harness.window, |_, window, cx| {
                let handle = grid.focus_handle(cx);
                window.focus(&handle, cx);
                window.press(key, cx);
            })
            .expect("window is open");
    }

    /// Dispatches an Action to the focused grid, without a forced frame.
    fn dispatch_action(&mut self, action: &dyn Action) {
        self.harness
            .cx
            .update_window(self.harness.window, |_, window, cx| {
                window.dispatch_action(action.boxed_clone(), cx);
            })
            .expect("window is open");
    }

    /// Selects every row from the keyboard with the focus on the first row
    /// (`RowSet`): every painted row is filled and the focus outline shows.
    /// Returns a note on what the frames paint.
    fn select_everything(&mut self) -> String {
        self.press("down");
        self.press("secondary-a");
        for _ in 0..5 {
            self.harness.tick();
        }
        self.harness.take();
        self.harness.cx.read_entity(&self.grid, |grid, _| {
            format!(
                "{} rows selected, focus outline {}",
                grid.selection().len(),
                if grid.is_focus_visible() {
                    "shown"
                } else {
                    "hidden"
                }
            )
        })
    }

    fn last_pull(&mut self) -> Duration {
        self.harness
            .cx
            .read_entity(&self.grid, |grid, _| grid.stats().last_pull())
    }

    fn notes(&mut self) -> String {
        self.harness.cx.read_entity(&self.grid, |grid, _| {
            let stats = grid.stats();
            let text = grid.text_stats();
            let lookups = (text.hits() + text.misses() + text.numeric()).max(1);
            format!(
                "cells {}, prepaint {:.2} ms, shaped {:.1}% of cells, rebuilds {} (max {:.1} ms), compactions {}",
                stats.visible_cells(),
                ms(stats.last_prepaint()),
                text.misses() as f64 * 100. / lookups as f64,
                stats.rebuilds(),
                ms(stats.max_rebuild()),
                stats.compactions(),
            )
        })
    }
}

fn header() {
    out!(
        "| Scenario | Draws | Draws/s | p50 | p95 | p99 | max | Overhead | Notes |\n|---|---|---|---|---|---|---|---|---|"
    );
}

/// How a scenario's selection variant labels its row.
#[derive(Default)]
struct SelectionLabel {
    /// Appended to the scenario name.
    suffix: String,
    /// Appended to the notes.
    note: String,
}

/// A grid in `RowSet` with `view`; with `selected`, every row selected from
/// the keyboard (the selection fill on every painted row and the focus
/// outline must not slow the frame).
fn selection_harness(
    pools: &Pools,
    view: ViewSpec<Quote>,
    selected: bool,
) -> (GridHarness, SelectionLabel) {
    let mut grid = GridHarness::configured(pools, move |grid| {
        grid.with_view(view)
            .with_selection_mode(SelectionMode::RowSet)
    });
    let label = if selected {
        SelectionLabel {
            suffix: format!(", {ROWS} rows selected, focus outline"),
            note: format!(", {}", grid.select_everything()),
        }
    } else {
        SelectionLabel::default()
    };
    (grid, label)
}

/// S1: ~2k updates/s, all on painted rows. Returns the draws' p50, which
/// S10 compares with.
fn scenario_visible_updates(pools: &Pools, selected: bool) -> Duration {
    let (mut grid, label) = selection_harness(
        pools,
        ViewSpec::new().with_sort([(Col(0), SortDirection::Ascending)]),
        selected,
    );
    let SelectionLabel {
        suffix,
        note: selection_note,
    } = label;
    let mut header = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        // ~2k updates/s at 60 fps, all of them on painted rows.
        let visible = grid.visible_keys();
        let ids: Vec<u64> = (0..33)
            .map(|_| visible[grid.rng.usize(..visible.len())])
            .collect();
        grid.write(ids);
        grid.harness.tick();
        header.push(
            grid.harness
                .cx
                .read_entity(&grid.grid, |grid, _| grid.stats().last_header_render()),
        );
    }
    let samples = grid.harness.take();
    let notes = format!(
        "{}, header build p50 {:.3} ms, p99 {:.3} ms{selection_note}",
        grid.notes(),
        ms(percentile(&header, 0.5)),
        ms(percentile(&header, 0.99)),
    );
    grid.harness.report(
        &format!("S1 2k/s, all on painted rows{suffix}"),
        FRAMES,
        &samples,
        &notes,
    );
    if !selected {
        grid.harness.screenshot("SPIKE_SCREENSHOT");
    }
    percentile(&samples, 0.5)
}

/// S10: the S1 load with the accessibility tree of the body planned
/// (`set_a11y_forced`) and a cell focused from the keyboard, so the frame
/// also plans the focused cell and asks its tooltip. The headless window
/// keeps accessibility off, so GPUI publishes no tree: the draws pay for
/// gathering the plan in the cell walk, the plan with every node GPUI would
/// build from it (`last_a11y_plan`, budget p99 ≤ 1 ms) and the row
/// elements' layout and prepaint, not for GPUI's tree update or the
/// platform adapter. The draws keep the S1 budget, p99 ≤ 8 ms.
fn scenario_a11y(pools: &Pools, s1_p50: Duration) -> Duration {
    let (mut grid, _) = selection_harness(
        pools,
        ViewSpec::new().with_sort([(Col(0), SortDirection::Ascending)]),
        false,
    );
    grid.update_grid(|grid, _| grid.set_a11y_forced(true));
    grid.press("down");
    for _ in 0..5 {
        grid.harness.tick();
    }
    grid.harness.take();
    let stats = |grid: &mut GridHarness| {
        grid.harness
            .cx
            .read_entity(&grid.grid, |grid, _| grid.stats())
    };
    let mut plans = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        // The S1 load: ~2k updates/s at 60 fps, all of them on painted rows.
        let visible = grid.visible_keys();
        let ids: Vec<u64> = (0..33)
            .map(|_| visible[grid.rng.usize(..visible.len())])
            .collect();
        grid.write(ids);
        let before = stats(&mut grid).frames();
        grid.harness.tick();
        let after = stats(&mut grid);
        if after.frames() != before {
            plans.push(after.last_a11y_plan());
        }
    }
    let samples = grid.harness.take();
    let last = stats(&mut grid);
    let p50 = percentile(&samples, 0.5);
    let notes = format!(
        "plan and nodes p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms (budget p99 1 ms); {} rows, {} synthetic cells and the focused cell; draw p50 {:+.2} ms over S1; {}",
        ms(percentile(&plans, 0.5)),
        ms(percentile(&plans, 0.99)),
        ms(percentile(&plans, 1.0)),
        last.a11y_rows(),
        last.a11y_cells(),
        ms(p50) - ms(s1_p50),
        grid.notes(),
    );
    grid.harness.report(
        "S10 S1 with the accessibility tree planned (forced), a cell focused",
        FRAMES,
        &samples,
        &notes,
    );
    p50
}

/// S20: the S1 load with `DirectionalChanges`: every hot cell that rose or
/// fell has an arrow, in `success` or `danger`. Prices move by up to
/// ±0.5%, so they rise and fall at random. Budget: p99 ≤ 8 ms.
fn scenario_directional(pools: &Pools, s1_p50: Duration) {
    let mut grid = GridHarness::configured(pools, |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Col(0), SortDirection::Ascending)]))
            .with_change_paint(ChangePaint::Directional(DirectionalChanges))
    });
    let mut icons = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        // ~2k updates/s at 60 fps, all of them on painted rows.
        let visible = grid.visible_keys();
        let ids: Vec<u64> = (0..33)
            .map(|_| visible[grid.rng.usize(..visible.len())])
            .collect();
        grid.write(ids);
        grid.harness.tick();
        icons.push(
            grid.harness
                .cx
                .read_entity(&grid.grid, |grid, _| grid.stats().icons()),
        );
    }
    let samples = grid.harness.take();
    icons.sort_unstable();
    let notes = format!(
        "{}, arrows per frame p50 {}, max {}; draw p50 {:+.2} ms over S1 (budget p99 8 ms)",
        grid.notes(),
        icons[icons.len() / 2],
        icons.last().copied().unwrap_or(0),
        ms(percentile(&samples, 0.5)) - ms(s1_p50),
    );
    grid.harness.report(
        "S20 S1 with DirectionalChanges: an arrow on every hot cell that rose or fell",
        FRAMES,
        &samples,
        &notes,
    );
}

/// S1h: the S1 load with the pointer resting over a truncated Comment cell,
/// its automatic tooltip shown. Every frame plans the hover div over the
/// cell and asks the delegate's `tooltip` once; the shown tooltip view
/// draws outside the measured subtree (overhead).
fn scenario_hover_tooltip(pools: &Pools) {
    let mut grid = GridHarness::new(
        pools,
        ViewSpec::new().with_sort([(Col(0), SortDirection::Ascending)]),
    );
    // The Comment column spans x 850..1070 (two frozen columns, 190 px, and
    // six text columns of 110 px before it); the header has a band row, so
    // the rows start at y 52. Line 20, mid-column.
    let position = point(px(960.), px(52. + 26. * 20. + 13.));
    for position in [position, position + point(px(1.), px(0.))] {
        grid.harness.dispatch(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        grid.harness.tick();
    }
    // GPUI's 500 ms tooltip delay.
    for _ in 0..40 {
        grid.harness.tick();
    }
    let shown = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.is_tooltip_shown());
    grid.harness.take();
    for _ in 0..FRAMES {
        // The S1 load: ~2k updates/s at 60 fps, all of them on painted rows.
        let visible = grid.visible_keys();
        let ids: Vec<u64> = (0..33)
            .map(|_| visible[grid.rng.usize(..visible.len())])
            .collect();
        grid.write(ids);
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let notes = format!(
        "{}, tooltip {}",
        grid.notes(),
        if shown { "shown" } else { "not shown" }
    );
    grid.harness.report(
        "S1h S1 with the pointer over a truncated cell, auto-tooltip shown",
        FRAMES,
        &samples,
        &notes,
    );
}

fn scenario_random_p2(pools: &Pools, selected: bool) {
    let (mut grid, label) = selection_harness(
        pools,
        ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]),
        selected,
    );
    let SelectionLabel {
        suffix,
        note: selection_note,
    } = label;
    for _ in 0..FRAMES {
        // ~10k updates/s at 60 fps on random keys; the sort column changes.
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let notes = format!("{}{selection_note}", grid.notes());
    grid.harness.report(
        &format!("S2 P2: 10k/s random keys, sort by a changing column{suffix}"),
        FRAMES,
        &samples,
        &notes,
    );
}

/// `Rebuild`, counting how many rebuilds run at once and how many start.
#[derive(Clone, Default)]
struct CountingEngine {
    running: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    started: Arc<AtomicUsize>,
}

impl ProjectionEngine<Quote> for CountingEngine {
    fn rebuild(&self, snapshot: &StoreSnapshot<Quote>, spec: &ViewSpec<Quote>) -> Projection {
        self.started.fetch_add(1, Ordering::Relaxed);
        let running = self.running.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(running, Ordering::Relaxed);
        let projection = Rebuild::default().rebuild(snapshot, spec);
        self.running.fetch_sub(1, Ordering::Relaxed);
        projection
    }
}

/// S11b: a search field typed into over P2. Eight quick filter texts 120 ms
/// apart while 10k/s updates stream, sorted by a text column; the filter
/// searches the ten S11 columns.
/// S27: a cell edited over P2. The editor of a text cell of a painted row
/// is open for 10 s while 10k/s updates stream, sorted by a changing
/// column; eight characters are typed 120 ms apart, as in S11b.
/// `LiveReorder` holds the order while the field is open: the field's row
/// stays on its screen line, and the rebuilds keep running, one at a time,
/// each new order replacing the held one. Enter commits; the held order
/// shows in the next frame.
fn scenario_editing_p2(pools: &Pools) {
    const TYPING_FROM: usize = 20;
    const TYPING_INTERVAL: Duration = Duration::from_millis(120);
    const TEXT: &str = "trader42";
    let frames = (Duration::from_secs(10).as_millis() / FRAME.as_millis()) as usize;
    // The trader text column.
    let edited = Col(35);
    let engine = CountingEngine::default();
    let mut grid = GridHarness::configured(pools, {
        let engine = engine.clone();
        move |grid| {
            grid.with_view(ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]))
                .with_editor(edited, TextEditor::new())
                .with_engine(engine)
        }
    });
    let key = grid.visible_keys()[3];
    let (window, state) = (grid.harness.window, grid.grid.clone());
    let opened = grid
        .harness
        .cx
        .update_window(window, |_, window, cx| {
            state.update(cx, |grid, cx| grid.start_edit(&key, edited, window, cx))
        })
        .expect("window is open");
    assert!(opened, "the editor opens on a painted row");
    grid.harness.tick();
    grid.harness.take();
    let started_before = engine.started.load(Ordering::Relaxed);
    let mut typed = 0;
    let mut moved = 0;
    for frame_ix in 0..frames {
        // ~10k updates/s at 60 fps on random keys; the sort column changes.
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        let typing_time = FRAME * frame_ix.saturating_sub(TYPING_FROM) as u32;
        if typed < TEXT.len()
            && frame_ix >= TYPING_FROM
            && typing_time >= TYPING_INTERVAL * typed as u32
        {
            let character = &TEXT[typed..typed + 1];
            grid.harness
                .cx
                .update_window(window, |_, window, cx| window.input(character, cx))
                .expect("window is open");
            typed += 1;
        }
        grid.harness.tick();
        if grid.visible_keys().get(3) != Some(&key) {
            moved += 1;
        }
    }
    let samples = grid.harness.take();
    let started = engine.started.load(Ordering::Relaxed) - started_before;
    let (editing, held) = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        (grid.is_editing(), grid.is_order_held())
    });
    // Enter commits: the held order shows in the next frame.
    grid.harness
        .cx
        .update_window(window, |_, window, cx| window.press("enter", cx))
        .expect("window is open");
    grid.harness.tick();
    let commit_frames = grid.harness.take();
    let held_after = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.is_order_held());
    let notes = format!(
        "{}, {typed} characters typed into the field, which is {}; the order {} at the end, the field's row off its screen line in {moved} frame(s); rebuilds started {started}, at most {} at once; after Enter the order is {} in 1 frame ({:.2} ms)",
        grid.notes(),
        if editing { "still open" } else { "closed" },
        if held { "held" } else { "not held" },
        engine.peak.load(Ordering::Relaxed),
        if held_after { "still held" } else { "shown" },
        ms(commit_frames.iter().copied().max().unwrap_or_default()),
    );
    grid.harness.report(
        "S27 P2 + a cell editor open 10 s, 8 characters typed 120 ms apart, order held",
        frames,
        &samples,
        &notes,
    );
}

/// Commits the value plus one at once, with no field: a stand-in for a
/// checkbox or a stepper (S28).
struct Bump;

impl CellEditor<Quote> for Bump {
    fn open(&self, start: &EditorStart<'_, Quote>, _: &mut Window, _: &mut App) -> EditorOpen {
        match start.value() {
            CellValue::Float(value) => EditorOpen::Commit(OwnedValue::Float(value + 1.)),
            _ => EditorOpen::Refuse,
        }
    }
}

/// S28: P2 with 1000 optimistic values in "Price 5", on the painted rows
/// (about 50) and on random others, half of them `Pending`, half
/// `Accepted`. The stream confirms 100 a second: a waiting commit gets its
/// `Ok`, an accepted one its value in the store. The pull, which settles
/// the accepted values, is timed against the same load without edits.
fn scenario_optimistic_values(pools: &Pools) {
    const EDITS: usize = 1000;
    const BASELINE_FRAMES: usize = 120;
    let edited = Col(5);
    let mut grid = GridHarness::configured(pools, move |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]))
            .with_editor(edited, Bump)
    });
    let stream = |grid: &mut GridHarness| {
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
    };
    // The pull without edits.
    let mut bare_pulls = Vec::with_capacity(BASELINE_FRAMES);
    for _ in 0..BASELINE_FRAMES {
        stream(&mut grid);
        grid.harness.tick();
        bare_pulls.push(grid.last_pull());
    }
    grid.harness.take();
    // The commits: random rows first, the painted ones last, top to
    // bottom, so the body ends at the top.
    let commits: Rc<RefCell<Vec<(EditId, u64, OwnedValue)>>> = Rc::default();
    let _subscription = grid.harness.cx.update(|cx| {
        let commits = commits.clone();
        cx.subscribe(&grid.grid, move |_, event, _| {
            if let LiveGridEvent::EditCommit {
                edit, key, value, ..
            } = event
            {
                commits.borrow_mut().push((*edit, *key, value.clone()));
            }
        })
    });
    let painted = grid.visible_keys();
    let mut keys: Vec<u64> = Vec::with_capacity(EDITS);
    while keys.len() < EDITS - painted.len() {
        let key = grid.rng.u64(..ROWS);
        if !painted.contains(&key) && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys.extend(painted.iter().copied());
    let (window, state) = (grid.harness.window, grid.grid.clone());
    grid.harness
        .cx
        .update_window(window, |_, window, cx| {
            state.update(cx, |grid, cx| {
                for key in &keys {
                    grid.start_edit(key, edited, window, cx);
                }
            })
        })
        .expect("window is open");
    // Every other commit is accepted.
    let mut waiting: Vec<(EditId, u64, OwnedValue)> = commits.take();
    let mut accepted = Vec::new();
    grid.update_grid(|grid, cx| {
        for (ix, commit) in waiting.iter().enumerate() {
            if ix % 2 == 1 {
                grid.resolve_edit(commit.0, Ok(()), cx);
                accepted.push(commit.clone());
            }
        }
    });
    waiting = waiting.into_iter().step_by(2).collect();
    for _ in 0..10 {
        grid.harness.tick();
    }
    grid.harness.take();
    let count = |grid: &mut GridHarness| {
        grid.harness
            .cx
            .read_entity(&grid.grid, |grid, _| grid.pending_edit_count())
    };
    let at_start = count(&mut grid);
    let mut pulls = Vec::with_capacity(FRAMES);
    for frame_ix in 0..FRAMES {
        stream(&mut grid);
        // 100 confirmations a second: 8 every 5 frames of 16 ms.
        let confirmations = if frame_ix % 5 < 3 { 2 } else { 1 };
        for ix in 0..confirmations {
            if ix % 2 == 0
                && let Some(commit) = waiting.pop()
            {
                grid.update_grid(|grid, cx| {
                    grid.resolve_edit(commit.0, Ok(()), cx);
                });
                accepted.push(commit);
            } else if let Some((_, key, value)) = accepted.pop() {
                let writer = grid
                    .harness
                    .cx
                    .read_entity(&grid.grid, |grid, _| grid.writer());
                let quote = &mut grid.model[key as usize];
                if let OwnedValue::Float(value) = value {
                    quote.prices[edited.0 as usize - 1] = value;
                }
                writer.upsert(quote.clone());
            }
        }
        grid.harness.tick();
        pulls.push(grid.last_pull());
    }
    let samples = grid.harness.take();
    let at_end = count(&mut grid);
    let notes = format!(
        "{}, {at_start} optimistic values at the start ({} painted), {at_end} at the end; pull p50 {:.3} ms, p99 {:.3} ms (without edits {:.3} ms, {:.3} ms)",
        grid.notes(),
        painted.len(),
        ms(percentile(&pulls, 0.5)),
        ms(percentile(&pulls, 0.99)),
        ms(percentile(&bare_pulls, 0.5)),
        ms(percentile(&bare_pulls, 0.99)),
    );
    grid.harness.report(
        "S28 P2 + 1000 optimistic values, 100 confirmed a second",
        FRAMES,
        &samples,
        &notes,
    );
}

fn scenario_quick_filter_typing(pools: &Pools) {
    const TEXTS: [&str; 8] = [
        "t", "tr", "tra", "trad", "trade", "trader", "trader4", "trader42",
    ];
    const TYPING_FROM: usize = 20;
    const TYPING_INTERVAL: Duration = Duration::from_millis(120);
    let engine = CountingEngine::default();
    let mut grid = GridHarness::configured(pools, {
        let engine = engine.clone();
        move |grid| {
            grid.with_view(
                ViewSpec::new().with_sort([(Col(COUNTERPARTY), SortDirection::Ascending)]),
            )
            .with_quick_filter_columns(QUICK_FILTER_COLUMNS.map(Col))
            .with_engine(engine)
        }
    });
    let rebuilds = |grid: &mut GridHarness| {
        grid.harness
            .cx
            .read_entity(&grid.grid, |grid, _| grid.stats().rebuilds())
    };
    let started_before = engine.started.load(Ordering::Relaxed);
    let finished_before = rebuilds(&mut grid);
    let mut typed = 0;
    let mut last_change: Option<(usize, u64)> = None;
    let mut shown_after = None;
    let mut steady_from = None;
    for frame_ix in 0..FRAMES {
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        let typing_time = FRAME * frame_ix.saturating_sub(TYPING_FROM) as u32;
        if typed < TEXTS.len()
            && frame_ix >= TYPING_FROM
            && typing_time >= TYPING_INTERVAL * typed as u32
        {
            let text = TEXTS[typed];
            grid.update_grid(|grid, cx| grid.set_quick_filter(text, cx));
            typed += 1;
            if typed == TEXTS.len() {
                last_change = Some((frame_ix, rebuilds(&mut grid)));
            }
        }
        grid.harness.tick();
        if let Some((changed_at, before)) = last_change
            && shown_after.is_none()
            && rebuilds(&mut grid) > before
        {
            // The rebuild in flight at the last change was dropped, so the
            // first one to finish after it has the last text.
            shown_after = Some(frame_ix + 1 - changed_at);
            steady_from = Some((frame_ix + 1, rebuilds(&mut grid)));
        }
    }
    let samples = grid.harness.take();
    let finished = rebuilds(&mut grid) - finished_before;
    let started = (engine.started.load(Ordering::Relaxed) - started_before) as u64;
    let steady = steady_from.map_or(0., |(from, at)| {
        (rebuilds(&mut grid) - at) as f64 / ((FRAMES - from) as f64 * FRAME.as_secs_f64())
    });
    let notes = format!(
        "{}, rebuilds started {started}, dropped unfinished {}, at most {} at once, last text shown {} frame(s) after its change, then {steady:.1} rebuilds/s with the filter on",
        grid.notes(),
        started.saturating_sub(finished),
        engine.peak.load(Ordering::Relaxed),
        shown_after.map_or("never".to_string(), |frames| frames.to_string()),
    );
    grid.harness.report(
        "S11b P2 + typing in the quick filter, 8 texts 120 ms apart",
        FRAMES,
        &samples,
        &notes,
    );
}

fn scenario_churn(pools: &Pools, selected: bool) {
    let (mut grid, label) = selection_harness(
        pools,
        ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]),
        selected,
    );
    let SelectionLabel {
        suffix,
        note: selection_note,
    } = label;
    let mut pulls = Vec::with_capacity(FRAMES);
    let writer = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.writer());
    let mut max_tombstones = 0;
    for _ in 0..FRAMES {
        // 10k updates/s plus ~1k removals/s and ~1k inserts/s; a third of the
        // removals hit painted rows.
        let ids: Vec<u64> = (0..166)
            .map(|_| grid.live[grid.rng.usize(..grid.live.len())])
            .collect();
        grid.write(ids);
        let visible = grid.visible_keys();
        for ix in 0..17 {
            let key = if ix % 3 == 0 && !visible.is_empty() {
                visible[grid.rng.usize(..visible.len())]
            } else {
                grid.live[grid.rng.usize(..grid.live.len())]
            };
            if let Some(position) = grid.live.iter().position(|id| *id == key) {
                grid.live.swap_remove(position);
                writer.remove(key);
            }
            let id = grid.model.len() as u64;
            let quote = pools.row(&mut grid.rng, id);
            grid.model.push(quote.clone());
            grid.live.push(id);
            writer.upsert(quote);
        }
        grid.harness.tick();
        pulls.push(grid.last_pull());
        max_tombstones = max_tombstones.max(
            grid.harness
                .cx
                .read_entity(&grid.grid, |grid, _| grid.stats().tombstones()),
        );
    }
    let samples = grid.harness.take();
    // The pull (drain) applies one snapshot on the UI thread: with every
    // row selected it also drops the removed keys, O(removed).
    let notes = format!(
        "{}, max tombstones skipped {max_tombstones}, pull p50 {:.3} ms, p99 {:.3} ms{selection_note}",
        grid.notes(),
        ms(percentile(&pulls, 0.5)),
        ms(percentile(&pulls, 0.99)),
    );
    grid.harness.report(
        &format!("S7 P2 + 1k/s removals + 1k/s inserts{suffix}"),
        FRAMES,
        &samples,
        &notes,
    );
}

fn scenario_fling(pools: &Pools, selected: bool) {
    let (mut grid, label) = selection_harness(pools, ViewSpec::new(), selected);
    let SelectionLabel {
        suffix,
        note: selection_note,
    } = label;
    for frame_ix in 0..FRAMES {
        let page = grid.visible_keys().len();
        grid.update_grid(|grid, cx| grid.scroll_to_row((frame_ix + 1) * page, cx));
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let notes = format!("{}{selection_note}", grid.notes());
    grid.harness.report(
        &format!("S3 fling: a new page every frame{suffix}"),
        FRAMES,
        &samples,
        &notes,
    );
}

/// S25: the S1 load with a status icon (changing with the first price) and
/// a time (changing with "Size 10") on every painted row. Budget p99 8 ms;
/// a time is laid out from the glyph table of numbers, so it is never
/// shaped.
fn scenario_typed_cells(pools: &Pools) {
    TYPED_CELLS.store(true, Ordering::Relaxed);
    let mut grid = GridHarness::new(
        pools,
        ViewSpec::new().with_sort([(Col(0), SortDirection::Ascending)]),
    );
    let mut icons = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let visible = grid.visible_keys();
        let ids: Vec<u64> = (0..33)
            .map(|_| visible[grid.rng.usize(..visible.len())])
            .collect();
        // Every written row's time moves a second.
        for id in &ids {
            grid.model[*id as usize].sizes[9] += 1;
        }
        grid.write(ids);
        grid.harness.tick();
        icons.push(
            grid.harness
                .cx
                .read_entity(&grid.grid, |grid, _| grid.stats().icons()),
        );
    }
    let samples = grid.harness.take();
    icons.sort_unstable();
    let notes = format!(
        "{}, icons {}..{} a frame",
        grid.notes(),
        icons.first().copied().unwrap_or_default(),
        icons.last().copied().unwrap_or_default(),
    );
    grid.harness.report(
        "S25 S1 + a status icon and a time on every row (budget p99 8 ms)",
        FRAMES,
        &samples,
        &notes,
    );
    TYPED_CELLS.store(false, Ordering::Relaxed);
}

/// S29: the P2 load (S2) with an image in the book column of every row
/// (16 different, 32 × 32 px) and the rows scrolling a row a frame. Budget
/// p99 12 ms and at most 10% over S2; the image source asks GPUI for no
/// image after the warm-up.
fn scenario_cell_images(pools: &Pools) {
    IMAGE_CELLS.store(true, Ordering::Relaxed);
    let source = AssetImageSource::default();
    let mut grid = GridHarness::configured(pools, {
        let source = source.clone();
        move |grid| {
            grid.with_view(ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]))
                .with_image_source(source)
        }
    });
    let warm = source.asset_requests();
    let mut images = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        grid.update_grid(|grid, cx| grid.scroll_by(point(px(0.), px(26.)), cx));
        grid.harness.tick();
        images.push(
            grid.harness
                .cx
                .read_entity(&grid.grid, |grid, _| grid.stats().images()),
        );
    }
    let samples = grid.harness.take();
    images.sort_unstable();
    let notes = format!(
        "{}, images {}..{} a frame, asset requests {} in warm-up and {} over the {FRAMES} frames, {} images kept",
        grid.notes(),
        images.first().copied().unwrap_or_default(),
        images.last().copied().unwrap_or_default(),
        warm,
        source.asset_requests() - warm,
        source.len(),
    );
    grid.harness.report(
        "S29 P2 + an image on every row (16 different) + a row scrolled a frame (budget p99 12 ms, +10% over S2)",
        FRAMES,
        &samples,
        &notes,
    );
    IMAGE_CELLS.store(false, Ordering::Relaxed);
}

/// S26 and S26b: a progress bar and a checkbox in two columns on every
/// painted row, about a hundred elements a frame. S26: the S1 load (budget
/// p99 8 ms). S26b: a new page every frame, as S3, so every host is new
/// (budget p99 12 ms).
fn scenario_cell_elements(pools: &Pools, fling: bool) {
    let mut grid = GridHarness::configured_with_schema(
        pools,
        widget_schema(),
        StoreOptions::default().with_highlight(HighlightPolicy::default()),
        false,
        |grid| {
            grid.with_view(ViewSpec::new().with_sort([(Col(0), SortDirection::Ascending)]))
                .with_selection_mode(SelectionMode::RowSet)
        },
    );
    let mut hosts_prepaint = Vec::with_capacity(FRAMES);
    let mut hosts = Vec::with_capacity(FRAMES);
    for frame_ix in 0..FRAMES {
        if fling {
            let page = grid.visible_keys().len();
            grid.update_grid(|grid, cx| grid.scroll_to_row((frame_ix + 1) * page, cx));
        } else {
            // ~2k updates/s at 60 fps, all of them on painted rows.
            let visible = grid.visible_keys();
            let ids: Vec<u64> = (0..33)
                .map(|_| visible[grid.rng.usize(..visible.len())])
                .collect();
            grid.write(ids);
        }
        grid.harness.tick();
        let stats = grid
            .harness
            .cx
            .read_entity(&grid.grid, |grid, _| grid.stats());
        hosts_prepaint.push(stats.last_hosts_prepaint());
        hosts.push(stats.cell_hosts());
    }
    let samples = grid.harness.take();
    hosts.sort_unstable();
    let notes = format!(
        "{}, hosts {}..{} a frame, hosts layout and prepaint p50 {:.3} ms, p99 {:.3} ms",
        grid.notes(),
        hosts.first().copied().unwrap_or_default(),
        hosts.last().copied().unwrap_or_default(),
        ms(percentile(&hosts_prepaint, 0.5)),
        ms(percentile(&hosts_prepaint, 0.99)),
    );
    let name = if fling {
        "S26b S26 at a fling: a new page every frame (budget p99 12 ms)"
    } else {
        "S26 S1 + a Progress and a Checkbox column, ~100 elements a frame (budget p99 8 ms)"
    };
    grid.harness.report(name, FRAMES, &samples, &notes);
}

/// S15: PageDown held down on P2: the key repeats at 30 Hz (every other
/// frame) while 10k/s updates re-sort the rows; each press moves the focus
/// and the selection a page down and scrolls the body by a page.
fn scenario_page_down(pools: &Pools) {
    let mut grid = GridHarness::new(
        pools,
        ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]),
    );
    grid.press("down");
    grid.harness.tick();
    grid.harness.take();
    let mut presses = 0;
    for frame_ix in 0..FRAMES {
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        if frame_ix % 2 == 0 {
            grid.dispatch_action(&SelectPageDown);
            presses += 1;
        }
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let focused = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        grid.focused_cell()
            .and_then(|cell| cell.row().copied())
            .and_then(|key| grid.snapshot().row_id(&key))
            .and_then(|id| grid.projection().display_ix(id))
    });
    let notes = format!(
        "{}, {presses} presses, focus at display row {}",
        grid.notes(),
        focused.map_or("none".to_string(), |ix| ix.to_string()),
    );
    grid.harness.report(
        "S15 P2 + PageDown autorepeat at 30 Hz",
        FRAMES,
        &samples,
        &notes,
    );
}

/// S8: the P2 load of S2, plus one pinned row at the top and one
/// at the bottom rewritten 10 times a second (highlighted by key), plus an
/// indeterminate progress bar. The bar's animation asks for a frame on every
/// display frame while it shows, so the grid redraws at the display rate;
/// the variant without the bar shows what that costs in draws per second.
/// Pinned cells take the same path as data cells: 2 × 40 more per frame.
///
/// The test platform has no frame loop: S8 delivers the frames an animation
/// asks for (`Harness::display_tick`), so the bar's frames are drawn. Other
/// scenarios do not, and their numbers stay comparable with earlier runs.
fn scenario_pinned_progress(pools: &Pools, progress: bool) {
    let mut grid = GridHarness::new(
        pools,
        ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]),
    );
    let writer = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.writer());
    // Keys of the bands' own space: the reference and the total.
    let mut top = pools.row(&mut grid.rng, ROWS);
    let mut bottom = pools.row(&mut grid.rng, ROWS + 1);
    writer.set_pinned_rows(PinnedEdge::Top, [Arc::new(top.clone())]);
    writer.set_pinned_rows(PinnedEdge::Bottom, [Arc::new(bottom.clone())]);
    if progress {
        writer.set_progress(Some(
            GridProgress::indeterminate().with_label("Recalculating"),
        ));
    }
    for _ in 0..10 {
        grid.harness.display_tick();
    }
    grid.harness.take();
    let frames_before = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.stats().frames());
    for frame_ix in 0..FRAMES {
        // ~10k updates/s at 60 fps on random keys; the sort column changes.
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        // Both bands rewritten 10 times a second.
        if frame_ix % 6 == 0 {
            mutate(&mut grid.rng, &mut top);
            mutate(&mut grid.rng, &mut bottom);
            writer.set_pinned_rows(PinnedEdge::Top, [Arc::new(top.clone())]);
            writer.set_pinned_rows(PinnedEdge::Bottom, [Arc::new(bottom.clone())]);
        }
        grid.harness.display_tick();
    }
    let samples = grid.harness.take();
    let (frames, pinned, shown) = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        (
            grid.stats().frames() - frames_before,
            grid.pinned_keys(PinnedEdge::Top).len() + grid.pinned_keys(PinnedEdge::Bottom).len(),
            grid.progress().is_some(),
        )
    });
    let notes = format!(
        "{}, {pinned} pinned rows, progress {}, body frames {frames}",
        grid.notes(),
        if shown { "shown" } else { "none" },
    );
    let name = if progress {
        "S8 P2 + pinned rows at the top and bottom rewritten 10/s + indeterminate progress"
    } else {
        "S8 P2 + pinned rows at the top and bottom rewritten 10/s, without the progress bar"
    };
    grid.harness.report(name, FRAMES, &samples, &notes);
}

fn scenario_horizontal(pools: &Pools) {
    let mut grid = GridHarness::new(pools, ViewSpec::new());
    for frame_ix in 0..FRAMES {
        let x = px(((frame_ix * 37) % 2_400) as f32);
        grid.update_grid(|grid, cx| grid.set_scroll_x(x, cx));
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let notes = grid.notes();
    grid.harness
        .report("S3b horizontal scroll", FRAMES, &samples, &notes);
}

fn scenario_resize(pools: &Pools) {
    let mut grid = GridHarness::new(pools, ViewSpec::new());
    for frame_ix in 0..FRAMES {
        let width = px(40. + ((frame_ix * 4) % 200) as f32);
        grid.update_grid(|grid, cx| {
            grid.set_column_width(Col(COMMENT as u16), width, cx);
            grid.set_column_width(Col(1), width, cx);
        });
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let notes = grid.notes();
    grid.harness.report(
        "S4 resize drag of two visible columns",
        FRAMES,
        &samples,
        &notes,
    );
    grid.harness.screenshot("SPIKE_SCREENSHOT_TEXT");
}

fn scenario_resize_right_zone(pools: &Pools) {
    let mut grid = GridHarness::new(pools, ViewSpec::new());
    for frame_ix in 0..FRAMES {
        let width = px(40. + ((frame_ix * 4) % 200) as f32);
        grid.update_grid(|grid, cx| grid.set_column_width(Col(RIGHT_ZONE[0] as u16), width, cx));
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let notes = grid.notes();
    grid.harness.report(
        "S4b resize of a right-zone column every frame",
        FRAMES,
        &samples,
        &notes,
    );
}

impl Harness {
    fn dispatch(&mut self, event: impl gpui_kit::InputEvent) {
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.dispatch_event(event.to_platform_input(), cx);
            })
            .expect("window is open");
    }

    fn drag_to(&mut self, position: Point<Pixels>) {
        self.dispatch(MouseMoveEvent {
            position,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
    }
}

/// S22: P2 grouped by two levels with the group panel shown; a header cell
/// dragged from the header up through the panel to its last gap and back,
/// 60 frames with an insertion line, then dropped where it started (nothing
/// changes).
fn scenario_group_panel_drag(pools: &Pools) {
    const DRAG_FRAMES: usize = 60;
    let mut grid = GridHarness::configured_with_panel(pools, true, |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Col(1), SortDirection::Descending)])
                .with_group_by([
                    GroupBy::column(Col(VENUE)),
                    GroupBy::column(Col(SIDE as u16)),
                ]),
        )
    });
    // The column after Side (no band: the header's full height), under the
    // panel's 26 px row; the last gap is right of the two chips.
    let left_zone = column_title_width(0).1 + column_title_width(31).1;
    let from = point(
        px(left_zone + column_title_width(SIDE).1 + 20.),
        px(26. + 13.),
    );
    let last_gap = point(px(WINDOW_WIDTH / 2.), px(13.));
    grid.harness.dispatch(MouseMoveEvent {
        position: from,
        pressed_button: None,
        modifiers: Modifiers::default(),
    });
    grid.harness.dispatch(MouseDownEvent {
        button: MouseButton::Left,
        position: from,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    grid.harness.drag_to(from + point(px(10.), px(0.)));
    grid.harness.tick();
    grid.harness.take();
    let mut lined = 0;
    for frame_ix in 0..DRAG_FRAMES {
        let half = (DRAG_FRAMES / 2) as f32;
        let t = if frame_ix < DRAG_FRAMES / 2 {
            (frame_ix + 1) as f32 / half
        } else {
            (DRAG_FRAMES - frame_ix - 1) as f32 / half
        };
        let position = point(
            from.x + (last_gap.x - from.x) * t,
            from.y + (last_gap.y - from.y) * t,
        );
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        grid.harness.drag_to(position);
        grid.harness.tick();
        let window = grid.harness.window;
        lined += grid
            .harness
            .cx
            .update_window(window, |_, window, _| {
                window.try_find("live-grid-group-panel-gap").is_some()
                    || window.try_find("live-grid-drop-gap").is_some()
            })
            .expect("window is open") as usize;
    }
    let samples = grid.harness.take();
    grid.harness.dispatch(MouseUpEvent {
        button: MouseButton::Left,
        position: from,
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    grid.harness.tick();
    let levels = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.group_by().len());
    let notes = format!(
        "{}, frames with an insertion line {lined}/{DRAG_FRAMES}, levels after the drop {levels}",
        grid.notes()
    );
    grid.harness.report(
        "S22 P2 grouped by 2 columns, header dragged through the group panel and back",
        DRAG_FRAMES,
        &samples,
        &notes,
    );
}

fn scenario_header_drag(pools: &Pools) {
    let mut grid = GridHarness::new(pools, ViewSpec::new());
    // Text 2, the first scrolling column (no band: the full header height),
    // dragged to the right edge of the scrolling zone, then held there while
    // the zone auto-scrolls to its end.
    let left_zone = column_title_width(0).1 + column_title_width(31).1;
    let right_zone: f32 = RIGHT_ZONE.iter().map(|ix| column_title_width(*ix).1).sum();
    let edge = WINDOW_WIDTH - right_zone;
    let y = 13.;
    let from = point(px(left_zone + 55.), px(y));
    grid.harness.dispatch(MouseMoveEvent {
        position: from,
        pressed_button: None,
        modifiers: Modifiers::default(),
    });
    grid.harness.dispatch(MouseDownEvent {
        button: MouseButton::Left,
        position: from,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    grid.harness.drag_to(from + point(px(10.), px(0.)));
    grid.harness.tick();
    grid.harness.take();
    let start_x = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.stats().frames());
    let mut x = from.x;
    for frame_ix in 0..FRAMES {
        let target = px(edge - 4.);
        x = if frame_ix < FRAMES / 3 {
            from.x + (target - from.x) * ((frame_ix + 1) as f32 / (FRAMES / 3) as f32)
        } else {
            target
        };
        grid.harness.drag_to(point(x, px(y)));
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let frames = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.stats().frames())
        - start_x;
    grid.harness.dispatch(MouseUpEvent {
        button: MouseButton::Left,
        position: point(x, px(y)),
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    grid.harness.tick();
    let order_changed = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        grid.columns().position(|column| column == Col(32)) != Some(2)
    });
    let notes = format!(
        "{}, body frames {frames}, dropped at the far end: {order_changed}",
        grid.notes()
    );
    grid.harness.report(
        "S12 header drag across the scrolling zone with auto-scroll",
        FRAMES,
        &samples,
        &notes,
    );
}

/// The frame that applies "autofit all": one draw of the subtree, with the
/// fit measured over its painted rows.
fn scenario_autofit(pools: &Pools) {
    let mut grid = GridHarness::new(pools, ViewSpec::new());
    grid.harness.take();
    let read = |grid: &mut GridHarness| {
        grid.harness.cx.read_entity(&grid.grid, |grid, _| {
            (
                grid.stats().last_prepaint(),
                grid.column_width(Col(COMMENT as u16)),
            )
        })
    };
    let (_, before) = read(&mut grid);
    // The update marks the grid dirty and GPUI draws it at the end of the
    // update: that frame measures and applies the widths. If it does not,
    // the next frame interval does.
    grid.update_grid(|grid, cx| grid.autofit_columns(cx));
    let mut draws = grid.harness.take();
    let (mut prepaint, mut width) = read(&mut grid);
    if width == before {
        grid.harness.tick();
        draws = grid.harness.take();
        (prepaint, width) = read(&mut grid);
    }
    out!(
        "| S13 \"autofit all\", one frame | {:.2} ms |",
        ms(draws.iter().copied().max().unwrap_or_default())
    );
    out!(
        "| S13 prepaint of the last draw (measure, apply, plan) | {:.2} ms ({} draws, comment column {:.0} → {:.0} px) |",
        ms(prepaint),
        draws.len(),
        f32::from(before),
        f32::from(width),
    );
}

fn scenario_fade(pools: &Pools) {
    let mut grid = GridHarness::new(pools, ViewSpec::new());
    let visible = grid.visible_keys();
    grid.write(visible.into_iter().take(30));
    grid.harness.tick();
    grid.harness.take();
    let before = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.stats().fade_wakeups());
    let frames = 400;
    for _ in 0..frames {
        grid.harness.tick();
    }
    let draws = grid.harness.take().len();
    let wakeups = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.stats().fade_wakeups())
        - before;
    let policy = HighlightPolicy::default();
    out!(
        "| S5 fade with no data, {:.1} s | {draws} | | | | | | | timer repaints {wakeups}; expected ≤ {} (steps of {} ms) |",
        frames as f64 * FRAME.as_secs_f64(),
        policy.steps() + 1,
        policy.duration_ms() / policy.steps(),
    );
}

fn scenario_datatable(pools: &Pools, fast: bool) {
    let mut cx = headless();
    let mut rng = fastrand::Rng::with_seed(7);
    let rows: Vec<Quote> = (0..ROWS).map(|id| pools.row(&mut rng, id)).collect();
    let order = display_order();
    let columns: Vec<Column> = order
        .iter()
        .map(|&ix| {
            let (title, width) = column_title_width(ix);
            let column = Column::new(format!("c{ix}"), title).width(px(width));
            if ix <= 30 {
                column.text_right()
            } else {
                column
            }
        })
        .collect();
    let samples: Samples = Rc::default();
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(window_options(), cx, |window, cx| {
                cx.new(|cx| TableRoot {
                    table: cx.new(|cx| {
                        TableState::new(
                            TableBench {
                                rows,
                                columns,
                                order,
                                cache: fast.then(FxHashMap::default),
                            },
                            window,
                            cx,
                        )
                    }),
                    samples: samples.clone(),
                })
            })
        })
        .expect("open headless window");
    let table = cx.read_entity(&root, |root, _| root.table.clone());
    let mut harness = Harness {
        cx,
        window,
        samples,
    };
    for _ in 0..30 {
        harness.tick();
        harness
            .cx
            .update_window(window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    }
    harness.take();
    for _ in 0..FRAMES {
        harness.cx.update(|cx| {
            table.update(cx, |table, cx| {
                let visible = table.visible_range().rows().clone();
                let delegate = table.delegate_mut();
                for _ in 0..33 {
                    let ix = rng.usize(visible.start..visible.end.max(visible.start + 1));
                    if let Some(quote) = delegate.rows.get_mut(ix) {
                        mutate(&mut rng, quote);
                        // The fast delegate formats changed rows here, before
                        // the draw, never in `render_td`.
                        if let Some(cache) = &mut delegate.cache
                            && cache.contains_key(&ix)
                        {
                            cache.insert(ix, format_row(quote, &delegate.order));
                        }
                    }
                }
                cx.notify();
            })
        });
        harness.tick();
    }
    let samples = harness.take();
    let (name, notes) = if fast {
        (
            "S6b DataTable, fast delegate (pre-formatted, render_td only clones)",
            "element per cell, no formatting in render_td",
        )
    } else {
        (
            "S6 baseline: DataTable, S1 load (render_td formats)",
            "element per cell, a String per cell per frame",
        )
    };
    harness.report(name, FRAMES, &samples, notes);
}

// ---------------------------------------------------------------------------
// S14: Cmd/Ctrl+A on P2.

/// The P2 rows under `String` keys ("Q00000042"), for the `String` figure
/// of S14.
#[derive(Clone)]
struct NamedQuote {
    key: String,
    quote: Quote,
}

impl GridRow for NamedQuote {
    type Key = String;
    type Column = Col;

    fn key(&self) -> &String {
        &self.key
    }

    fn value(&self, column: Col) -> CellValue<'_> {
        self.quote.value(column)
    }
}

struct NamedBlotter;

impl LiveGridDelegate for NamedBlotter {
    type Row = NamedQuote;
}

struct NamedRoot {
    grid: Entity<LiveGridState<NamedBlotter>>,
}

impl Render for NamedRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(LiveGrid::new(&self.grid).xsmall().bordered(false))
    }
}

/// Cmd/Ctrl+A over P2 `RUNS` times, as the key's Action on the focused
/// grid: the UI-thread time of the action and the frame it draws, and of
/// `scoped_selection()` over every row. Escape clears between runs.
fn select_all_runs<D: LiveGridDelegate>(
    harness: &mut Harness,
    grid: &Entity<LiveGridState<D>>,
    mut write: impl FnMut(&mut Harness),
) -> (Vec<Duration>, Vec<Duration>, usize) {
    const RUNS: usize = 30;
    let window = harness.window;
    harness
        .cx
        .update_window(window, |_, window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
        })
        .expect("window is open");
    let (mut select, mut scoped, mut selected) = (Vec::new(), Vec::new(), 0);
    for _ in 0..RUNS {
        write(harness);
        harness.tick();
        let ((), elapsed) = time(|| {
            harness
                .cx
                .update_window(window, |_, window, cx| {
                    window.dispatch_action(SelectAll.boxed_clone(), cx);
                })
                .expect("window is open")
        });
        select.push(elapsed);
        let (count, elapsed) = time(|| {
            harness
                .cx
                .read_entity(grid, |grid, _| grid.scoped_selection().len())
        });
        scoped.push(elapsed);
        selected = harness
            .cx
            .read_entity(grid, |grid, _| grid.selection().len())
            .max(count);
        write(harness);
        harness.tick();
        harness
            .cx
            .update_window(window, |_, window, cx| {
                window.dispatch_action(Cancel.boxed_clone(), cx);
            })
            .expect("window is open");
    }
    harness.take();
    (select, scoped, selected)
}

fn scenario_select_all(pools: &Pools) {
    let p2 = || ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]);
    let mut grid = GridHarness::configured(pools, move |grid| {
        grid.with_view(p2())
            .with_selection_mode(SelectionMode::RowSet)
    });
    let writer = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.writer());
    let (mut model, mut rng) = (grid.model.clone(), fastrand::Rng::with_seed(14));
    let (select, scoped, selected) = select_all_runs(&mut grid.harness, &grid.grid, |_| {
        for _ in 0..166 {
            let quote = &mut model[rng.usize(..ROWS as usize)];
            mutate(&mut rng, quote);
            writer.upsert(quote.clone());
        }
    });
    out!(
        "| S14 Cmd/Ctrl+A on P2, {selected} u64 keys (action and its frame) | p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms |",
        ms(percentile(&select, 0.5)),
        ms(percentile(&select, 0.99)),
        ms(percentile(&select, 1.0)),
    );
    out!(
        "| scoped_selection(), {selected} of {ROWS} rows selected, u64 keys | p50 {:.2} ms, p99 {:.2} ms |",
        ms(percentile(&scoped, 0.5)),
        ms(percentile(&scoped, 0.99)),
    );
    // A new data set with every row selected: the pull keeps the keys the
    // new snapshot has, O(selected).
    grid.dispatch_action(&SelectAll);
    grid.harness.tick();
    writer.replace_all(grid.model.iter().cloned().map(Arc::new));
    grid.harness.tick();
    let (pull, kept) = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        (grid.stats().last_pull(), grid.selection().len())
    });
    out!(
        "| replace_all {ROWS} rows with {kept} selected, the pull (UI thread) | {:.2} ms |",
        ms(pull)
    );

    // The same rows under `String` keys.
    let mut cx = headless();
    let mut rng = fastrand::Rng::with_seed(7);
    let mut named: Vec<NamedQuote> = (0..ROWS)
        .map(|id| NamedQuote {
            key: format!("Q{id:08}"),
            quote: pools.row(&mut rng, id),
        })
        .collect();
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(window_options(), cx, |_, cx| {
                cx.new(|cx| NamedRoot {
                    grid: cx.new(|cx| {
                        LiveGridState::new_standalone(
                            schema(),
                            NamedBlotter,
                            StoreOptions::default(),
                            cx,
                        )
                        .with_view(ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]))
                        .with_selection_mode(SelectionMode::RowSet)
                    }),
                })
            })
        })
        .expect("open headless window");
    let grid = cx.read_entity(&root, |root, _| root.grid.clone());
    let writer = cx.read_entity(&grid, |grid, _| grid.writer());
    writer.replace_all(named.iter().cloned().map(Arc::new));
    let mut harness = Harness {
        cx,
        window,
        samples: Samples::default(),
    };
    for _ in 0..30 {
        harness.tick();
    }
    let (select, scoped, selected) = select_all_runs(&mut harness, &grid, |_| {
        for _ in 0..166 {
            let row = &mut named[rng.usize(..ROWS as usize)];
            mutate(&mut rng, &mut row.quote);
            writer.upsert(row.clone());
        }
    });
    out!(
        "| S14 Cmd/Ctrl+A on P2, {selected} String keys (action and its frame) | p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms |",
        ms(percentile(&select, 0.5)),
        ms(percentile(&select, 0.99)),
        ms(percentile(&select, 1.0)),
    );
    out!(
        "| scoped_selection(), {selected} of {ROWS} rows selected, String keys | p50 {:.2} ms, p99 {:.2} ms |",
        ms(percentile(&scoped, 0.5)),
        ms(percentile(&scoped, 0.99)),
    );
}

// ---------------------------------------------------------------------------
// S9: "Copy all" of 100k × 40.

/// Runs jobs on the background executor, as `BackgroundRunner` does, and
/// records how long each one took on its thread.
#[derive(Clone, Default)]
struct TimedRunner {
    jobs: Arc<std::sync::Mutex<Vec<Duration>>>,
}

impl TimedRunner {
    fn take(&self) -> Vec<Duration> {
        std::mem::take(&mut *self.jobs.lock().expect("not poisoned"))
    }
}

impl gpui_kit_grid::TaskRunner for TimedRunner {
    fn run(&self, job: Box<dyn FnOnce() + Send>, cx: &App) -> gpui_kit::Task<()> {
        let jobs = self.jobs.clone();
        cx.background_spawn(async move {
            let started = Instant::now();
            job();
            jobs.lock().expect("not poisoned").push(started.elapsed());
        })
    }
}

/// What `copy_runs` measured, one sample per run.
#[derive(Default)]
struct CopyRuns {
    /// The Action on the UI thread: taking the snapshot, the order and the
    /// columns (and resolving the scope of `Copy`), then starting the job.
    action: Vec<Duration>,
    /// The job: building the table and writing the text.
    job: Vec<Duration>,
    /// The rest of the copy on the UI thread: the test platform's clipboard
    /// write and the task that waits for the job.
    finish: Vec<Duration>,
    /// The macOS pasteboard write of the same text (`SPIKE_SYSTEM_CLIPBOARD`).
    pasteboard: Vec<Duration>,
    bytes: usize,
    rows: usize,
    columns: usize,
}

impl CopyRuns {
    /// The UI-thread time of every run: the Action, the finish and the
    /// pasteboard write when it was measured.
    fn ui_thread(&self) -> Vec<Duration> {
        (0..self.action.len())
            .map(|ix| {
                self.action[ix]
                    + self.finish[ix]
                    + self.pasteboard.get(ix).copied().unwrap_or_default()
            })
            .collect()
    }

    fn report(&self, name: &str) {
        let max = |samples: &[Duration]| ms(percentile(samples, 1.0));
        let pasteboard = if self.pasteboard.is_empty() {
            String::new()
        } else {
            format!(
                ", macOS pasteboard write max {:.2} ms",
                max(&self.pasteboard)
            )
        };
        out!(
            "| {name}, {} rows × {} columns, {:.1} MB, {} runs | background max {:.1} ms (p50 {:.1} ms), UI thread max {:.2} ms (action max {:.2} ms, clipboard write max {:.2} ms{pasteboard}) |",
            self.rows,
            self.columns,
            self.bytes as f64 / 1e6,
            self.action.len(),
            max(&self.job),
            ms(percentile(&self.job, 0.5)),
            max(&self.ui_thread()),
            max(&self.action),
            max(&self.finish),
        );
    }
}

/// Dispatches a copy Action to the focused grid five times. The test
/// dispatcher runs the job on this thread, timed by `TimedRunner`; the rest
/// of `run_until_parked` is the UI-thread finish. With
/// `SPIKE_SYSTEM_CLIPBOARD=1` the text is also written to the macOS general
/// pasteboard, whose previous content is put back.
fn copy_runs<D: LiveGridDelegate>(
    harness: &mut Harness,
    grid: &Entity<LiveGridState<D>>,
    runner: &TimedRunner,
    action: &dyn Action,
) -> CopyRuns {
    copy_runs_with(harness, grid, runner, &|window, cx| {
        window.dispatch_action(action.boxed_clone(), cx)
    })
}

/// [`copy_runs`] of any copy on the UI thread, such as
/// `LiveGridState::copy_scope`.
fn copy_runs_with<D: LiveGridDelegate>(
    harness: &mut Harness,
    grid: &Entity<LiveGridState<D>>,
    runner: &TimedRunner,
    copy: &dyn Fn(&mut Window, &mut App),
) -> CopyRuns {
    const RUNS: usize = 5;
    let window = harness.window;
    harness
        .cx
        .update_window(window, |_, window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
        })
        .expect("window is open");
    let system = std::env::var_os("SPIKE_SYSTEM_CLIPBOARD")
        .map(|_| gpui_kit::platform::current_platform(true));
    let mut runs = CopyRuns::default();
    for _ in 0..RUNS {
        harness.tick();
        runner.take();
        let ((), action_time) = time(|| {
            harness
                .cx
                .update_window(window, |_, window, cx| copy(window, cx))
                .expect("window is open")
        });
        let ((), parked) = time(|| harness.cx.run_until_parked());
        let job: Duration = runner.take().iter().sum();
        runs.action.push(action_time);
        runs.job.push(job);
        runs.finish.push(parked.saturating_sub(job));
        let text = harness
            .cx
            .update(|cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .expect("copied");
        runs.bytes = text.len();
        runs.rows = text.lines().count();
        runs.columns = text
            .lines()
            .next()
            .map_or(0, |line| line.split('\t').count());
        if let Some(platform) = &system {
            let previous = platform.read_from_clipboard();
            let item = gpui_kit::ClipboardItem::new_string(text);
            let ((), elapsed) = time(|| platform.write_to_clipboard(item));
            runs.pasteboard.push(elapsed);
            if let Some(previous) = previous {
                platform.write_to_clipboard(previous);
            }
        }
    }
    harness.take();
    runs
}

/// S9: "Copy all" over P2 (100k rows, 39 visible columns), and Cmd/Ctrl+C
/// with every row selected, under `u64` and `String` keys. The copy job
/// runs on the deterministic dispatcher, so "background" is its time on one
/// thread.
fn scenario_copy_all(pools: &Pools) {
    use gpui_kit_grid::actions::{Copy, CopyAll};

    let p2 = || ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]);
    let runner = TimedRunner::default();
    let mut grid = GridHarness::configured(pools, {
        let runner = runner.clone();
        move |grid| {
            grid.with_view(p2())
                .with_selection_mode(SelectionMode::RowSet)
                .with_task_runner(runner)
        }
    });
    let mut runs = copy_runs(&mut grid.harness, &grid.grid, &runner, &CopyAll);
    // The header line is not a row.
    runs.rows -= 1;
    runs.report("S9 copy all, u64 keys");
    grid.dispatch_action(&SelectAll);
    grid.harness.tick();
    copy_runs(&mut grid.harness, &grid.grid, &runner, &Copy)
        .report("Cmd/Ctrl+C with every row selected, u64 keys");
    drop(grid);

    // The same rows under `String` keys: `Copy` resolves the scope and finds
    // every row by key.
    let mut cx = headless();
    let mut rng = fastrand::Rng::with_seed(7);
    let named: Vec<NamedQuote> = (0..ROWS)
        .map(|id| NamedQuote {
            key: format!("Q{id:08}"),
            quote: pools.row(&mut rng, id),
        })
        .collect();
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(window_options(), cx, |_, cx| {
                cx.new(|cx| NamedRoot {
                    grid: cx.new(|cx| {
                        LiveGridState::new_standalone(
                            schema(),
                            NamedBlotter,
                            StoreOptions::default(),
                            cx,
                        )
                        .with_view(ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]))
                        .with_selection_mode(SelectionMode::RowSet)
                        .with_task_runner(runner.clone())
                    }),
                })
            })
        })
        .expect("open headless window");
    let grid = cx.read_entity(&root, |root, _| root.grid.clone());
    let writer = cx.read_entity(&grid, |grid, _| grid.writer());
    writer.replace_all(named.into_iter().map(Arc::new));
    let mut harness = Harness {
        cx,
        window,
        samples: Samples::default(),
    };
    for _ in 0..30 {
        harness.tick();
    }
    let mut runs = copy_runs(&mut harness, &grid, &runner, &CopyAll);
    runs.rows -= 1;
    runs.report("S9 copy all, String keys");
    harness
        .cx
        .update_window(window, |_, window, cx| {
            window.dispatch_action(SelectAll.boxed_clone(), cx);
        })
        .expect("window is open");
    harness.tick();
    copy_runs(&mut harness, &grid, &runner, &Copy)
        .report("Cmd/Ctrl+C with every row selected, String keys");
}

/// S9b: "Copy all" with every group expanded
/// (`copy_scope(ExportScope::Expanded)`) over P2 grouped by venue (8
/// groups, all collapsed, so the copy expands them), Sum(Volume) and
/// Avg(Last), the totals row at the bottom, `u64` keys; then `CopyAll` of
/// the same view expanded on screen. As in S9, "background" is the job's
/// time on one thread.
fn scenario_copy_expanded(pools: &Pools) {
    use gpui_kit_grid::actions::CopyAll;

    let runner = TimedRunner::default();
    let mut grid = GridHarness::configured(pools, {
        let runner = runner.clone();
        move |grid| {
            grid.with_view(
                ViewSpec::new()
                    .with_sort([(Col(1), SortDirection::Descending)])
                    .with_group_by([GroupBy::column(Col(VENUE))])
                    .with_aggregates(group_aggregates()),
            )
            .with_expand_depth(ExpandDepth::Levels(0))
            .with_totals(PinnedEdge::Bottom)
            .with_task_runner(runner)
        }
    });
    let groups = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.projection().groups().len());
    let state = grid.grid.clone();
    let mut runs = copy_runs_with(&mut grid.harness, &grid.grid, &runner, &|_, cx| {
        state.update(cx, |state, cx| {
            assert!(state.copy_scope(ExportScope::Expanded, cx));
        });
    });
    // The header line is not a row.
    runs.rows -= 1;
    runs.report(&format!(
        "S9b copy all expanded, {groups} collapsed groups, footers none, totals, u64 keys"
    ));
    grid.update_grid(|grid, cx| grid.set_expand_depth(ExpandDepth::All, cx));
    for _ in 0..10 {
        grid.harness.tick();
    }
    let mut runs = copy_runs(&mut grid.harness, &grid.grid, &runner, &CopyAll);
    runs.rows -= 1;
    runs.report(&format!(
        "S9b copy all as shown, {groups} expanded groups, totals, u64 keys"
    ));
}

// ---------------------------------------------------------------------------
// Background costs on real threads, without GPUI.

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = f();
    (value, started.elapsed())
}

/// S11: the quick filter in a background rebuild of the P2 data set, 100k
/// rows sorted by a text column, over the ten S11 columns. Queries of one
/// and three terms that match few rows, many rows and none (every column
/// read). The allocation count is per rebuild: a few vectors, not one per
/// row.
fn scenario_quick_filter_rebuild(pools: &Pools) {
    const RUNS: usize = 30;
    let mut rng = fastrand::Rng::with_seed(11);
    let mut ingest = Ingest::<Quote>::new(HighlightPolicy::default(), Instant::now());
    ingest.apply(
        [Op::ReplaceAll(
            (0..ROWS)
                .map(|id| Arc::new(pools.row(&mut rng, id)))
                .collect(),
        )],
        Instant::now(),
    );
    let snapshot = ingest.snapshot();
    let sorted = ViewSpec::new().with_sort([(Col(COUNTERPARTY), SortDirection::Ascending)]);
    let allocations_of = |spec: &ViewSpec<Quote>| {
        let before = allocations();
        let projection = Rebuild::default().rebuild(&snapshot, spec);
        (allocations() - before, projection.len())
    };
    let (sort_only, _) = allocations_of(&sorted);
    for (name, text) in [
        ("1 term, few rows", "ins042"),
        ("1 term, many rows", "sell"),
        ("1 term, no row (every column read)", "zzz"),
        ("3 terms, few rows", "ins042 sell trader7"),
        ("3 terms, every row", "book counterparty trader"),
    ] {
        let spec = sorted.clone().with_quick_filter(QuickFilter::new(
            text,
            quick_filter_columns(),
            &ContainsAllTerms,
        ));
        let samples: Vec<Duration> = (0..RUNS)
            .map(|_| time(|| Rebuild::default().rebuild(&snapshot, &spec)).1)
            .collect();
        let (allocated, rows) = allocations_of(&spec);
        out!(
            "| S11 quick filter P2, {name}: \"{text}\", {rows} of {ROWS} rows, text sort | p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms; {allocated} allocations per rebuild (sort alone {sort_only}) |",
            ms(percentile(&samples, 0.5)),
            ms(percentile(&samples, 0.99)),
            ms(percentile(&samples, 1.0)),
        );
    }
}

fn background_benchmarks(pools: &Pools) {
    let mut rng = fastrand::Rng::with_seed(11);
    let mut model: Vec<Quote> = (0..ROWS).map(|id| pools.row(&mut rng, id)).collect();
    let epoch = Instant::now();
    let mut ingest = Ingest::<Quote>::new(HighlightPolicy::default(), epoch);

    let (_, load) = time(|| {
        ingest.apply(
            [Op::ReplaceAll(
                model.iter().cloned().map(Arc::new).collect(),
            )],
            Instant::now(),
        )
    });
    out!(
        "| replace_all {ROWS} rows (ingest thread) | {:.1} ms |",
        ms(load)
    );

    // Updates: the key lookup on the write path is an FxHashMap, O(1).
    let held = ingest.snapshot();
    let ops: Vec<Op<Quote>> = (0..10_000)
        .map(|_| {
            let quote = &mut model[rng.usize(..ROWS as usize)];
            mutate(&mut rng, quote);
            Op::Upsert(Arc::new(quote.clone()))
        })
        .collect();
    let (_, apply) = time(|| ingest.apply(ops, Instant::now()));
    out!(
        "| 10k updates, snapshot held | {:.1} ms ({:.2} µs/op) |",
        ms(apply),
        apply.as_secs_f64() * 1e6 / 10_000.
    );
    drop(held);

    // Inserts and removals also maintain the published key index.
    let held = ingest.snapshot();
    let fresh: Vec<Quote> = (ROWS..ROWS + 10_000)
        .map(|id| pools.row(&mut rng, id))
        .collect();
    let (_, inserts) = time(|| {
        ingest.apply(
            fresh
                .iter()
                .cloned()
                .map(|quote| Op::Upsert(Arc::new(quote))),
            Instant::now(),
        )
    });
    let (_, removals) =
        time(|| ingest.apply((ROWS..ROWS + 10_000).map(Op::Remove), Instant::now()));
    out!(
        "| 10k inserts / 10k removals, snapshot held | {:.2} / {:.2} µs/op |",
        inserts.as_secs_f64() * 1e6 / 10_000.,
        removals.as_secs_f64() * 1e6 / 10_000.
    );
    drop(held);

    let (snapshot, publish) = time(|| ingest.snapshot());
    out!("| publish snapshot | {:.3} ms |", ms(publish));

    // Key lookups from a snapshot (UI, producers) vs a plain FxHashMap.
    let keys: Vec<u64> = (0..1_000_000).map(|_| rng.u64(..ROWS)).collect();
    let (found, lookup) = time(|| {
        keys.iter()
            .filter(|key| snapshot.get(key).is_some())
            .count()
    });
    let plain: FxHashMap<u64, usize> = (0..ROWS).map(|id| (id, id as usize)).collect();
    let (_, plain_lookup) = time(|| keys.iter().filter(|key| plain.contains_key(key)).count());
    assert_eq!(found, keys.len());
    out!(
        "| snapshot.get(key), 1M random keys | {:.0} ns/lookup (FxHashMap: {:.0} ns) |",
        lookup.as_secs_f64() * 1e9 / keys.len() as f64,
        plain_lookup.as_secs_f64() * 1e9 / keys.len() as f64
    );

    // Steady state at 10k/s: batches of 166 ops every 16 ms of simulated time,
    // each followed by a publish while the previous snapshot is held.
    let mut held = ingest.snapshot();
    let mut batch_times = Vec::new();
    let mut max_stamp_bytes = 0;
    let mut max_hot = 0;
    for step in 0..3_750u32 {
        let now = epoch + Duration::from_millis(u64::from(step) * 16);
        let ops: Vec<Op<Quote>> = (0..166)
            .map(|_| {
                let quote = &mut model[rng.usize(..ROWS as usize)];
                mutate(&mut rng, quote);
                Op::Upsert(Arc::new(quote.clone()))
            })
            .collect();
        let (_, elapsed) = time(|| {
            ingest.apply(ops, now);
            ingest.sweep(now);
            held = ingest.snapshot();
        });
        batch_times.push(elapsed);
        if step % 100 == 0 {
            max_stamp_bytes = max_stamp_bytes.max(held.stamp_bytes());
            max_hot = max_hot.max(ingest.hot_rows());
        }
    }
    out!(
        "| ingest batch of 166 ops + sweep + publish, 60 s at 10k/s | p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms (CPU share {:.1}%) |",
        ms(percentile(&batch_times, 0.5)),
        ms(percentile(&batch_times, 0.99)),
        ms(percentile(&batch_times, 1.0)),
        batch_times.iter().sum::<Duration>().as_secs_f64() * 100. / 60.,
    );
    out!(
        "| highlight memory at 10k/s, T = 5 s | max {max_hot} hot rows, {:.1} MB of stamps |",
        max_stamp_bytes as f64 / 1e6
    );

    let snapshot = ingest.snapshot();
    let specs: [(&str, ViewSpec<Quote>); 4] = [
        ("insertion order", ViewSpec::new()),
        (
            "float column",
            ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]),
        ),
        (
            "text column",
            ViewSpec::new().with_sort([(Col(34), SortDirection::Ascending)]),
        ),
        (
            "text + float + filter",
            ViewSpec::new()
                .with_sort([
                    (Col(31), SortDirection::Ascending),
                    (Col(2), SortDirection::Descending),
                ])
                .with_filter(ColumnSet::first(COLUMNS), |quote: &Quote| {
                    quote.prices[3] > 100.
                }),
        ),
    ];
    // The default splits the work across cores; one thread shows what the
    // split gains (`Rebuild::with_max_threads(1)`). The second label keeps
    // a prefix of its own for `script/spike_charts.py`.
    let default_threads = Rebuild::default().max_threads();
    for (engine, label) in [
        (Rebuild::default(), String::new()),
        (
            Rebuild::default().with_max_threads(1),
            " on one thread".to_owned(),
        ),
    ] {
        for (name, spec) in &specs {
            let samples: Vec<Duration> = (0..5)
                .map(|_| time(|| engine.rebuild(&snapshot, spec)).1)
                .collect();
            out!(
                "| Rebuild {ROWS} rows{label}, {name} | median {:.1} ms, max {:.1} ms |",
                ms(percentile(&samples, 0.5)),
                ms(percentile(&samples, 1.0))
            );
        }
    }
    out!("| Rebuild threads by default (cores less two, at most eight) | {default_threads} |");

    // Removing 1k rows: compaction (filter, no sort) vs a full rebuild.
    let projection = Rebuild::default().rebuild(&snapshot, &specs[1].1);
    ingest.apply(
        (0..1_000).map(|_| Op::Remove(rng.u64(..ROWS))),
        Instant::now(),
    );
    let after = ingest.snapshot();
    let compact: Vec<Duration> = (0..5)
        .map(|_| time(|| Rebuild::default().compact(&projection, &after)).1)
        .collect();
    let rebuild: Vec<Duration> = (0..5)
        .map(|_| time(|| Rebuild::default().rebuild(&after, &specs[1].1)).1)
        .collect();
    out!(
        "| after 1k removals: compact vs rebuild ({ROWS} rows, float sort) | {:.2} ms vs {:.1} ms |",
        ms(percentile(&compact, 0.5)),
        ms(percentile(&rebuild, 0.5))
    );

    group_benchmarks(pools);
    scenario_tree_rebuild(pools);
}

/// Schema columns of the grouping scenarios: the venue (8 values), the
/// instrument (200) and the side (2); the volume, summed; the last price,
/// averaged; the price the view sorts by, which changes.
const VENUE: u16 = 37;
const INSTRUMENT: u16 = 31;
const BUY_SELL: u16 = 32;
const VOLUME: u16 = 21;
const LAST: u16 = 2;
const CHANGING_PRICE: u16 = 1;

/// S16, S16b, S17 and S18: the grouped rebuild, the aggregate job and the
/// expand job on the P2 data set, on real threads: 300 runs each.
fn group_benchmarks(pools: &Pools) {
    const RUNS: usize = 300;
    let report = |name: &str, samples: &[Duration], notes: &str| {
        out!(
            "| {name} | p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms{notes} |",
            ms(percentile(samples, 0.5)),
            ms(percentile(samples, 0.99)),
            ms(percentile(samples, 1.0)),
        );
    };
    let runs = |runs: usize, job: &mut dyn FnMut(usize) -> Duration| -> Vec<Duration> {
        (0..runs).map(job).collect()
    };

    let mut rng = fastrand::Rng::with_seed(16);
    let mut model: Vec<Quote> = (0..ROWS).map(|id| pools.row(&mut rng, id)).collect();
    let mut ingest = Ingest::<Quote>::new(HighlightPolicy::default(), Instant::now());
    ingest.apply(
        [Op::ReplaceAll(
            model.iter().cloned().map(Arc::new).collect(),
        )],
        Instant::now(),
    );
    let snapshot = ingest.snapshot();
    let engine = Rebuild::default();
    let sort = [(Col(CHANGING_PRICE), SortDirection::Descending)];
    let aggregates = || [(Col(VOLUME), Aggregate::Sum), (Col(LAST), Aggregate::Avg)];

    // S16: one level, against the flat P2 rebuild by the same changing
    // column: the grouping costs its level's sort key and the grouping
    // passes (groups, aggregates, group order, footers, lines).
    let s16 = ViewSpec::new()
        .with_sort(sort)
        .with_group_by([GroupBy::column(Col(VENUE))])
        .with_aggregates(aggregates());
    let groups = engine.rebuild(&snapshot, &s16).groups().len();
    let grouped = runs(RUNS, &mut |_| time(|| engine.rebuild(&snapshot, &s16)).1);
    let flat_view = ViewSpec::new().with_sort(sort);
    let flat = runs(RUNS, &mut |_| {
        time(|| engine.rebuild(&snapshot, &flat_view)).1
    });
    report(
        &format!(
            "S16 Rebuild {ROWS} rows grouped by 1 column ({groups} groups), Sum(Volume), Avg(Last), sort by a changing column"
        ),
        &grouped,
        &format!(
            "; flat, sorted by the same column: p50 {:.1} ms",
            ms(percentile(&flat, 0.5)),
        ),
    );
    let one_thread = Rebuild::default().with_max_threads(1);
    let single = runs(30, &mut |_| time(|| one_thread.rebuild(&snapshot, &s16)).1);
    report("S16 Rebuild on one thread (30 runs)", &single, "");

    // S16b: two levels, every other instrument collapsed.
    let two_levels = ViewSpec::new()
        .with_sort(sort)
        .with_group_by([
            GroupBy::column(Col(INSTRUMENT)),
            GroupBy::column(Col(BUY_SELL)),
        ])
        .with_aggregates(aggregates());
    let projection = engine.rebuild(&snapshot, &two_levels);
    let expansion = projection
        .groups()
        .iter()
        .filter(|group| group.depth() == 0)
        .step_by(2)
        .fold(Expansion::new(ExpandDepth::All), |expansion, group| {
            expansion.with_group(group.path().clone(), false, 2)
        });
    let s16b = two_levels.with_expansion(expansion);
    let projection = engine.rebuild(&snapshot, &s16b);
    let (groups, lines) = (projection.groups().len(), projection.len());
    let samples = runs(RUNS, &mut |_| time(|| engine.rebuild(&snapshot, &s16b)).1);
    report(
        &format!(
            "S16b Rebuild {ROWS} rows grouped by 2 columns ({groups} groups, half the instruments collapsed: {lines} lines)"
        ),
        &samples,
        "",
    );

    // Stamping what changed in the summaries of S16b's 600 groups after 1k
    // updates of their aggregated columns, as the view does at the end of
    // every job; and S21's rebuild with 1000 fading rows.
    let before = engine.rebuild(&snapshot, &s16b);
    let mut stamped_model = model.clone();
    let mut stamping = Ingest::<Quote>::new(HighlightPolicy::default(), Instant::now());
    stamping.apply(
        [Op::ReplaceAll(
            stamped_model.iter().cloned().map(Arc::new).collect(),
        )],
        Instant::now(),
    );
    let ops: Vec<Op<Quote>> = (0..1_000)
        .map(|_| {
            let quote = &mut stamped_model[rng.usize(..ROWS as usize)];
            quote.sizes[usize::from(VOLUME) - 21] += rng.i64(1..100);
            Op::Upsert(Arc::new(quote.clone()))
        })
        .collect();
    stamping.apply(ops, Instant::now());
    let mut after = engine.aggregate(&before, &stamping.snapshot(), &s16b);
    let policy = HighlightPolicy::default();
    let samples = runs(RUNS, &mut |run| {
        time(|| after.carry_summary_stamps(&before, 1_000 + run as u32, policy)).1
    });
    let hot = after
        .groups()
        .iter()
        .filter(|group| after.group_stamps(group.id()).is_some())
        .count();
    report(
        &format!(
            "S16b carry_summary_stamps over {} groups after 1k updates of Volume",
            after.groups().len()
        ),
        &samples,
        &format!("; {hot} groups with a hot aggregate"),
    );
    let mut fading = Ingest::<Quote>::new(
        HighlightPolicy::default().with_removal_fade(Duration::from_secs(1)),
        Instant::now(),
    );
    fading.apply(
        [Op::ReplaceAll(
            model.iter().cloned().map(Arc::new).collect(),
        )],
        Instant::now(),
    );
    fading.apply(
        (0..1_000u64).map(|ix| Op::Remove(ix * (ROWS / 1_000))),
        Instant::now(),
    );
    let with_fading = fading.snapshot();
    let fading_rows = with_fading.fading_rows().count();
    let samples = runs(RUNS, &mut |_| time(|| engine.rebuild(&with_fading, &s16)).1);
    report(
        &format!("S21 Rebuild of S16 with {fading_rows} fading rows"),
        &samples,
        &format!(
            "; {} lines, budget p99 30 ms",
            engine.rebuild(&with_fading, &s16).len()
        ),
    );

    // S17: the aggregated columns change, and nothing the order reads.
    let projection = engine.rebuild(&snapshot, &s16);
    let dependencies = s16.dependencies();
    assert!(
        !dependencies.contains(ColumnIx::new(VOLUME))
            && !dependencies.contains(ColumnIx::new(LAST))
    );
    let ops: Vec<Op<Quote>> = (0..1_000)
        .map(|_| {
            let quote = &mut model[rng.usize(..ROWS as usize)];
            quote.sizes[usize::from(VOLUME) - 21] += rng.i64(-100..100);
            quote.prices[usize::from(LAST) - 1] *= 1. + (rng.f64() - 0.5) / 100.;
            Op::Upsert(Arc::new(quote.clone()))
        })
        .collect();
    ingest.apply(ops, Instant::now());
    let changed = ingest.snapshot();
    let samples = runs(RUNS, &mut |_| {
        time(|| engine.aggregate(&projection, &changed, &s16)).1
    });
    report(
        "S17 aggregate job on the S16 projection after 1k updates of Volume and Last",
        &samples,
        "",
    );

    // S18: expanding and collapsing a group of half the rows, and
    // everything at once over 1000 groups.
    let by_side = ViewSpec::new()
        .with_sort(sort)
        .with_group_by([GroupBy::column(Col(BUY_SELL))])
        .with_aggregates(aggregates());
    let collapsed = by_side
        .clone()
        .with_expansion(Expansion::new(ExpandDepth::Levels(0)));
    let shut = engine.rebuild(&changed, &collapsed);
    let first = shut
        .groups()
        .get(shut.groups().iter().next().unwrap().id())
        .unwrap();
    let (path, rows) = (first.path().clone(), first.row_count());
    let opened =
        by_side.with_expansion(Expansion::new(ExpandDepth::Levels(0)).with_group(path, true, 1));
    let open = engine.expand(&shut, &changed, &opened);
    let samples = runs(RUNS, &mut |run| {
        if run % 2 == 0 {
            time(|| engine.expand(&shut, &changed, &opened)).1
        } else {
            time(|| engine.expand(&open, &changed, &collapsed)).1
        }
    });
    report(
        &format!("S18 expand job: expand and collapse a group of {rows} rows"),
        &samples,
        "",
    );
    let bucket = GroupBy::key(GroupKey::new(
        "bucket",
        ColumnSet::first(1),
        |quote: &Quote| CellValue::Int((quote.id % 1_000) as i64),
    ));
    let buckets = ViewSpec::new()
        .with_sort(sort)
        .with_group_by([bucket])
        .with_aggregates(aggregates());
    let all = buckets
        .clone()
        .with_expansion(Expansion::new(ExpandDepth::All));
    let none = buckets.with_expansion(Expansion::new(ExpandDepth::Levels(0)));
    let expanded = engine.rebuild(&changed, &all);
    let collapsed = engine.expand(&expanded, &changed, &none);
    let groups = expanded.groups().len();
    let samples = runs(RUNS, &mut |run| {
        if run % 2 == 0 {
            time(|| engine.expand(&expanded, &changed, &none)).1
        } else {
            time(|| engine.expand(&collapsed, &changed, &all)).1
        }
    });
    report(
        &format!("S18 expand job: expand all and collapse all over {groups} groups"),
        &samples,
        "",
    );
}

/// S19's row: a P2 quote with a parent, so the rows form a tree.
#[derive(Clone)]
struct TreeQuote {
    quote: Quote,
    parent: Option<u64>,
}

impl GridRow for TreeQuote {
    type Key = u64;
    type Column = Col;

    fn key(&self) -> &u64 {
        &self.quote.id
    }

    fn value(&self, column: Col) -> CellValue<'_> {
        self.quote.value(column)
    }

    fn parent_key(&self) -> Option<&u64> {
        self.parent.as_ref()
    }
}

/// S19 (background): the P2 rows as a tree of depth 5 (200 funds, 800
/// desks, 3k books, 16k sub-books, 80k positions: about 20k branches),
/// written in a random order; 10k updates a second to the sort column
/// between rebuilds at 10 Hz; a quick filter on the instrument (5% of the
/// rows) with `KeepAncestors`. Budget: `Rebuild` p99 ≤ 40 ms.
fn scenario_tree_rebuild(pools: &Pools) {
    const LEVELS: [u64; 5] = [200, 800, 3_000, 16_000, 80_000];
    const RUNS: usize = 100;
    let mut rng = fastrand::Rng::with_seed(19);
    let mut model: Vec<TreeQuote> = Vec::with_capacity(ROWS as usize);
    let mut above = 0..0;
    for count in LEVELS {
        let level = model.len() as u64..model.len() as u64 + count;
        for id in level.clone() {
            model.push(TreeQuote {
                quote: pools.row(&mut rng, id),
                parent: (!above.is_empty()).then(|| rng.u64(above.clone())),
            });
        }
        above = level;
    }
    let branches = model
        .iter()
        .filter_map(|row| row.parent)
        .collect::<FxHashSet<u64>>()
        .len();
    let mut arrival: Vec<Arc<TreeQuote>> = model.iter().cloned().map(Arc::new).collect();
    rng.shuffle(&mut arrival);
    let mut ingest = Ingest::<TreeQuote>::new(HighlightPolicy::default(), Instant::now());
    ingest.apply([Op::ReplaceAll(arrival)], Instant::now());

    let mut instrument = ColumnSet::EMPTY;
    instrument.insert(ColumnIx::new(INSTRUMENT));
    let spec = ViewSpec::new()
        .with_sort([(Col(CHANGING_PRICE), SortDirection::Descending)])
        .with_quick_filter(QuickFilter::new("ins04", instrument, &ContainsAllTerms))
        .with_tree(TreeSpec::new());
    let engine = Rebuild::default();
    let mut samples = Vec::with_capacity(RUNS);
    let mut shown = 0;
    for _ in 0..RUNS {
        // 100 ms of 10k updates a second, to the sort column.
        ingest.apply(
            (0..1_000).map(|_| {
                let ix = rng.usize(..model.len());
                let row = &mut model[ix];
                row.quote.prices[usize::from(CHANGING_PRICE) - 1] = rng.f64() * 1_000.;
                Op::Upsert(Arc::new(row.clone()))
            }),
            Instant::now(),
        );
        let snapshot = ingest.snapshot();
        let (projection, elapsed) = time(|| engine.rebuild(&snapshot, &spec));
        shown = projection.row_count();
        samples.push(elapsed);
    }
    out!(
        "| S19 tree {ROWS} rows, depth 5, {branches} branches, 10k updates/s, float sort, quick filter with KeepAncestors ({shown} rows shown) | p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms (budget p99 ≤ 40 ms) |",
        ms(percentile(&samples, 0.5)),
        ms(percentile(&samples, 0.99)),
        ms(percentile(&samples, 1.0)),
    );
}

// ---------------------------------------------------------------------------
// The tree on screen.

/// S19's grid: the P2 columns over tree rows.
struct TreeBlotter;

impl LiveGridDelegate for TreeBlotter {
    type Row = TreeQuote;
}

struct TreeGridRoot {
    grid: Entity<LiveGridState<TreeBlotter>>,
    samples: Samples,
}

impl Render for TreeGridRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Timed::new(
            div()
                .size_full()
                .child(LiveGrid::new(&self.grid).xsmall().bordered(false)),
            self.samples.clone(),
        )
    }
}

/// The rows of S19: 100k P2 quotes as a tree of depth 5 (200 funds, 800
/// desks, 3k books, 16k sub-books, 80k positions: about 20k branches), each
/// under a random row of the level above, as the background S19.
fn tree_model(pools: &Pools, rng: &mut fastrand::Rng) -> Vec<TreeQuote> {
    const LEVELS: [u64; 5] = [200, 800, 3_000, 16_000, 80_000];
    let mut model: Vec<TreeQuote> = Vec::with_capacity(ROWS as usize);
    let mut above = 0..0;
    for count in LEVELS {
        let level = model.len() as u64..model.len() as u64 + count;
        for id in level.clone() {
            model.push(TreeQuote {
                quote: pools.row(rng, id),
                parent: (!above.is_empty()).then(|| rng.u64(above.clone())),
            });
        }
        above = level;
    }
    model
}

/// S19 (frame): the S19 tree on screen, everything expanded, sorted by a
/// changing column under 10k updates a second (166 a frame on random rows,
/// the sort column among them), the tree column (Id) frozen on the left;
/// with `filtered`, a quick filter on the instrument (5% of the rows) with
/// `KeepAncestors`. Budget: frame p99 ≤ 12 ms.
fn scenario_tree_frames(pools: &Pools, filtered: bool) {
    let mut cx = headless();
    let mut rng = fastrand::Rng::with_seed(19);
    let mut model = tree_model(pools, &mut rng);
    let branches = model
        .iter()
        .filter_map(|row| row.parent)
        .collect::<FxHashSet<u64>>()
        .len();
    let samples: Samples = Rc::default();
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(window_options(), cx, |_, cx| {
                cx.new(|cx| TreeGridRoot {
                    grid: cx.new(|cx| {
                        LiveGridState::new_standalone(
                            schema(),
                            TreeBlotter,
                            StoreOptions::default().with_highlight(HighlightPolicy::default()),
                            cx,
                        )
                        .with_view(
                            ViewSpec::new()
                                .with_sort([(Col(CHANGING_PRICE), SortDirection::Descending)])
                                .with_tree(TreeSpec::new().with_column(Col(0))),
                        )
                        .with_quick_filter_columns([Col(INSTRUMENT)])
                    }),
                    samples: samples.clone(),
                })
            })
        })
        .expect("open headless window");
    let grid = cx.read_entity(&root, |root, _| root.grid.clone());
    let writer = cx.read_entity(&grid, |grid, _| grid.writer());
    let mut arrival: Vec<Arc<TreeQuote>> = model.iter().cloned().map(Arc::new).collect();
    rng.shuffle(&mut arrival);
    writer.replace_all(arrival);
    if filtered {
        cx.update(|cx| grid.update(cx, |grid, cx| grid.set_quick_filter("ins04", cx)));
    }
    let mut harness = Harness {
        cx,
        window,
        samples,
    };
    // Load, rebuild and warm caches, then start measuring.
    for _ in 0..30 {
        harness.tick();
    }
    harness.take();
    for _ in 0..FRAMES {
        for _ in 0..166 {
            let ix = rng.usize(..model.len());
            let row = &mut model[ix];
            mutate(&mut rng, &mut row.quote);
            row.quote.prices[usize::from(CHANGING_PRICE) - 1] = rng.f64() * 1_000.;
            writer.upsert(row.clone());
        }
        harness.tick();
    }
    let samples = harness.take();
    let notes = harness.cx.read_entity(&grid, |grid, _| {
        let (stats, text) = (grid.stats(), grid.text_stats());
        let projection = grid.projection();
        format!(
            "{branches} branches, {} rows pass, {} lines, cells {}, icons {}, prepaint {:.2} ms, rebuilds {} (max {:.1} ms), tree cycles {}, text misses {} of {} lookups",
            projection.row_count(),
            projection.len(),
            stats.visible_cells(),
            stats.icons(),
            ms(stats.last_prepaint()),
            stats.rebuilds(),
            ms(stats.max_rebuild()),
            stats.tree_cycles(),
            text.misses(),
            text.hits() + text.misses() + text.numeric(),
        )
    });
    let name = if filtered {
        "S19 tree 100k rows, depth 5, 10k updates/s, sort by a changing column, quick filter with KeepAncestors, tree column frozen left"
    } else {
        "S19 tree 100k rows, depth 5, 10k updates/s, sort by a changing column, all expanded, tree column frozen left"
    };
    harness.report(name, FRAMES, &samples, &notes);
}

// ---------------------------------------------------------------------------
// Groups on screen.

/// The aggregates of S16 to S18: Sum(Volume), Avg(Last).
fn group_aggregates() -> [(Col, Aggregate<Quote>); 2] {
    [(Col(VOLUME), Aggregate::Sum), (Col(LAST), Aggregate::Avg)]
}

/// The notes of a grouped scenario: the shared ones, the captions painted
/// in the last frame and the text cache's misses.
fn group_notes(grid: &mut GridHarness) -> String {
    let notes = grid.notes();
    grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        let (stats, text) = (grid.stats(), grid.text_stats());
        format!(
            "{notes}, captions painted {}, text misses {} of {} lookups",
            stats.painted_captions(),
            text.misses(),
            text.hits() + text.misses() + text.numeric(),
        )
    })
}

/// S16 (frame): the P2 load (10k updates/s on random keys, sorted by a
/// changing column) grouped by one column, 10 venues, with Sum(Volume) and
/// Avg(Last) on the captions, everything expanded. With `a11y`, the
/// accessibility tree is planned too. Returns the draws' p50.
fn scenario_grouped_p2(pools: &Pools, a11y: bool) -> Duration {
    let mut grid = GridHarness::new(
        pools,
        ViewSpec::new()
            .with_sort([(Col(CHANGING_PRICE), SortDirection::Descending)])
            .with_group_by([GroupBy::column(Col(VENUE))])
            .with_aggregates(group_aggregates()),
    );
    if a11y {
        grid.update_grid(|grid, _| grid.set_a11y_forced(true));
        for _ in 0..5 {
            grid.harness.tick();
        }
        grid.harness.take();
    }
    for _ in 0..FRAMES {
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let groups = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.projection().groups().len());
    let notes = format!("{groups} groups, {}", group_notes(&mut grid));
    let name = if a11y {
        "S16 P2 grouped by 1 column, Sum and Avg, all expanded, accessibility planned (forced)"
    } else {
        "S16 P2 grouped by 1 column, Sum and Avg, all expanded"
    };
    grid.harness.report(name, FRAMES, &samples, &notes);
    percentile(&samples, 0.5)
}

/// S16b (frame): the P2 load grouped by instrument and side (500 groups),
/// every other instrument collapsed: many captions on screen.
fn scenario_grouped_two_levels(pools: &Pools) {
    let mut grid = GridHarness::new(
        pools,
        ViewSpec::new()
            .with_sort([(Col(CHANGING_PRICE), SortDirection::Descending)])
            .with_group_by([
                GroupBy::column(Col(INSTRUMENT)),
                GroupBy::column(Col(BUY_SELL)),
            ])
            .with_aggregates(group_aggregates()),
    );
    let collapsed: Vec<_> = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        grid.projection()
            .groups()
            .iter()
            .filter(|group| group.depth() == 0)
            .step_by(2)
            .map(|group| group.path().clone())
            .collect()
    });
    grid.update_grid(|grid, cx| {
        for path in collapsed {
            grid.set_expanded(ExpandTarget::Group(path), false, cx);
        }
    });
    for _ in 0..10 {
        grid.harness.tick();
    }
    grid.harness.take();
    for _ in 0..FRAMES {
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let (groups, lines) = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        (grid.projection().groups().len(), grid.projection().len())
    });
    let notes = format!("{groups} groups, {lines} lines, {}", group_notes(&mut grid));
    grid.harness.report(
        "S16b P2 grouped by 2 columns, half the instruments collapsed",
        FRAMES,
        &samples,
        &notes,
    );
}

/// S23: S16 (the P2 load grouped by one column, 10 venues, Sum(Volume) and
/// Avg(Last)) with a footer under every group and the totals row at the
/// bottom, scrolled to the end of the first group: its last rows, its
/// footer and the next caption are on screen with the totals. Notes report
/// the footers on screen and the bold text cache.
fn scenario_totals_footers(pools: &Pools) {
    let mut grid = GridHarness::configured(pools, |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Col(CHANGING_PRICE), SortDirection::Descending)])
                .with_group_by([GroupBy::column(Col(VENUE))])
                .with_aggregates(group_aggregates())
                .with_group_footer(EveryGroupFooter),
        )
        .with_totals(PinnedEdge::Bottom)
    });
    // The second group's caption 20 lines down: the first group's footer
    // is above it. P2 updates rows in place, so group sizes stay.
    let line = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        let projection = grid.projection();
        let second = projection
            .groups()
            .iter()
            .filter(|group| group.depth() == 0)
            .nth(1)?;
        projection.group_line(second.id())
    });
    if let Some(line) = line {
        grid.update_grid(|grid, cx| grid.scroll_to_row(line.saturating_sub(20), cx));
    }
    for _ in 0..10 {
        grid.harness.tick();
    }
    grid.harness.take();
    for _ in 0..FRAMES {
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    let (groups, footers, bold) = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        let projection = grid.projection();
        let footers = grid
            .visible_rows()
            .filter(|ix| matches!(projection.line(*ix), Some(DisplayRow::GroupFooter { .. })))
            .count();
        (
            projection.groups().len(),
            footers,
            grid.text_stats().bold_entries(),
        )
    });
    let notes = format!(
        "{groups} groups, footers on screen {footers}, totals at the bottom, bold text entries {bold}, {}",
        group_notes(&mut grid)
    );
    grid.harness.report(
        "S23 S16 with a footer under every group and the totals row at the bottom",
        FRAMES,
        &samples,
        &notes,
    );
}

/// S21: the P2 load grouped by one column as S16, with ~1k removals/s (a
/// third of them on painted rows, as S7) and ~1k inserts/s, a removal fade
/// of 1 s and `LiveReorder`: removed rows fade on their lines and stay in
/// the order through rebuilds until their fade ends. Painted rows go faster
/// than they fade, so the screen soon shows fading lines almost only: the
/// frame's worst case. Budgets: draws p99 ≤ 12 ms, `Rebuild` p99 ≤ 30 ms,
/// at most 1.2k fading rows in a snapshot.
fn scenario_fading_grouped(pools: &Pools) {
    let options = StoreOptions::default()
        .with_highlight(HighlightPolicy::default().with_removal_fade(Duration::from_secs(1)));
    let mut grid = GridHarness::configured_with_store(pools, options, false, |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Col(CHANGING_PRICE), SortDirection::Descending)])
                .with_group_by([GroupBy::column(Col(VENUE))])
                .with_aggregates(group_aggregates()),
        )
        .with_reorder_policy(LiveReorder)
    });
    let writer = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.writer());
    let mut rebuilds = Vec::new();
    let mut seen_rebuilds = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.stats().rebuilds());
    let (mut max_fading, mut max_lines, mut painted_lines) = (0, 0, 0);
    for _ in 0..FRAMES {
        let ids: Vec<u64> = (0..166)
            .map(|_| grid.live[grid.rng.usize(..grid.live.len())])
            .collect();
        grid.write(ids);
        let visible = grid.visible_keys();
        for ix in 0..17 {
            let key = if ix % 3 == 0 && !visible.is_empty() {
                visible[grid.rng.usize(..visible.len())]
            } else {
                grid.live[grid.rng.usize(..grid.live.len())]
            };
            if let Some(position) = grid.live.iter().position(|id| *id == key) {
                grid.live.swap_remove(position);
                writer.remove(key);
            }
            let id = grid.model.len() as u64;
            let quote = pools.row(&mut grid.rng, id);
            grid.model.push(quote.clone());
            grid.live.push(id);
            writer.upsert(quote);
        }
        grid.harness.tick();
        let (stats, fading) = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
            (grid.stats(), grid.snapshot().fading_rows().count())
        });
        if stats.rebuilds() != seen_rebuilds {
            seen_rebuilds = stats.rebuilds();
            rebuilds.push(stats.last_rebuild());
        }
        max_fading = max_fading.max(fading);
        max_lines = max_lines.max(stats.fading_lines());
        painted_lines += stats.fading_lines();
    }
    let samples = grid.harness.take();
    let notes = format!(
        "Rebuild p50 {:.1} ms, p99 {:.1} ms (budget 30 ms); fading rows in a snapshot at most {max_fading} (budget 1200); fading lines painted per frame at most {max_lines}, {:.1} on average; {}",
        ms(percentile(&rebuilds, 0.5)),
        ms(percentile(&rebuilds, 0.99)),
        painted_lines as f64 / FRAMES as f64,
        group_notes(&mut grid),
    );
    grid.harness.report(
        "S21 P2 grouped by 1 column + 1k/s removals fading out for 1 s + 1k/s inserts, LiveReorder",
        FRAMES,
        &samples,
        &notes,
    );
}

/// S18 (frame): the frames that show an expand job. A group of half the
/// rows (by side) expanded and collapsed again and again; then every one of
/// 1000 groups (a key of the row) expanded and collapsed at once. No other
/// load: every draw shows a new expansion, or the chevron's turn before it.
fn scenario_expand_frames(pools: &Pools) {
    const TOGGLES: usize = 60;
    let sort = [(Col(CHANGING_PRICE), SortDirection::Descending)];

    let mut grid = GridHarness::configured(pools, move |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort(sort)
                .with_group_by([GroupBy::column(Col(BUY_SELL))])
                .with_aggregates(group_aggregates()),
        )
        .with_expand_depth(ExpandDepth::Levels(0))
    });
    let (path, rows) = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        let group = grid.projection().groups().iter().next().expect("a group");
        (group.path().clone(), group.row_count())
    });
    for toggle in 0..TOGGLES {
        let target = ExpandTarget::Group(path.clone());
        grid.update_grid(|grid, cx| {
            grid.set_expanded(target, toggle % 2 == 0, cx);
        });
        for _ in 0..3 {
            grid.harness.tick();
        }
    }
    let samples = grid.harness.take();
    let notes = format!(
        "{TOGGLES} toggles, {} draws, {}",
        samples.len(),
        group_notes(&mut grid)
    );
    grid.harness.report(
        &format!("S18 frames showing expand and collapse of a group of {rows} rows"),
        TOGGLES * 3,
        &samples,
        &notes,
    );

    let bucket = GroupBy::key(GroupKey::new(
        "bucket",
        ColumnSet::first(1),
        |quote: &Quote| CellValue::Int((quote.id % 1_000) as i64),
    ));
    let mut grid = GridHarness::new(
        pools,
        ViewSpec::new()
            .with_sort(sort)
            .with_group_by([bucket])
            .with_aggregates(group_aggregates()),
    );
    for toggle in 0..TOGGLES {
        let depth = if toggle % 2 == 0 {
            ExpandDepth::Levels(0)
        } else {
            ExpandDepth::All
        };
        grid.update_grid(|grid, cx| grid.set_expand_depth(depth, cx));
        for _ in 0..3 {
            grid.harness.tick();
        }
    }
    let samples = grid.harness.take();
    let groups = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.projection().groups().len());
    let notes = format!(
        "{TOGGLES} toggles, {} draws, {}",
        samples.len(),
        group_notes(&mut grid)
    );
    grid.harness.report(
        &format!("S18 frames showing expand all and collapse all over {groups} groups"),
        TOGGLES * 3,
        &samples,
        &notes,
    );
}

/// S10b: the accessibility plan of a grouped frame. Two levels, blocks of
/// 12 rows by side, so 54 lines show 10 or 11 captions; three aggregates;
/// the focus on a caption; the S1 load on painted rows. `last_a11y_plan`
/// times the plan and every node GPUI would build from it (budget
/// p99 ≤ 1 ms).
fn scenario_a11y_grouped(pools: &Pools, s10_p50: Duration) {
    let block = GroupBy::key(GroupKey::new(
        "block",
        ColumnSet::first(1),
        |quote: &Quote| CellValue::Int((quote.id / 12) as i64),
    ));
    let mut grid = GridHarness::configured(pools, move |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Col(0), SortDirection::Ascending)])
                .with_group_by([block, GroupBy::column(Col(BUY_SELL))])
                .with_aggregates([
                    (Col(VOLUME), Aggregate::Sum),
                    (Col(LAST), Aggregate::Avg),
                    (Col(CHANGING_PRICE), Aggregate::Max),
                ]),
        )
    });
    grid.update_grid(|grid, _| grid.set_a11y_forced(true));
    let first = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
        grid.projection()
            .groups()
            .iter()
            .nth(1)
            .expect("a subgroup")
            .path()
            .clone()
    });
    grid.update_grid(|grid, cx| {
        grid.set_focused_group(&first, cx);
    });
    let handle = grid.grid.clone();
    grid.harness
        .cx
        .update_window(grid.harness.window, |_, window, cx| {
            let handle = handle.focus_handle(cx);
            window.focus(&handle, cx);
        })
        .expect("window is open");
    for _ in 0..5 {
        grid.harness.tick();
    }
    grid.harness.take();
    let stats = |grid: &mut GridHarness| {
        grid.harness
            .cx
            .read_entity(&grid.grid, |grid, _| grid.stats())
    };
    let mut plans = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let visible = grid.visible_keys();
        let ids: Vec<u64> = (0..33)
            .map(|_| visible[grid.rng.usize(..visible.len())])
            .collect();
        grid.write(ids);
        let before = stats(&mut grid).frames();
        grid.harness.tick();
        let after = stats(&mut grid);
        if after.frames() != before {
            plans.push(after.last_a11y_plan());
        }
    }
    let samples = grid.harness.take();
    let last = stats(&mut grid);
    let notes = format!(
        "plan and nodes p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms (budget p99 1 ms); {} rows ({} captions), {} synthetic cells and the focused caption; draw p50 {:+.2} ms over S10; {}",
        ms(percentile(&plans, 0.5)),
        ms(percentile(&plans, 0.99)),
        ms(percentile(&plans, 1.0)),
        last.a11y_rows(),
        last.painted_captions(),
        last.a11y_cells(),
        ms(percentile(&samples, 0.5)) - ms(s10_p50),
        group_notes(&mut grid),
    );
    grid.harness.report(
        "S10b grouped view with the accessibility tree planned (forced), a caption focused",
        FRAMES,
        &samples,
        &notes,
    );
}

// ---------------------------------------------------------------------------
// S24: update rate sweep. The P2 data set (100k rows, random keys) under
// faster streams than P2's 10k updates/s, to find where it breaks.

/// The rates of S24's ingest sweep, in updates per second.
const S24_RATES: [u32; 6] = [10_000, 15_000, 20_000, 50_000, 100_000, 200_000];
/// The rate of S24's frames.
const S24_FRAME_RATE: u32 = 100_000;
/// The store loop's pauses after a publish at [`S24_FRAME_RATE`]: the
/// former default publish interval (4 ms), the grid's pull interval and the
/// current default (8 ms), a frame (16 ms).
const S24_PAUSES: [Duration; 3] = [
    Duration::from_millis(4),
    Duration::from_millis(8),
    Duration::from_millis(16),
];
/// Simulated time per rate of the ingest sweep.
const S24_SECONDS: u32 = 10;
/// A batch every 1/60 s, as the 60 s measurement at 10k/s.
const S24_BATCH_INTERVAL: Duration = Duration::from_micros(16_667);
/// Slots per store page. `replace_all` fills an empty store in key order,
/// so the slot of a row is its key and its page is `key / STORE_PAGE`.
const STORE_PAGE: usize = 1024;
const _: () = assert!((ROWS as usize).div_ceil(STORE_PAGE) <= 128);

/// S24's budgets: the ingest's share of one core (%), one ingest batch,
/// change stamps (MB), frames p99 (flat, grouped) and rebuilds p99.
const S24_INGEST_BUDGET: f64 = 50.;
const S24_BATCH_BUDGET: Duration = Duration::from_millis(16);
const S24_STAMP_BUDGET: f64 = 10.;
const S24_FLAT_FRAME_BUDGET: Duration = Duration::from_millis(8);
const S24_GROUPED_FRAME_BUDGET: Duration = Duration::from_millis(12);
const S24_REBUILD_BUDGET: Duration = Duration::from_millis(30);

/// The 1, 5 and 15 minute load averages of the machine, for the notes:
/// S24 runs while other work may load it.
fn load_average() -> String {
    let text = std::fs::read_to_string("/proc/loadavg").ok().or_else(|| {
        std::process::Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
    });
    text.map_or_else(
        || "n/a".to_owned(),
        |text| {
            text.split_whitespace()
                .filter(|part| part.parse::<f64>().is_ok())
                .take(3)
                .collect::<Vec<_>>()
                .join(" ")
        },
    )
}

/// When S24's ingest takes a batch.
#[derive(Clone, Copy)]
enum Cadence {
    /// Every interval of simulated time, however long a batch takes.
    Every(Duration),
    /// As the store's ingest task under a steady stream: it pauses for the
    /// publish interval after each publish and then takes everything
    /// written meanwhile, so a batch follows the previous one by the pause
    /// plus that batch's own time.
    StoreLoop(Duration),
}

impl Cadence {
    /// The simulated time from the previous batch, which took `previous`,
    /// to the next.
    fn period(self, previous: Duration) -> Duration {
        match self {
            Self::Every(interval) => interval,
            Self::StoreLoop(pause) => pause + previous,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Every(_) => String::new(),
            Self::StoreLoop(pause) => format!(", store loop ({} ms pause)", pause.as_millis()),
        }
    }
}

/// One rate of S24's ingest sweep: the batches and their parts.
#[derive(Default)]
struct IngestRun {
    rate: u32,
    load: String,
    batches: Vec<Duration>,
    /// Operations of every batch, and the simulated time they span.
    ops: u64,
    seconds: f64,
    /// Totals of the four parts of every batch.
    apply: Duration,
    sweep: Duration,
    publish: Duration,
    release: Duration,
    /// The slowest sweep: one batch every half highlight duration frees
    /// the expired stamps.
    sweep_max: Duration,
    /// Pages the batches' updates fell on, each copied once by `apply`
    /// while the previous snapshot holds it.
    pages: u64,
    allocations: u64,
    max_hot: usize,
    max_stamp_bytes: usize,
}

impl IngestRun {
    /// The ingest's share of one core, in %.
    fn cpu_share(&self) -> f64 {
        self.batches.iter().sum::<Duration>().as_secs_f64() * 100. / self.seconds
    }

    fn batch_max(&self) -> f64 {
        ms(percentile(&self.batches, 1.0))
    }

    fn stamp_mb(&self) -> f64 {
        self.max_stamp_bytes as f64 / 1e6
    }

    fn report(&self, label: &str) {
        let batches = self.batches.len().max(1) as f64;
        let ops = self.ops.max(1) as f64;
        let per_batch = |total: Duration| ms(total) / batches;
        out!(
            "| S24 ingest {}k/s{label}: {:.0} ops every {:.1} ms | {} | {:.1}% | {:.2} | {:.2} | {:.2} | {:.3} / {:.3} / {:.3} / {:.3} | {:.1} | {:.2} | {:.2} | {:.1} | {} | {:.1} MB |",
            self.rate / 1_000,
            ops / batches,
            self.seconds * 1e3 / batches,
            self.load,
            self.cpu_share(),
            ms(percentile(&self.batches, 0.5)),
            ms(percentile(&self.batches, 0.99)),
            self.batch_max(),
            per_batch(self.apply),
            per_batch(self.sweep),
            per_batch(self.publish),
            per_batch(self.release),
            self.pages as f64 / batches,
            self.apply.as_secs_f64() * 1e6 / ops,
            self.allocations as f64 / ops,
            ms(self.sweep_max),
            self.max_hot,
            self.stamp_mb(),
        );
    }
}

/// S24, ingest: `rate` updates/s on random keys of the P2 data set for
/// [`S24_SECONDS`] of simulated time, a batch at each `cadence`, each
/// followed by a sweep and a publish while the previous snapshot is held,
/// as the 60 s measurement at 10k/s. The parts of a batch are timed apart:
/// `apply` (with the copy of every page the batch writes to), `sweep`,
/// `publish` (the snapshot) and `release` (dropping the previous snapshot,
/// with the pages and stamps it held alone). The default `HighlightPolicy`
/// (5 s). Hot rows are sampled after every batch, so the peak before a
/// sweep is seen.
fn ingest_at_rate(pools: &Pools, rate: u32, cadence: Cadence) -> IngestRun {
    let mut rng = fastrand::Rng::with_seed(24);
    let mut model: Vec<Quote> = (0..ROWS).map(|id| pools.row(&mut rng, id)).collect();
    let epoch = Instant::now();
    let mut ingest = Ingest::<Quote>::new(HighlightPolicy::default(), epoch);
    ingest.apply(
        [Op::ReplaceAll(
            model.iter().cloned().map(Arc::new).collect(),
        )],
        epoch,
    );
    let mut held = ingest.snapshot();
    let mut run = IngestRun {
        rate,
        load: load_average(),
        ..IngestRun::default()
    };
    let end = Duration::from_secs(u64::from(S24_SECONDS));
    let (mut elapsed, mut previous, mut owed) = (Duration::ZERO, Duration::ZERO, 0.);
    loop {
        let period = cadence.period(previous);
        if elapsed + period > end {
            break;
        }
        elapsed += period;
        // The writes of the period, carrying the fraction of an operation.
        owed += f64::from(rate) * period.as_secs_f64();
        let count = owed as usize;
        owed -= count as f64;
        let now = epoch + elapsed;
        let mut pages = 0u128;
        let ops: Vec<Op<Quote>> = (0..count)
            .map(|_| {
                let id = rng.usize(..ROWS as usize);
                pages |= 1 << (id / STORE_PAGE);
                let quote = &mut model[id];
                mutate(&mut rng, quote);
                Op::Upsert(Arc::new(quote.clone()))
            })
            .collect();
        let allocated = allocations();
        let (_, apply) = time(|| ingest.apply(ops, now));
        run.allocations += allocations() - allocated;
        let (_, sweep) = time(|| ingest.sweep(now));
        let (snapshot, publish) = time(|| ingest.snapshot());
        let (_, release) = time(|| drop(std::mem::replace(&mut held, snapshot)));
        previous = apply + sweep + publish + release;
        run.batches.push(previous);
        run.ops += count as u64;
        run.apply += apply;
        run.sweep += sweep;
        run.publish += publish;
        run.release += release;
        run.sweep_max = run.sweep_max.max(sweep);
        run.pages += u64::from(pages.count_ones());
        let hot = ingest.hot_rows();
        if hot > run.max_hot {
            run.max_hot = hot;
            run.max_stamp_bytes = held.stamp_bytes();
        }
    }
    run.seconds = elapsed.as_secs_f64();
    run
}

/// S24, ingest sweep: [`ingest_at_rate`] at each of [`S24_RATES`], first a
/// batch every 1/60 s, then as the store's ingest task takes them (a pause
/// of the default `publish_interval`, 8 ms, after each publish). Then the
/// store loop at 100k/s with the other pauses of [`S24_PAUSES`], and what
/// one page copy costs. Returns the runs of both sweeps.
fn scenario_ingest_sweep(pools: &Pools) -> (Vec<IngestRun>, Vec<IngestRun>) {
    out!(
        "| Rate | Load (1, 5, 15 min) | CPU share of one core | Batch p50, ms | p99, ms | max, ms | Apply / sweep / publish / release, ms per batch | Pages copied per batch | Apply, µs per op | Allocations per op | Sweep max, ms | Hot rows max | Stamps max |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    let sweep = |cadence: Cadence| -> Vec<IngestRun> {
        S24_RATES
            .iter()
            .map(|&rate| {
                let run = ingest_at_rate(pools, rate, cadence);
                run.report(&cadence.label());
                run
            })
            .collect()
    };
    let every_frame = sweep(Cadence::Every(S24_BATCH_INTERVAL));
    let store_loop = sweep(Cadence::StoreLoop(
        StoreOptions::default().publish_interval(),
    ));
    // A longer pause takes fewer, larger batches: each copies the pages
    // once.
    for pause in S24_PAUSES {
        if pause == StoreOptions::default().publish_interval() {
            continue;
        }
        let cadence = Cadence::StoreLoop(pause);
        ingest_at_rate(pools, S24_FRAME_RATE, cadence).report(&cadence.label());
    }
    out!("\n| Measurement | Result |\n|---|---|");
    page_copy_probe(pools);
    (every_frame, store_loop)
}

/// S24: what the copy of a page costs. One update on each page of the
/// store, applied and published, with the previous snapshot held (`apply`
/// copies every page and dropping the old snapshot frees them) and with it
/// dropped before the batch (the pages are the ingest's alone: no copy).
fn page_copy_probe(pools: &Pools) {
    const RUNS: usize = 200;
    let pages = (ROWS as usize).div_ceil(STORE_PAGE);
    let mut rng = fastrand::Rng::with_seed(25);
    let mut model: Vec<Quote> = (0..ROWS).map(|id| pools.row(&mut rng, id)).collect();
    let epoch = Instant::now();
    let mut ingest = Ingest::<Quote>::new(HighlightPolicy::default(), epoch);
    ingest.apply(
        [Op::ReplaceAll(
            model.iter().cloned().map(Arc::new).collect(),
        )],
        epoch,
    );
    let mut held = Some(ingest.snapshot());
    let (mut copied, mut unique) = (Vec::with_capacity(RUNS), Vec::with_capacity(RUNS));
    for run in 0..RUNS * 2 {
        let hold = run % 2 == 0;
        let ops: Vec<Op<Quote>> = (0..pages)
            .map(|page| {
                let id = (page * STORE_PAGE + rng.usize(..STORE_PAGE)).min(ROWS as usize - 1);
                let quote = &mut model[id];
                mutate(&mut rng, quote);
                Op::Upsert(Arc::new(quote.clone()))
            })
            .collect();
        if !hold {
            held = None;
        }
        let now = epoch + Duration::from_millis(run as u64);
        let (_, elapsed) = time(|| {
            ingest.apply(ops, now);
            drop(held.replace(ingest.snapshot()));
        });
        if hold {
            copied.push(elapsed);
        } else {
            unique.push(elapsed);
        }
    }
    let (copied, unique) = (percentile(&copied, 0.5), percentile(&unique, 0.5));
    out!(
        "| S24 page copy-on-write: one update on each of the {pages} pages ({STORE_PAGE} slots), applied and published | previous snapshot held p50 {:.3} ms, dropped p50 {:.3} ms: {:.1} µs per page copied and freed ({:.1} ns per slot); load {} |",
        ms(copied),
        ms(unique),
        copied.saturating_sub(unique).as_secs_f64() * 1e6 / pages as f64,
        copied.saturating_sub(unique).as_secs_f64() * 1e9 / (pages * STORE_PAGE) as f64,
        load_average(),
    );
}

/// `Rebuild`, timing the aggregate jobs it is given, so S24 shows which
/// jobs a grouped view runs under its load.
#[derive(Clone, Default)]
struct JobTimes {
    aggregates: Arc<std::sync::Mutex<Vec<Duration>>>,
}

impl ProjectionEngine<Quote> for JobTimes {
    fn rebuild(&self, snapshot: &StoreSnapshot<Quote>, spec: &ViewSpec<Quote>) -> Projection {
        Rebuild::default().rebuild(snapshot, spec)
    }

    fn aggregate(
        &self,
        previous: &Projection,
        snapshot: &StoreSnapshot<Quote>,
        spec: &ViewSpec<Quote>,
    ) -> Projection {
        let (projection, elapsed) = time(|| Rebuild::default().aggregate(previous, snapshot, spec));
        if let Ok(mut aggregates) = self.aggregates.lock() {
            aggregates.push(elapsed);
        }
        projection
    }
}

/// What S24's frames of one view gave, for the knee.
struct RateFrames {
    rate: u32,
    frame_p99: Duration,
    rebuild_p99: Duration,
}

/// S24 (frame): the P2 load at `rate` updates/s, `rate / 60` writes on
/// random keys per frame, over `grid`. Every rebuild and every pull of a
/// snapshot (UI thread, outside the draw) is timed; `notes` adds the
/// view's own.
fn rate_frames(
    mut grid: GridHarness,
    rate: u32,
    name: &str,
    notes: impl FnOnce(&mut GridHarness) -> String,
) -> RateFrames {
    let writes = rate as usize / 60;
    let stats = |grid: &mut GridHarness| {
        grid.harness
            .cx
            .read_entity(&grid.grid, |grid, _| grid.stats())
    };
    let load = load_average();
    let (mut rebuilds, mut pulls) = (Vec::new(), Vec::new());
    let mut seen = stats(&mut grid);
    for _ in 0..FRAMES {
        let ids: Vec<u64> = (0..writes).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        grid.harness.tick();
        let now = stats(&mut grid);
        if now.rebuilds() != seen.rebuilds() {
            rebuilds.push(now.last_rebuild());
        }
        if now.snapshots() != seen.snapshots() {
            pulls.push(now.last_pull());
        }
        seen = now;
    }
    let samples = grid.harness.take();
    let rebuild_p99 = percentile(&rebuilds, 0.99);
    let notes = format!(
        "load {load}; {writes} writes per frame; Rebuild {} jobs, p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms (budget p99 30 ms); pull of a snapshot (UI thread) {} pulls, p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms; {}",
        rebuilds.len(),
        ms(percentile(&rebuilds, 0.5)),
        ms(rebuild_p99),
        ms(percentile(&rebuilds, 1.0)),
        pulls.len(),
        ms(percentile(&pulls, 0.5)),
        ms(percentile(&pulls, 0.99)),
        ms(percentile(&pulls, 1.0)),
        notes(&mut grid),
    );
    grid.harness.report(name, FRAMES, &samples, &notes);
    RateFrames {
        rate,
        frame_p99: percentile(&samples, 0.99),
        rebuild_p99,
    }
}

/// S24 (frames): S2 (P2 sorted by a changing column) and S16 (P2 grouped
/// by one column, Sum and Avg) at `rate` updates/s; the grouped grid times
/// the aggregate jobs it runs. Then S17's aggregate job on real threads,
/// after the updates of one throttle interval (100 ms) at `rate`.
fn scenario_rate_frames(pools: &Pools, rate: u32) -> (RateFrames, RateFrames) {
    let k = rate / 1_000;
    let (grid, _) = selection_harness(
        pools,
        ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]),
        false,
    );
    let flat = rate_frames(
        grid,
        rate,
        &format!("S24 S2 at {k}k/s: P2, random keys, sort by a changing column"),
        GridHarness::notes,
    );

    let engine = JobTimes::default();
    let s16 = ViewSpec::new()
        .with_sort([(Col(CHANGING_PRICE), SortDirection::Descending)])
        .with_group_by([GroupBy::column(Col(VENUE))])
        .with_aggregates(group_aggregates());
    let grid = GridHarness::configured(pools, {
        let (engine, view) = (engine.clone(), s16.clone());
        move |grid| grid.with_view(view).with_engine(engine)
    });
    if let Ok(mut jobs) = engine.aggregates.lock() {
        jobs.clear();
    }
    let grouped = rate_frames(
        grid,
        rate,
        &format!("S24 S16 at {k}k/s: P2 grouped by 1 column, Sum and Avg, all expanded"),
        |grid| {
            let groups = grid
                .harness
                .cx
                .read_entity(&grid.grid, |grid, _| grid.projection().groups().len());
            let jobs = engine
                .aggregates
                .lock()
                .map(|jobs| jobs.clone())
                .unwrap_or_default();
            format!(
                "aggregate jobs {} (p99 {:.1} ms): every pull changes the sort column, so each job is a Rebuild, which recounts the aggregates; {groups} groups, {}",
                jobs.len(),
                ms(percentile(&jobs, 0.99)),
                group_notes(grid),
            )
        },
    );

    // S17's job under this load: one throttle interval of the stream, with
    // the same updates (Volume and Last change among the others).
    const RUNS: usize = 100;
    let load = load_average();
    let mut rng = fastrand::Rng::with_seed(17);
    let mut model: Vec<Quote> = (0..ROWS).map(|id| pools.row(&mut rng, id)).collect();
    let mut ingest = Ingest::<Quote>::new(HighlightPolicy::default(), Instant::now());
    ingest.apply(
        [Op::ReplaceAll(
            model.iter().cloned().map(Arc::new).collect(),
        )],
        Instant::now(),
    );
    let engine = Rebuild::default();
    let projection = engine.rebuild(&ingest.snapshot(), &s16);
    let updates = rate as usize / 10;
    let ops: Vec<Op<Quote>> = (0..updates)
        .map(|_| {
            let quote = &mut model[rng.usize(..ROWS as usize)];
            mutate(&mut rng, quote);
            Op::Upsert(Arc::new(quote.clone()))
        })
        .collect();
    ingest.apply(ops, Instant::now());
    let changed = ingest.snapshot();
    let aggregate: Vec<Duration> = (0..RUNS)
        .map(|_| time(|| engine.aggregate(&projection, &changed, &s16)).1)
        .collect();
    let rebuild: Vec<Duration> = (0..RUNS)
        .map(|_| time(|| engine.rebuild(&changed, &s16)).1)
        .collect();
    out!("\n| Measurement | Result |\n|---|---|");
    out!(
        "| S24 aggregate job on the S16 projection after 100 ms at {k}k/s ({updates} updates, {RUNS} runs) | p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms; Rebuild of S16 on the same snapshot p50 {:.1} ms, p99 {:.1} ms; load {load} |",
        ms(percentile(&aggregate, 0.5)),
        ms(percentile(&aggregate, 0.99)),
        ms(percentile(&aggregate, 1.0)),
        ms(percentile(&rebuild, 0.5)),
        ms(percentile(&rebuild, 0.99)),
    );
    (flat, grouped)
}

/// The first of `values` (rate, value) over `budget`, else the largest.
fn first_over(values: impl IntoIterator<Item = (u32, f64)>, budget: f64, unit: &str) -> String {
    let values: Vec<(u32, f64)> = values.into_iter().collect();
    match values.iter().find(|(_, value)| *value > budget) {
        Some((rate, value)) => format!("**{}k/s** ({value:.1}{unit})", rate / 1_000),
        None => {
            let (rate, value) = values
                .iter()
                .copied()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or_default();
            format!(
                "not reached up to {}k/s (largest {value:.1}{unit} at {}k/s)",
                values.last().map_or(0, |(rate, _)| rate / 1_000),
                rate / 1_000
            )
        }
    }
}

/// S24, the knee: for each budget, the first rate of the sweep over it.
/// The ingest's budgets for both cadences: a batch every 1/60 s and the
/// store's own loop.
fn report_knee(
    (every_frame, store_loop): (&[IngestRun], &[IngestRun]),
    flat: &[RateFrames],
    grouped: &[RateFrames],
) {
    out!("| Budget | First rate over it |\n|---|---|");
    let ingest = |runs: &[IngestRun], value: fn(&IngestRun) -> f64| -> Vec<(u32, f64)> {
        runs.iter().map(|run| (run.rate, value(run))).collect()
    };
    let frames = |frames: &[RateFrames], value: fn(&RateFrames) -> Duration| -> Vec<(u32, f64)> {
        frames
            .iter()
            .map(|frames| (frames.rate, ms(value(frames))))
            .collect()
    };
    let publish_interval = StoreOptions::default().publish_interval().as_millis();
    for (cadence, runs) in [
        ("a batch every 1/60 s".to_owned(), every_frame),
        (
            format!("store loop, {publish_interval} ms pause"),
            store_loop,
        ),
    ] {
        out!(
            "| S24 knee: ingest above {S24_INGEST_BUDGET:.0}% of one core, {cadence} | {} |",
            first_over(ingest(runs, IngestRun::cpu_share), S24_INGEST_BUDGET, "%")
        );
        out!(
            "| S24 knee: an ingest batch above {} ms, {cadence} | {} |",
            S24_BATCH_BUDGET.as_millis(),
            first_over(
                ingest(runs, IngestRun::batch_max),
                ms(S24_BATCH_BUDGET),
                " ms"
            )
        );
        out!(
            "| S24 knee: change stamps above {S24_STAMP_BUDGET:.0} MB, {cadence} | {} |",
            first_over(ingest(runs, IngestRun::stamp_mb), S24_STAMP_BUDGET, " MB")
        );
    }
    for (label, frames_of, budget) in [
        ("flat", flat, S24_FLAT_FRAME_BUDGET),
        ("grouped", grouped, S24_GROUPED_FRAME_BUDGET),
    ] {
        out!(
            "| S24 knee: frame p99 above {} ms, {label} | {} |",
            budget.as_millis(),
            first_over(
                frames(frames_of, |frames| frames.frame_p99),
                ms(budget),
                " ms"
            )
        );
        out!(
            "| S24 knee: Rebuild p99 above {} ms, {label} | {} |",
            S24_REBUILD_BUDGET.as_millis(),
            first_over(
                frames(frames_of, |frames| frames.rebuild_p99),
                ms(S24_REBUILD_BUDGET),
                " ms"
            )
        );
    }
}

/// S24: the update rate sweep. The ingest at each rate on a real thread,
/// the frames of S2 and S16 at 100k/s, then the knee: the first rate that
/// breaks a budget.
fn scenario_rate_sweep(pools: &Pools) {
    let started = Instant::now();
    out!(
        "\n## S24: update rate sweep, P2 data set ({ROWS} rows, random keys), load at start {}\n",
        load_average()
    );
    out!(
        "### Ingest, {S24_SECONDS} s of simulated time per rate, HighlightPolicy default (5 s): a batch every 1/60 s, then as the store's ingest task takes them\n"
    );
    let (every_frame, store_loop) = scenario_ingest_sweep(pools);
    out!("\n### Frames at {}k/s\n", S24_FRAME_RATE / 1_000);
    header();
    let (flat, grouped) = scenario_rate_frames(pools, S24_FRAME_RATE);
    out!("\n### Knee\n");
    report_knee((&every_frame, &store_loop), &[flat], &[grouped]);
    out!(
        "\nload at end {}; S24 took {:.0} s",
        load_average(),
        started.elapsed().as_secs_f64()
    );
}

// ---------------------------------------------------------------------------
// S35–S37: exports of P2.

/// What `export_runs` measured, one sample per run.
#[derive(Default)]
struct ExportRuns {
    /// The export call on the UI thread: taking the snapshot, the order and
    /// the columns, then starting the job.
    action: Vec<Duration>,
    /// The job: building the table and writing it.
    job: Vec<Duration>,
    /// The rest of `run_until_parked` on the UI thread.
    finish: Vec<Duration>,
    rows: usize,
    columns: usize,
    bytes: u64,
}

impl ExportRuns {
    fn ui_thread(&self) -> Vec<Duration> {
        (0..self.action.len())
            .map(|ix| self.action[ix] + self.finish[ix])
            .collect()
    }

    fn report(&self, name: &str, budget: Duration) {
        let max = |samples: &[Duration]| ms(percentile(samples, 1.0));
        // The table the job builds: one `ExportRow` per row, beside the
        // snapshot it borrows from.
        let table = self.rows * size_of::<gpui_kit_grid::ExportRow<'static, Quote>>();
        out!(
            "| {name}, {} rows × {} columns, {:.1} MB written, {} runs | background max {:.1} ms (p50 {:.1} ms, budget {} ms), UI thread max {:.2} ms (export call max {:.2} ms), table {:.1} MB |",
            self.rows,
            self.columns,
            self.bytes as f64 / 1e6,
            self.action.len(),
            max(&self.job),
            ms(percentile(&self.job, 0.5)),
            budget.as_millis(),
            max(&self.ui_thread()),
            max(&self.action),
            table as f64 / 1e6,
        );
    }
}

/// Starts an export five times with `start` and waits for each. The test
/// dispatcher runs the job on this thread, timed by `TimedRunner`; the rest
/// of `run_until_parked` is the UI-thread finish.
fn export_runs<T>(
    grid: &mut GridHarness,
    runner: &TimedRunner,
    start: &dyn Fn(
        &mut LiveGridState<Blotter>,
        &mut Context<LiveGridState<Blotter>>,
    ) -> Result<ExportJob<T>, ExportError>,
) -> (ExportRuns, Vec<ExportOutcome<T>>) {
    const RUNS: usize = 5;
    let mut runs = ExportRuns::default();
    let mut outcomes = Vec::new();
    for _ in 0..RUNS {
        grid.harness.tick();
        runner.take();
        let (job, action) = time(|| {
            grid.harness
                .cx
                .update(|cx| grid.grid.update(cx, |grid, cx| start(grid, cx)))
                .expect("something to export")
        });
        let ((), parked) = time(|| grid.harness.cx.run_until_parked());
        let job_time: Duration = runner.take().iter().sum();
        assert!(job.is_finished(), "the job ran");
        let outcome = smol::block_on(job).expect("exported");
        runs.action.push(action);
        runs.job.push(job_time);
        runs.finish.push(parked.saturating_sub(job_time));
        runs.rows = outcome.rows();
        runs.bytes = outcome.bytes().unwrap_or_default();
        outcomes.push(outcome);
    }
    runs.columns = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.visible_columns().count());
    grid.harness.take();
    (runs, outcomes)
}

/// S35: CSV of P2 (100k rows, 39 visible columns, sorted by a changing
/// column), flat, `u64` keys, to a `Vec<u8>` and to a file in the
/// temporary directory. As in S9, "background" is the job's time on one
/// thread.
fn scenario_export_csv(pools: &Pools) {
    let runner = TimedRunner::default();
    let mut grid = GridHarness::configured(pools, {
        let runner = runner.clone();
        move |grid| {
            grid.with_view(ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]))
                .with_task_runner(runner)
        }
    });
    let (runs, _) = export_runs(&mut grid, &runner, &|grid, cx| {
        grid.export(
            ExportRequest::new(ExportScope::Visible),
            CsvFormat::new(),
            Vec::new(),
            cx,
        )
    });
    runs.report("S35 CSV to a Vec<u8>, u64 keys", S35_BACKGROUND_BUDGET);

    let dir = std::env::temp_dir().join(format!("spike-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let path = dir.join("p2.csv");
    let (runs, _) = export_runs(&mut grid, &runner, &|grid, cx| {
        grid.export_to_path(
            ExportRequest::new(ExportScope::Visible),
            CsvFormat::new(),
            &path,
            cx,
        )
    });
    runs.report("S35 CSV to a file, u64 keys", S35_BACKGROUND_BUDGET);
    let _ = std::fs::remove_dir_all(&dir);
}

const S35_BACKGROUND_BUDGET: Duration = Duration::from_millis(600);
const S36_BACKGROUND_BUDGET: Duration = Duration::from_millis(400);

/// S36: `export_with` over P2 grouped by venue (8 groups, collapsed, so the
/// export expands them), Sum(Volume) and Avg(Last), the totals row, 1% of
/// the rows with a pending price: a closure that reads every cell's value
/// and number hint and every row's depth and hidden mark, as a spreadsheet
/// format would, without writing a file. The grid's cost, not a library's.
fn scenario_export_with(pools: &Pools) {
    let runner = TimedRunner::default();
    let mut grid = GridHarness::configured(pools, {
        let runner = runner.clone();
        move |grid| {
            grid.with_view(
                ViewSpec::new()
                    .with_sort([(Col(1), SortDirection::Descending)])
                    .with_group_by([GroupBy::column(Col(VENUE))])
                    .with_aggregates(group_aggregates()),
            )
            .with_expand_depth(ExpandDepth::Levels(0))
            .with_totals(PinnedEdge::Bottom)
            .with_task_runner(runner)
        }
    });
    let pending: Vec<u64> = (0..ROWS).step_by(100).collect();
    grid.update_grid(|grid, cx| {
        grid.set_test_pending_values(
            pending
                .iter()
                .map(|&id| (id, Col(CHANGING_PRICE), OwnedValue::Float(-1.))),
            cx,
        )
    });
    let groups = grid
        .harness
        .cx
        .read_entity(&grid.grid, |grid, _| grid.projection().groups().len());
    let (runs, outcomes) = export_runs(&mut grid, &runner, &|grid, cx| {
        grid.export_with(
            ExportRequest::new(ExportScope::Expanded),
            |table, _, control| {
                let (mut numbers, mut digits, mut hidden, mut pending) =
                    (0usize, 0u64, 0usize, 0usize);
                for (ix, row) in table.rows().iter().enumerate() {
                    hidden += usize::from(row.is_hidden());
                    digits += u64::from(row.depth());
                    for column in table.columns() {
                        let Some(cell) = row.cell(column.column()) else {
                            continue;
                        };
                        numbers += usize::from(cell.value().is_number());
                        pending += usize::from(cell.is_pending());
                        if let Some(hint) = cell.number_hint() {
                            digits += u64::from(hint.fraction_digits());
                        }
                    }
                    if ix % 1024 == 1023 {
                        control.advance(1024)?;
                    }
                }
                control.advance(table.rows().len() % 1024)?;
                Ok((numbers, digits, hidden, pending))
            },
            cx,
        )
    });
    let (numbers, _, hidden, pending) = *outcomes[0].output();
    runs.report(
        &format!(
            "S36 export_with, every cell read, {groups} collapsed groups expanded, totals, {pending} pending cells, {hidden} hidden rows, {numbers} numbers, u64 keys"
        ),
        S36_BACKGROUND_BUDGET,
    );
}

/// Runs jobs on GPUI's background executor, as `BackgroundRunner` does, but
/// the next one after [`Self::next_on_thread`] on a thread of its own: an
/// export that runs while the frames are drawn, as on a machine with cores
/// to spare.
#[derive(Clone, Default)]
struct ExportThreadRunner {
    next_on_thread: Arc<std::sync::atomic::AtomicBool>,
}

impl ExportThreadRunner {
    fn next_on_thread(&self) {
        self.next_on_thread.store(true, Ordering::SeqCst);
    }
}

impl gpui_kit_grid::TaskRunner for ExportThreadRunner {
    fn run(&self, job: Box<dyn FnOnce() + Send>, cx: &App) -> gpui_kit::Task<()> {
        if self.next_on_thread.swap(false, Ordering::SeqCst) {
            std::thread::spawn(job);
            gpui_kit::Task::ready(())
        } else {
            cx.background_spawn(async move { job() })
        }
    }
}

/// S37: the S2 load (P2, ~10k updates/s on random keys, sorted by a
/// changing column) alone, then with the S35 CSV export running again and
/// again on a thread of its own while the frames are drawn. Busy is the
/// time of every frame interval's work on the test dispatcher (ingest,
/// pulls, rebuilds and draws) over the intervals' time.
fn scenario_export_under_load(pools: &Pools) {
    let runner = ExportThreadRunner::default();
    let mut grid = GridHarness::configured(pools, {
        let runner = runner.clone();
        move |grid| {
            grid.with_view(ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]))
                .with_task_runner(runner)
        }
    });
    let frames = |grid: &mut GridHarness, export: bool| {
        let mut job: Option<ExportJob<Vec<u8>>> = None;
        let (mut exports, mut busy) = (0, Duration::ZERO);
        for _ in 0..FRAMES {
            if export && job.as_ref().is_none_or(ExportJob::is_finished) {
                if job.take().is_some() {
                    exports += 1;
                }
                runner.next_on_thread();
                job = Some(
                    grid.harness
                        .cx
                        .update(|cx| {
                            grid.grid.update(cx, |grid, cx| {
                                grid.export(
                                    ExportRequest::new(ExportScope::Visible),
                                    CsvFormat::new(),
                                    Vec::new(),
                                    cx,
                                )
                            })
                        })
                        .expect("something to export"),
                );
            }
            let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
            grid.write(ids);
            let ((), elapsed) = time(|| grid.harness.tick());
            busy += elapsed;
        }
        if let Some(job) = job {
            job.cancel();
            let _ = smol::block_on(job);
        }
        let share = busy.as_secs_f64() * 100. / (FRAMES as f64 * FRAME.as_secs_f64());
        (grid.harness.take(), exports, share)
    };
    let (alone, _, alone_busy) = frames(&mut grid, false);
    let notes = format!("{}, busy {alone_busy:.0}%", grid.notes());
    grid.harness
        .report("S37 baseline: S2 in the same run", FRAMES, &alone, &notes);
    let (exporting, exports, busy) = frames(&mut grid, true);
    let ratio = ms(percentile(&exporting, 0.99)) / ms(percentile(&alone, 0.99)).max(f64::EPSILON);
    let notes = format!(
        "{}, {exports} CSV exports of 100k × 39 finished meanwhile, busy {busy:.0}%, p99 {:.0}% of the baseline",
        grid.notes(),
        ratio * 100.,
    );
    grid.harness.report(
        "S37 S2 with a CSV export running on another thread",
        FRAMES,
        &exporting,
        &notes,
    );
}

// ---------------------------------------------------------------------------
// S41: `LiveGridHandle` on 128 columns.

/// The columns of the 128-column grid (S41; S40 shows its column chooser).
const WIDE_COLUMNS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct WideCol(u16);

static ALL_WIDE_COLUMNS: [WideCol; WIDE_COLUMNS] = {
    let mut all = [WideCol(0); WIDE_COLUMNS];
    let mut ix = 0;
    while ix < WIDE_COLUMNS {
        all[ix] = WideCol(ix as u16);
        ix += 1;
    }
    all
};

static WIDE_COLUMN_IDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    (0..WIDE_COLUMNS)
        .map(|ix| &*Box::leak(format!("w{ix}").into_boxed_str()))
        .collect()
});

impl GridColumns for WideCol {
    fn all() -> &'static [Self] {
        &ALL_WIDE_COLUMNS
    }

    fn id(self) -> &'static str {
        WIDE_COLUMN_IDS[self.0 as usize]
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self.0)
    }
}

/// A row of numbers: every cell is the id times the column's place.
struct WideRow {
    id: u64,
}

impl GridRow for WideRow {
    type Key = u64;
    type Column = WideCol;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: WideCol) -> CellValue<'_> {
        CellValue::Int(self.id as i64 * i64::from(column.0))
    }
}

struct WideBlotter;

impl LiveGridDelegate for WideBlotter {
    type Row = WideRow;
}

/// 128 columns: two frozen at the left, three bands of eight columns (one
/// nested in another), every tenth column hidden, two frozen at the right.
fn wide_schema() -> GridSchema<WideCol> {
    let column = |ix: u16| {
        GridColumn::new(WideCol(ix), format!("Column {ix}"))
            .with_width(px(80.))
            .with_visible(ix % 10 != 9)
    };
    let band = |id: &'static str, title: &str, columns: std::ops::Range<u16>| {
        columns.fold(ColumnBand::new(id, title.to_owned()), |band, ix| {
            band.column(column(ix))
        })
    };
    let mut schema = GridSchema::new().column(column(0)).column(column(1));
    schema = schema
        .band(band("market", "Market", 2..10))
        .band(band("risk", "Risk", 10..14).band(band("greeks", "Greeks", 14..18)));
    schema = (18..126).fold(schema, |schema, ix| schema.column(column(ix)));
    (126..128)
        .fold(schema, |schema, ix| {
            schema.column(column(ix).with_zone(ColumnZone::FrozenRight))
        })
        .with_frozen(2)
}

/// S41: `LiveGridHandle` over a grid of 128 columns and 10k rows, two of
/// them sorted, 1000 rows selected: `columns()` (every column with its
/// title, bands and sort) and the counts a status bar reads on every
/// notify.
fn scenario_handle() {
    const COLUMN_RUNS: u32 = 2_000;
    const COUNT_RUNS: u32 = 1_000_000;
    let mut cx = headless();
    let grid = cx.update(|cx| {
        cx.new(|cx| {
            LiveGridState::new_standalone(wide_schema(), WideBlotter, StoreOptions::default(), cx)
                .with_view(ViewSpec::new().with_sort([
                    (WideCol(3), SortDirection::Descending),
                    (WideCol(40), SortDirection::Ascending),
                ]))
                .with_selection_mode(SelectionMode::RowSet)
        })
    });
    let writer = cx.read_entity(&grid, |grid, _| grid.writer());
    writer.replace_all((0..10_000).map(|id| Arc::new(WideRow { id })));
    for _ in 0..10 {
        cx.advance_clock(FRAME);
        cx.run_until_parked();
    }
    cx.update(|cx| grid.update(cx, |grid, cx| grid.set_selected_keys(0..1_000, cx)));
    let handle = LiveGridHandle::new(&grid);
    let per_call = |elapsed: Duration, runs: u32| elapsed.as_secs_f64() * 1e9 / f64::from(runs);
    let (columns, row_count, selected, held) = cx.update(|cx| {
        let (columns, elapsed) = time(|| {
            let mut columns = Vec::new();
            for _ in 0..COLUMN_RUNS {
                columns = std::hint::black_box(handle.columns(cx));
            }
            columns
        });
        assert_eq!(columns.len(), WIDE_COLUMNS);
        let columns = per_call(elapsed, COLUMN_RUNS);
        let count = |read: &dyn Fn() -> usize| {
            let ((), elapsed) = time(|| {
                for _ in 0..COUNT_RUNS {
                    std::hint::black_box(read());
                }
            });
            per_call(elapsed, COUNT_RUNS)
        };
        (
            columns,
            count(&|| handle.row_count(cx)),
            count(&|| handle.selected_count(cx)),
            count(&|| usize::from(handle.is_order_held(cx))),
        )
    });
    out!(
        "| S41 LiveGridHandle, {WIDE_COLUMNS} columns (3 bands, 2 sorted), 10000 rows, 1000 selected | columns() {:.1} µs (budget 50 µs); row_count {row_count:.0} ns, selected_count {selected:.0} ns, is_order_held {held:.0} ns (budget 1 µs) |",
        columns / 1e3,
    );
}

// ---------------------------------------------------------------------------
// S40: the column chooser on 128 columns under P2.

/// A row of [`wide_schema`] whose values change: the id times the column's
/// place, plus a tick each update bumps.
#[derive(Clone)]
struct TickingRow {
    id: u64,
    tick: i64,
}

impl GridRow for TickingRow {
    type Key = u64;
    type Column = WideCol;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: WideCol) -> CellValue<'_> {
        CellValue::Int(self.id as i64 * i64::from(column.0) + self.tick)
    }
}

struct TickingBlotter;

impl LiveGridDelegate for TickingBlotter {
    type Row = TickingRow;
}

struct ChooserRoot {
    grid: Entity<LiveGridState<TickingBlotter>>,
}

impl Render for ChooserRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(LiveGrid::new(&self.grid).xsmall().bordered(false))
    }
}

/// S40: [`wide_schema`] (128 columns, three bands, frozen zones) over 100k
/// rows sorted by a changing column, ~10k updates/s (P2). Opening the
/// chooser: the Action and the frame that draws it, the panel built for 128
/// columns. Typing into its search: each character and its frame. Hiding a
/// column from its row under the stream: the frames, a click every 20.
/// Every frame is the whole window drawn (`render_frame`).
fn scenario_column_chooser() {
    use gpui_kit_grid::actions::ShowColumnChooser;

    const STREAM_FRAMES: usize = 300;
    let mut cx = headless();
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(window_options(), cx, |_, cx| {
                cx.new(|cx| ChooserRoot {
                    grid: cx.new(|cx| {
                        LiveGridState::new_standalone(
                            wide_schema(),
                            TickingBlotter,
                            StoreOptions::default(),
                            cx,
                        )
                        .with_view(
                            ViewSpec::new().with_sort([(WideCol(3), SortDirection::Descending)]),
                        )
                    }),
                })
            })
        })
        .expect("open headless window");
    let grid = cx.read_entity(&root, |root, _| root.grid.clone());
    let writer = cx.read_entity(&grid, |grid, _| grid.writer());
    writer.replace_all((0..ROWS).map(|id| Arc::new(TickingRow { id, tick: 0 })));
    let tick = |cx: &mut HeadlessAppContext| {
        cx.advance_clock(FRAME);
        cx.run_until_parked();
    };
    for _ in 0..30 {
        tick(&mut cx);
    }
    let draw = |cx: &mut HeadlessAppContext| {
        time(|| {
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .expect("window is open")
        })
        .1
    };

    // Open: the Action, the deferred build of the panel, its first frame.
    let ((), open) = time(|| {
        cx.update_window(window, |_, window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.dispatch_action(Box::new(ShowColumnChooser), cx);
        })
        .expect("window is open");
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    });
    assert!(cx.read_entity(&grid, |grid, _| grid.is_column_chooser_open()));

    // Typing "Column 12" into the search field, which has the focus: each
    // keystroke (the input, the panel filtering its rows), then its frame.
    let (mut keystrokes, mut typing) = (Vec::new(), Vec::new());
    for character in "Column 12".chars() {
        let text = character.to_string();
        let mut key = gpui_kit::Keystroke::parse(&text).expect("a valid keystroke");
        key.key_char = Some(text);
        let ((), handled) = time(|| {
            cx.update_window(window, |_, window, cx| {
                window.dispatch_keystroke(key, cx);
            })
            .expect("window is open");
            cx.run_until_parked();
        });
        keystrokes.push(handled);
        typing.push(draw(&mut cx));
    }
    // Back to the whole list.
    for _ in 0.."Column 12".len() {
        cx.update_window(window, |_, window, cx| window.press("backspace", cx))
            .expect("window is open");
    }
    tick(&mut cx);

    // Under the stream: a column's row clicked every 20 frames.
    let mut rng = fastrand::Rng::with_seed(40);
    let mut ticks = vec![0i64; ROWS as usize];
    let mut frames = Vec::with_capacity(STREAM_FRAMES);
    let mut after_click = Vec::new();
    let mut toggles = 0;
    for frame_ix in 0..STREAM_FRAMES {
        for _ in 0..166 {
            let id = rng.u64(..ROWS);
            ticks[id as usize] += 1;
            writer.upsert(TickingRow {
                id,
                tick: ticks[id as usize],
            });
        }
        let clicked = frame_ix % 20 == 10;
        if clicked {
            let row = ElementId::NamedChild(
                Arc::new(ElementId::Name("chooser-column".into())),
                WideCol(3 + (frame_ix / 20) as u16 % 4).id().into(),
            );
            cx.update_window(window, |_, window, cx| {
                window.within("live-grid-column-chooser").click(row, cx)
            })
            .expect("window is open");
            toggles += 1;
        }
        tick(&mut cx);
        let elapsed = draw(&mut cx);
        frames.push(elapsed);
        if clicked {
            after_click.push(elapsed);
        }
    }
    let layout_changes = toggles;
    out!(
        "| S40 column chooser, {WIDE_COLUMNS} columns (3 bands, frozen zones), P2 {ROWS} rows | open {:.2} ms (budget 16 ms); search character: frame max {:.2} ms (p50 {:.2} ms, budget 4 ms), keystroke max {:.2} ms; under ~10k updates/s, {layout_changes} columns toggled from their rows: frame p99 {:.2} ms, max {:.2} ms (budget p99 12 ms), frames with a toggle max {:.2} ms; whole window drawn |",
        ms(open),
        ms(percentile(&typing, 1.0)),
        ms(percentile(&typing, 0.5)),
        ms(percentile(&keystrokes, 1.0)),
        ms(percentile(&frames, 0.99)),
        ms(percentile(&frames, 1.0)),
        ms(percentile(&after_click, 1.0)),
    );
}

// ---------------------------------------------------------------------------
// S38 and S39: dragging rows.

/// The middle of the body: a painted data row under any header.
fn body_middle() -> Point<Pixels> {
    point(px(WINDOW_WIDTH / 3.), px(WINDOW_HEIGHT / 2.))
}

impl Harness {
    /// A press and a release of the left button at `position`.
    fn click_at(&mut self, position: Point<Pixels>) {
        self.press_at(position);
        self.dispatch(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
    }

    /// The pointer moves to `position` and the left button goes down.
    fn press_at(&mut self, position: Point<Pixels>) {
        self.dispatch(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        self.dispatch(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
    }
}

/// S38: P2 rows dragged over a grid that takes drops (`DropOnRows`): the
/// pointer goes down the rows for 40 frames, then holds near their bottom
/// edge for 20, where they auto-scroll, while 10k/s updates stream, sorted
/// by a changing column. The grid drags its own rows over itself: the
/// frame's cost is the target's (the hit test, the policy, the indicator),
/// which a second grid would pay the same. The time of each pointer move's
/// dispatch bounds the target's placing.
fn scenario_row_drag_over(pools: &Pools) {
    const DRAG_FRAMES: usize = 60;
    let mut grid = GridHarness::configured(pools, |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]))
            .with_row_drag_gesture(SelectedRowsDrag::new())
            .with_row_drop_policy(DropOnRows)
    });
    let drops = Rc::new(Cell::new(0usize));
    let _subscription = grid.harness.cx.update(|cx| {
        let drops = drops.clone();
        cx.subscribe(&grid.grid, move |_, event, _| {
            if let LiveGridEvent::Drop { .. } = event {
                drops.set(drops.get() + 1);
            }
        })
    });
    // The row under the pointer is selected, then pressed again and
    // dragged past the gesture's threshold.
    let from = body_middle();
    grid.harness.click_at(from);
    grid.harness.tick();
    grid.harness.press_at(from);
    grid.harness.drag_to(from + point(px(0.), px(12.)));
    grid.harness.tick();
    grid.harness.drag_to(from + point(px(4.), px(16.)));
    grid.harness.tick();
    let dragging = grid.harness.cx.update(|cx| cx.has_active_drag());
    grid.harness.take();
    let edge = px(WINDOW_HEIGHT - 6.);
    let mut moves = Vec::with_capacity(DRAG_FRAMES);
    let mut targets = Vec::with_capacity(DRAG_FRAMES);
    let mut position = from;
    for frame_ix in 0..DRAG_FRAMES {
        let ids: Vec<u64> = (0..166).map(|_| grid.rng.u64(..ROWS)).collect();
        grid.write(ids);
        let y = if frame_ix < 40 {
            from.y + (edge - from.y) * ((frame_ix + 1) as f32 / 40.)
        } else {
            edge
        };
        position = point(from.x + px(4.), y);
        let started = Instant::now();
        grid.harness.drag_to(position);
        moves.push(started.elapsed());
        // The target's own cost: the hit test and the drop policy of this
        // move (`GridStats::last_drop_target`), without the frame the
        // harness draws at the end of the update.
        targets.push(
            grid.harness
                .cx
                .read_entity(&grid.grid, |grid, _| grid.stats().last_drop_target()),
        );
        grid.harness.tick();
    }
    let samples = grid.harness.take();
    grid.harness.dispatch(MouseUpEvent {
        button: MouseButton::Left,
        position,
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    grid.harness.tick();
    let notes = format!(
        "{}, drag {}, the drop target per move p50 {:.4} ms p99 {:.4} ms (budget ≤ 0.1 ms), a move's dispatch with its frame p50 {:.3} ms p99 {:.3} ms, drops {}",
        grid.notes(),
        if dragging { "active" } else { "NOT active" },
        ms(percentile(&targets, 0.5)),
        ms(percentile(&targets, 0.99)),
        ms(percentile(&moves, 0.5)),
        ms(percentile(&moves, 0.99)),
        drops.get(),
    );
    grid.harness.report(
        "S38 P2 rows dragged over a drop target, auto-scroll at the edge",
        DRAG_FRAMES,
        &samples,
        &notes,
    );
}

/// S39: a row drag started with all 100k rows selected (`u64` keys,
/// Cmd/Ctrl+A): the UI thread's time from the move past the gesture's
/// threshold (the command scope of 100k rows and the delegate's payload) to
/// the armed drag. Five starts. Budget ≤ 16 ms (as S14).
fn scenario_row_drag_start(pools: &Pools) {
    const STARTS: usize = 5;
    let mut grid = GridHarness::configured(pools, |grid| {
        grid.with_view(ViewSpec::new())
            .with_selection_mode(SelectionMode::RowSet)
            .with_row_drag_gesture(SelectedRowsDrag::new())
    });
    let selected = grid.select_everything();
    let from = body_middle();
    let mut starts = Vec::with_capacity(STARTS);
    let mut keys = 0;
    for _ in 0..STARTS {
        grid.harness.press_at(from);
        grid.harness.drag_to(from + point(px(0.), px(4.)));
        let started = Instant::now();
        grid.harness.drag_to(from + point(px(0.), px(12.)));
        starts.push(started.elapsed());
        keys = grid.harness.cx.read_entity(&grid.grid, |grid, _| {
            grid.row_drag().map_or(0, |drag| drag.keys().len())
        });
        grid.harness.dispatch(MouseUpEvent {
            button: MouseButton::Left,
            position: from + point(px(0.), px(12.)),
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        grid.harness.tick();
    }
    out!(
        "| S39 row drag start, all {ROWS} rows selected ({selected}) | {keys} keys; p50 {:.2} ms, max {:.2} ms (budget 16 ms) |",
        ms(percentile(&starts, 0.5)),
        ms(percentile(&starts, 1.0)),
    );
}

// ---------------------------------------------------------------------------
// S42: keyed storm. A producer thread writes R events/s over N keys into a
// real store on GPUI's real background executor (GCD threads, no window),
// with a Zipf (s = 1.1) or a uniform choice of key. A view sorted by the
// changing counter and grouped by one column follows it: a thread pulls the
// published snapshots at most every 8 ms (the grid's pull interval) and
// scans the changes against the painted rows, as the grid's pull does, and
// a background thread rebuilds with `Rebuild` at most every 100 ms (the
// grid's `min_rebuild_interval`), one rebuild at a time. A probe row written
// every millisecond times write → publish. The anti-scenario writes a new
// key with every event and removes it after 10 s.

/// The storm's key counts and event rates.
const S42_KEYS: [u32; 3] = [1_000, 10_000, 100_000];
const S42_RATES: [u32; 4] = [10_000, 50_000, 100_000, 250_000];
/// Higher rates, to find where the storm breaks.
const S42_HIGH_RATES: [u32; 3] = [500_000, 1_000_000, 2_000_000];
/// The keep-every-write store runs right after the coalescing one on the
/// cells that merge at least this many events per queue entry.
const S42_COMPARE_ABOVE: f64 = 1.1;
/// The anti-scenario: new keys a second and their time to live.
const S42_NEW_KEYS: u32 = 100_000;
const S42_TTL: Duration = Duration::from_secs(10);
/// The Zipf exponent: a hot minority of keys takes most events.
const S42_ZIPF: f64 = 1.1;
/// Groups of the view.
const S42_GROUPS: usize = 16;
/// A keep-every-write store that queues more than this stops its cell.
const S42_QUEUE_GUARD: usize = 3_000_000;
/// The grid's pull interval and rebuild throttle.
const S42_PULL: Duration = Duration::from_millis(8);
const S42_REBUILD_THROTTLE: Duration = Duration::from_millis(100);
/// Rows painted in a frame of the grouped view (1440 px of 26 px rows).
const S42_PAINTED: usize = 55;
/// The budgets: the ingest's share of a core (S24), an ingest batch, a
/// grouped rebuild (S16), the UI's pull out of a 12 ms grouped frame whose
/// paint takes about 1.4 ms (S16), and write → publish while keeping up.
const S42_INGEST_BUDGET: f64 = 50.;
const S42_BATCH_BUDGET: Duration = Duration::from_millis(16);
const S42_REBUILD_BUDGET: Duration = Duration::from_millis(30);
const S42_PULL_BUDGET: Duration = Duration::from_millis(10);
const S42_LATENCY_BUDGET: Duration = Duration::from_millis(100);

/// Texts of the storm's rows, leaked once: `value` borrows them.
fn leaked(count: usize, name: impl Fn(usize) -> String) -> Vec<&'static str> {
    (0..count)
        .map(|ix| &*Box::leak(name(ix).into_boxed_str()))
        .collect()
}

static S42_GROUP_NAMES: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| leaked(S42_GROUPS, |ix| format!("service-{ix:02}")));
static S42_SUBJECTS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    leaked(512, |ix| {
        format!("dc{}-h{}-r{:02}-n{:02}", ix % 6, ix % 3, ix % 16, ix % 20)
    })
});
static S42_DCS: LazyLock<Vec<&'static str>> = LazyLock::new(|| leaked(6, |ix| format!("dc{ix}")));
const S42_SEVERITIES: [&str; 4] = ["■ CRIT", "▲ MAJOR", "◆ MINOR", "● WARN"];

/// An alert console's columns, as `ops` has them: the count, a value and
/// the last firing change with every event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum StormColumn {
    Key,
    Severity,
    State,
    Rule,
    Subject,
    Group,
    Dc,
    Count,
    Value,
    FirstSeen,
    LastSeen,
    Probe,
}

impl GridColumns for StormColumn {
    fn all() -> &'static [Self] {
        use StormColumn::*;
        &[
            Key, Severity, State, Rule, Subject, Group, Dc, Count, Value, FirstSeen, LastSeen,
            Probe,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Severity => "severity",
            Self::State => "state",
            Self::Rule => "rule",
            Self::Subject => "subject",
            Self::Group => "group",
            Self::Dc => "dc",
            Self::Count => "count",
            Self::Value => "value",
            Self::FirstSeen => "first_seen",
            Self::LastSeen => "last_seen",
            Self::Probe => "probe",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// One storm row: its key, which picks its texts, the count of its events
/// (the producer's), a value and the time of the last event; the probe row
/// carries its sequence.
struct StormRow {
    key: u32,
    count: i64,
    value: f64,
    last: i64,
    probe: u64,
}

impl GridRow for StormRow {
    type Key = u32;
    type Column = StormColumn;

    fn key(&self) -> &u32 {
        &self.key
    }

    fn value(&self, column: StormColumn) -> CellValue<'_> {
        let key = self.key as usize;
        match column {
            StormColumn::Key => CellValue::Int(self.key.into()),
            StormColumn::Severity => CellValue::Text(S42_SEVERITIES[key % 4]),
            StormColumn::State => CellValue::Text("FIRING"),
            StormColumn::Rule => CellValue::Text("inlet temperature high"),
            StormColumn::Subject => CellValue::Text(S42_SUBJECTS[key % S42_SUBJECTS.len()]),
            StormColumn::Group => CellValue::Text(S42_GROUP_NAMES[key % S42_GROUPS]),
            StormColumn::Dc => CellValue::Text(S42_DCS[key % S42_DCS.len()]),
            StormColumn::Count => CellValue::Int(self.count),
            StormColumn::Value => CellValue::Float(self.value),
            StormColumn::FirstSeen => CellValue::Int(i64::from(self.key) % 86_400),
            StormColumn::LastSeen => CellValue::Int(self.last),
            StormColumn::Probe => CellValue::Int(self.probe as i64),
        }
    }
}

/// The probe's key: outside every key range of the storm.
const S42_PROBE: u32 = u32::MAX;

/// How the storm picks a key.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyChoice {
    Zipf,
    Uniform,
    /// Every event a new key, removed after [`S42_TTL`].
    NewKeys,
}

impl KeyChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Zipf => "Zipf 1.1",
            Self::Uniform => "uniform",
            Self::NewKeys => "new keys",
        }
    }

    /// The name of a group of cells in `SPIKE_S42_CELLS`.
    fn cells(self, keys: u32) -> String {
        match self {
            Self::Zipf => format!("zipf:{keys}"),
            Self::Uniform => format!("uniform:{keys}"),
            Self::NewKeys => "new".to_owned(),
        }
    }
}

/// Picks keys `0..n`: by rank of a Zipf law over a shuffled order of the
/// keys (hot keys spread over the pages), or uniformly.
struct KeySampler {
    cdf: Vec<f64>,
    keys: Vec<u32>,
}

impl KeySampler {
    fn new(n: u32, choice: KeyChoice, rng: &mut fastrand::Rng) -> Self {
        let mut keys: Vec<u32> = (0..n).collect();
        rng.shuffle(&mut keys);
        let cdf = if choice == KeyChoice::Zipf {
            let mut total = 0.;
            let mut cdf: Vec<f64> = (1..=n)
                .map(|rank| {
                    total += 1. / f64::from(rank).powf(S42_ZIPF);
                    total
                })
                .collect();
            cdf.iter_mut().for_each(|value| *value /= total);
            cdf
        } else {
            Vec::new()
        };
        Self { cdf, keys }
    }

    fn sample(&self, rng: &mut fastrand::Rng) -> u32 {
        let rank = if self.cdf.is_empty() {
            rng.usize(..self.keys.len())
        } else {
            let u = rng.f64();
            self.cdf
                .partition_point(|value| *value < u)
                .min(self.keys.len() - 1)
        };
        self.keys[rank]
    }
}

/// One cell of the storm.
#[derive(Clone, Copy)]
struct StormCell {
    keys: u32,
    rate: u32,
    choice: KeyChoice,
    coalescing: WriteCoalescing,
    seconds: f64,
}

impl StormCell {
    fn policy(&self) -> &'static str {
        if self.coalescing == WriteCoalescing::ByKey {
            "by key"
        } else {
            "every write"
        }
    }
}

/// What one cell measured.
#[derive(Default)]
struct StormRun {
    load: String,
    elapsed: f64,
    /// Storm events written (upserts and removals), and probe writes.
    sent: u64,
    probes: u64,
    /// Writes applied, probes and the initial load left out.
    applied: u64,
    writes: u64,
    coalesced: u64,
    peak_pending: usize,
    pending_at_end: usize,
    latencies: Vec<Duration>,
    batches: Vec<Duration>,
    batch_count: u64,
    batch_max: Duration,
    rebuilds: Vec<Duration>,
    pulls: Vec<Duration>,
    rows: usize,
    rss_mb: f64,
    stopped: bool,
    by_key: bool,
}

impl StormRun {
    fn sent_rate(&self) -> f64 {
        self.sent as f64 / self.elapsed
    }

    fn applied_rate(&self) -> f64 {
        self.applied as f64 / self.elapsed
    }

    /// Storm events per queue entry: the probe's entries (about one per
    /// batch by key, one per probe otherwise) and the initial load left out.
    fn coalescing(&self) -> f64 {
        let probe_entries = if self.by_key {
            self.batch_count.min(self.probes)
        } else {
            self.probes
        };
        let entries = self
            .writes
            .saturating_sub(self.coalesced)
            .saturating_sub(probe_entries)
            .saturating_sub(1);
        self.sent as f64 / entries.max(1) as f64
    }

    /// The ingest's share of a core: the sampled batches scaled to all.
    fn ingest_share(&self) -> f64 {
        let sampled: Duration = self.batches.iter().sum();
        let scale = self.batch_count as f64 / self.batches.len().max(1) as f64;
        sampled.as_secs_f64() * scale * 100. / self.elapsed
    }

    /// Keeps up: applies what is sent, publishes in time.
    fn keeps_up(&self) -> bool {
        !self.stopped
            && self.applied as f64 >= 0.99 * self.sent as f64
            && percentile(&self.latencies, 0.99) <= S42_LATENCY_BUDGET
    }

    /// The budgets this cell breaks.
    fn broken(&self) -> Vec<&'static str> {
        let mut broken = Vec::new();
        if !self.keeps_up() {
            broken.push("falls behind");
        }
        if self.ingest_share() > S42_INGEST_BUDGET {
            broken.push("ingest > 50%");
        }
        if percentile(&self.batches, 0.99) > S42_BATCH_BUDGET {
            broken.push("batch p99 > 16 ms");
        }
        if percentile(&self.rebuilds, 0.99) > S42_REBUILD_BUDGET {
            broken.push("rebuild p99 > 30 ms");
        }
        if percentile(&self.pulls, 0.99) > S42_PULL_BUDGET {
            broken.push("pull p99 > 10 ms");
        }
        broken
    }

    fn report(&self, cell: &StormCell) {
        let rate = match cell.choice {
            KeyChoice::NewKeys => format!(
                "{}k new keys/s, TTL {} s",
                cell.rate / 1_000,
                S42_TTL.as_secs()
            ),
            _ => format!("{}k/s", cell.rate / 1_000),
        };
        let broken = self.broken();
        let keys = match cell.choice {
            KeyChoice::NewKeys => "—".to_owned(),
            _ => cell.keys.to_string(),
        };
        out!(
            "| {} | {keys} | {rate} | {} | {} | {:.0}k | {:.0}k | {} | {:.2} | {:.2} / {:.2} | {:.2} / {:.2} ({:.1}) | {:.1}% | {:.1} / {:.1} ({}) | {:.1} | {:.3} / {:.3} | {} | {:.0} | {} |",
            cell.choice.label(),
            cell.policy(),
            self.load,
            self.sent_rate() / 1e3,
            self.applied_rate() / 1e3,
            self.peak_pending,
            self.coalescing(),
            ms(percentile(&self.latencies, 0.5)),
            ms(percentile(&self.latencies, 0.99)),
            ms(percentile(&self.batches, 0.5)),
            ms(percentile(&self.batches, 0.99)),
            ms(self.batch_max),
            self.ingest_share(),
            ms(percentile(&self.rebuilds, 0.5)),
            ms(percentile(&self.rebuilds, 0.99)),
            self.rebuilds.len(),
            self.rebuilds.len() as f64 / self.elapsed,
            ms(percentile(&self.pulls, 0.5)),
            ms(percentile(&self.pulls, 0.99)),
            self.rows,
            self.rss_mb,
            if broken.is_empty() {
                "ok".to_owned()
            } else {
                broken.join(", ")
            } + &if self.stopped {
                format!(" (stopped: {} queued)", self.pending_at_end)
            } else {
                String::new()
            },
        );
    }
}

/// The process's resident memory, MB.
fn rss_mb() -> f64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse::<f64>().ok())
        .map_or(0., |kb| kb / 1024.)
}

/// Waits for the next publish, at most 50 ms: false once `stop` is set
/// or the store is gone.
fn next_publish(
    subscription: &gpui_kit_grid::StoreSubscription<StormRow>,
    stop: &AtomicBool,
) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let published = smol::block_on(smol::future::or(
            async { Some(subscription.changed().await) },
            async {
                smol::Timer::after(Duration::from_millis(50)).await;
                None
            },
        ));
        match published {
            Some(open) => return open && !stop.load(Ordering::Relaxed),
            None => continue,
        }
    }
}

/// One cell: a store, its producer, the view's threads and the probe.
fn storm_cell(executor: &gpui_kit::BackgroundExecutor, cell: StormCell) -> StormRun {
    use std::sync::Mutex;

    let options = StoreOptions::default().with_write_coalescing(cell.coalescing);
    let store = gpui_kit_grid::GridStore::<StormRow>::builder(options).build_on(executor.clone());
    let writer = store.writer();
    let mut rng = fastrand::Rng::with_seed(42);
    let mut counts = vec![0i64; cell.keys as usize];
    if cell.choice != KeyChoice::NewKeys {
        writer.replace_all((0..cell.keys).map(|key| {
            Arc::new(StormRow {
                key,
                count: 0,
                value: 0.,
                last: 0,
                probe: 0,
            })
        }));
    }
    let _ = smol::block_on(writer.flush());
    let spec = ViewSpec::new()
        .with_sort([(StormColumn::Count, SortDirection::Descending)])
        .with_group_by([GroupBy::column(StormColumn::Group)])
        .with_aggregates([(StormColumn::Count, Aggregate::Sum)]);
    let mut view_columns = ColumnSet::EMPTY;
    view_columns.insert(StormColumn::Count.ix());
    view_columns.insert(StormColumn::Group.ix());

    let stop = Arc::new(AtomicBool::new(false));
    let probes: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let load = load_average();

    // Write → publish: wakes with every publish, reads the probe and the
    // last batch's time.
    let watcher = std::thread::spawn({
        let (subscription, store, stop, probes) = (
            store.subscribe(),
            store.clone(),
            stop.clone(),
            probes.clone(),
        );
        move || {
            let (mut latencies, mut batches) = (Vec::new(), Vec::new());
            let (mut seen_probe, mut seen_batches) = (0u64, 0u64);
            while next_publish(&subscription, &stop) {
                let now = Instant::now();
                let (snapshot, _) = subscription.take();
                let stats = store.stats();
                if stats.batches() > seen_batches {
                    seen_batches = stats.batches();
                    batches.push(stats.last_batch());
                }
                let probe = snapshot.get(&S42_PROBE).map_or(0, |row| row.probe);
                if probe > seen_probe
                    && let Ok(sent) = probes.lock()
                {
                    for seq in seen_probe + 1..=probe {
                        if let Some(at) = sent.get(seq as usize - 1) {
                            latencies.push(now.saturating_duration_since(*at));
                        }
                    }
                    seen_probe = probe;
                }
            }
            (latencies, batches)
        }
    });

    // The view: pulls at most every 8 ms and scans the changes against the
    // painted rows; a rebuild at most every 100 ms, one at a time.
    let painted: Arc<Mutex<Vec<gpui_kit_grid::RowId>>> = Arc::default();
    let rebuilding = Arc::new(AtomicBool::new(false));
    let (rebuild_tx, rebuild_rx) = std::sync::mpsc::channel::<Arc<StoreSnapshot<StormRow>>>();
    let rebuilder = std::thread::spawn({
        let (painted, rebuilding, spec) = (painted.clone(), rebuilding.clone(), spec.clone());
        move || {
            let mut rebuilds = Vec::new();
            for snapshot in rebuild_rx {
                let (projection, elapsed) = time(|| Rebuild::default().rebuild(&snapshot, &spec));
                rebuilds.push(elapsed);
                if let Ok(mut painted) = painted.lock() {
                    *painted = projection
                        .rows()
                        .iter()
                        .take(S42_PAINTED)
                        .copied()
                        .collect();
                }
                rebuilding.store(false, Ordering::Release);
            }
            rebuilds
        }
    });
    let view = std::thread::spawn({
        let (subscription, stop, painted, rebuilding) = (
            store.subscribe(),
            stop.clone(),
            painted.clone(),
            rebuilding.clone(),
        );
        move || {
            let mut pulls = Vec::new();
            let (mut last_pull, mut last_rebuild) = (Instant::now(), None::<Instant>);
            let mut dirty = false;
            while next_publish(&subscription, &stop) {
                let wait = S42_PULL.saturating_sub(last_pull.elapsed());
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                last_pull = Instant::now();
                let ((snapshot, visible), elapsed) = time(|| {
                    let (snapshot, changes) = subscription.take();
                    dirty |= changes.has_structure_change()
                        || changes.columns().intersects(view_columns);
                    let painted = painted.lock().map(|rows| rows.clone()).unwrap_or_default();
                    let visible = changes
                        .updated()
                        .iter()
                        .chain(changes.removed())
                        .any(|id| painted.contains(id));
                    (snapshot, visible)
                });
                pulls.push(elapsed);
                std::hint::black_box(visible);
                let throttled = last_rebuild.is_some_and(|at| at.elapsed() < S42_REBUILD_THROTTLE);
                if dirty && !throttled && !rebuilding.load(Ordering::Acquire) {
                    rebuilding.store(true, Ordering::Release);
                    last_rebuild = Some(Instant::now());
                    dirty = false;
                    let _ = rebuild_tx.send(snapshot);
                }
            }
            pulls
        }
    });

    // The producer, paced on the wall clock, a probe every millisecond.
    let producer = std::thread::spawn({
        let (writer, probes, stop) = (writer.clone(), probes.clone(), stop.clone());
        let store = store.clone();
        move || {
            let sampler = KeySampler::new(cell.keys, cell.choice, &mut rng);
            let duration = Duration::from_secs_f64(cell.seconds);
            let started = Instant::now();
            let (mut sent, mut events, mut probe, mut probes_sent) = (0u64, 0u64, 0u64, 0u64);
            let mut next_key = 0u32;
            let mut live: std::collections::VecDeque<(u32, Instant)> = Default::default();
            let mut stopped = false;
            loop {
                let elapsed = started.elapsed();
                if elapsed >= duration {
                    break;
                }
                let due = (f64::from(cell.rate) * elapsed.as_secs_f64()) as u64;
                let chunk = events + 2_000;
                while events < due.min(chunk) {
                    let (key, count) = match cell.choice {
                        KeyChoice::NewKeys => {
                            next_key += 1;
                            live.push_back((next_key, Instant::now()));
                            (next_key, 1)
                        }
                        _ => {
                            let key = sampler.sample(&mut rng);
                            counts[key as usize] += 1;
                            (key, counts[key as usize])
                        }
                    };
                    writer.upsert(StormRow {
                        key,
                        count,
                        value: rng.f64(),
                        last: elapsed.as_millis() as i64,
                        probe: 0,
                    });
                    events += 1;
                    sent += 1;
                }
                // The anti-scenario's keys leave after their time to live.
                while live.front().is_some_and(|(_, at)| at.elapsed() >= S42_TTL) {
                    if let Some((key, _)) = live.pop_front() {
                        writer.remove(key);
                        sent += 1;
                    }
                }
                if u128::from(probe) < elapsed.as_millis() {
                    probe += 1;
                    if let Ok(mut sent) = probes.lock() {
                        sent.push(Instant::now());
                    }
                    writer.upsert(StormRow {
                        key: S42_PROBE,
                        count: 0,
                        value: 0.,
                        last: 0,
                        probe,
                    });
                    probes_sent += 1;
                }
                if events % 16_384 < 2_000 && store.stats().pending() > S42_QUEUE_GUARD {
                    stopped = true;
                    break;
                }
                if events >= due {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            let elapsed = started.elapsed().as_secs_f64();
            stop.store(stopped, Ordering::Relaxed);
            (sent, probes_sent, elapsed, stopped)
        }
    });

    let mut rss = rss_mb();
    while !producer.is_finished() {
        std::thread::sleep(Duration::from_millis(500));
        rss = rss.max(rss_mb());
    }
    let (sent, probes, elapsed, stopped) = producer.join().expect("producer");
    let stats = store.stats();
    let rows = store.snapshot().len();
    stop.store(true, Ordering::Relaxed);
    let (latencies, batches) = watcher.join().expect("watcher");
    let pulls = view.join().expect("view");
    let rebuilds = rebuilder.join().expect("rebuilder");
    let run = StormRun {
        load,
        elapsed,
        sent,
        probes,
        applied: stats.applied_writes().saturating_sub(probes),
        writes: stats.writes(),
        coalesced: stats.coalesced_writes(),
        peak_pending: stats.peak_pending(),
        pending_at_end: stats.pending(),
        latencies,
        batches,
        batch_count: stats.batches(),
        batch_max: stats.max_batch(),
        rebuilds,
        pulls,
        rows,
        rss_mb: rss,
        stopped,
        by_key: cell.coalescing == WriteCoalescing::ByKey,
    };
    drop(writer);
    drop(store);
    run
}

/// S42: the keyed storm sweep, the keep-every-write comparison where the
/// policies differ, the anti-scenario and the knee.
fn scenario_keyed_storm() {
    let started = Instant::now();
    let seconds = std::env::var("SPIKE_S42_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10.);
    // `SPIKE_S42_CELLS=uniform:100000,zipf:100000,new` runs only these
    // groups of cells, to measure them again on a quieter machine.
    let only: Option<Vec<String>> = std::env::var("SPIKE_S42_CELLS").ok().map(|value| {
        value
            .split(',')
            .map(|item| item.trim().to_ascii_lowercase())
            .collect()
    });
    let wanted = |choice: KeyChoice, keys: u32| {
        only.as_ref()
            .is_none_or(|only| only.contains(&choice.cells(keys)))
    };
    let platform = gpui_kit::platform::current_platform(true);
    let executor = platform.background_executor();
    out!(
        "\n## S42: keyed storm, {seconds} s per cell, load at start {}\n",
        load_average()
    );
    out!(
        "Real threads: the store on GPUI's background executor, one producer thread (one `upsert` per event, a probe every ms), a view thread pulling at most every 8 ms and scanning the changes against {S42_PAINTED} painted rows, a `Rebuild` thread (sorted by the changing count, grouped by one column of {S42_GROUPS} groups, `Sum` of the count) at most every 100 ms. Rows of 12 columns, an alert console's; an event changes three. Sent/s: storm events (upserts, and the anti-scenario's removals), the probe left out. Coalescing: storm events per queue entry, the probe's entries left out. Ingest: the sampled batches scaled to all, over the cell's time. Batch: CPU time of an ingest batch that published (sampled at each publish); max in parentheses. RSS is the process's, which the allocator does not give back between cells.\n"
    );
    const HEADER: &str = "| Keys | N | Rate | Policy | Load (1, 5, 15 min) | Sent/s | Applied/s | Peak pending | Coalescing | Write → publish p50 / p99, ms | Batch p50 / p99, ms (max) | Ingest, % of a core | Rebuild p50 / p99, ms (count) | Rebuilds/s | Pull p50 / p99, ms | Rows | RSS max, MB | Budgets |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|";
    out!("{HEADER}");
    let mut runs: Vec<(StormCell, StormRun)> = Vec::new();
    let mut compared: Vec<(StormCell, StormRun)> = Vec::new();
    for choice in [KeyChoice::Zipf, KeyChoice::Uniform] {
        for keys in S42_KEYS.into_iter().filter(|keys| wanted(choice, *keys)) {
            for rate in S42_RATES.into_iter().chain(S42_HIGH_RATES) {
                let cell = StormCell {
                    keys,
                    rate,
                    choice,
                    coalescing: WriteCoalescing::ByKey,
                    seconds,
                };
                let run = storm_cell(&executor, cell);
                run.report(&cell);
                // Right after, on the same machine state: every write kept.
                if run.coalescing() >= S42_COMPARE_ABOVE {
                    let every = StormCell {
                        coalescing: WriteCoalescing::KeepEveryWrite,
                        ..cell
                    };
                    let every_run = storm_cell(&executor, every);
                    every_run.report(&every);
                    compared.push((every, every_run));
                }
                runs.push((cell, run));
            }
        }
    }
    out!(
        "\n### Keep every write against by key, where they differ (coalescing at least {S42_COMPARE_ABOVE})\n"
    );
    out!(
        "| Keys | N | Rate | Peak pending, by key / every write | Applied/s | Ingest, % of a core | Batch p99, ms | Write → publish p99, ms |\n|---|---|---|---|---|---|---|---|"
    );
    for (every, every_run) in &compared {
        let Some((_, run)) = runs.iter().find(|(cell, _)| {
            (cell.choice, cell.keys, cell.rate) == (every.choice, every.keys, every.rate)
        }) else {
            continue;
        };
        out!(
            "| {} | {} | {}k/s | {} / {} | {:.0}k / {:.0}k | {:.1} / {:.1} | {:.2} / {:.2} | {:.1} / {:.1} |",
            every.choice.label(),
            every.keys,
            every.rate / 1_000,
            run.peak_pending,
            every_run.peak_pending,
            run.applied_rate() / 1e3,
            every_run.applied_rate() / 1e3,
            run.ingest_share(),
            every_run.ingest_share(),
            ms(percentile(&run.batches, 0.99)),
            ms(percentile(&every_run.batches, 0.99)),
            ms(percentile(&run.latencies, 0.99)),
            ms(percentile(&every_run.latencies, 0.99)),
        );
    }
    out!("\n### Anti-scenario: every event a new key\n");
    out!("{HEADER}");
    let anti = StormCell {
        keys: 0,
        rate: S42_NEW_KEYS,
        choice: KeyChoice::NewKeys,
        coalescing: WriteCoalescing::ByKey,
        seconds: seconds * 2.,
    };
    if wanted(KeyChoice::NewKeys, 0) {
        storm_cell(&executor, anti).report(&anti);
    }

    out!("\n### Knee: the first rate that breaks a budget\n");
    out!("| Keys | N | First rate over a budget | Budgets broken there |\n|---|---|---|---|");
    for choice in [KeyChoice::Zipf, KeyChoice::Uniform] {
        for keys in S42_KEYS.into_iter().filter(|keys| wanted(choice, *keys)) {
            let first = runs
                .iter()
                .filter(|(cell, _)| cell.choice == choice && cell.keys == keys)
                .find(|(_, run)| !run.broken().is_empty());
            let (rate, broken) = first.map_or_else(
                || ("none up to 2000k/s".to_owned(), String::new()),
                |(cell, run)| (format!("{}k/s", cell.rate / 1_000), run.broken().join(", ")),
            );
            out!("| {} | {keys} | {rate} | {broken} |", choice.label());
        }
    }
    out!(
        "\nload at end {}; S42 took {:.0} s",
        load_average(),
        started.elapsed().as_secs_f64()
    );
}

fn main() {
    let pools = Pools::new();
    if std::env::var_os("SPIKE_S24_ONLY").is_some() {
        scenario_rate_sweep(&pools);
        return;
    }
    if std::env::var_os("SPIKE_CELLS_ONLY").is_some() {
        header();
        scenario_visible_updates(&pools, false);
        scenario_random_p2(&pools, false);
        scenario_fling(&pools, false);
        scenario_typed_cells(&pools);
        scenario_cell_elements(&pools, false);
        scenario_cell_elements(&pools, true);
        scenario_cell_images(&pools);
        return;
    }
    if std::env::var_os("SPIKE_S42_ONLY").is_some() {
        scenario_keyed_storm();
        return;
    }
    if std::env::var_os("SPIKE_DRAG_ONLY").is_some() {
        header();
        scenario_random_p2(&pools, false);
        scenario_row_drag_over(&pools);
        scenario_row_drag_start(&pools);
        return;
    }
    if std::env::var_os("SPIKE_PAINT_GROUP_ONLY").is_some() {
        header();
        scenario_visible_updates(&pools, false);
        let s1_p50 = scenario_visible_updates(&pools, false);
        scenario_directional(&pools, s1_p50);
        scenario_random_p2(&pools, false);
        scenario_cell_elements(&pools, false);
        scenario_grouped_p2(&pools, false);
        scenario_grouped_two_levels(&pools);
        scenario_fading_grouped(&pools);
        out!("\n## Background threads\n");
        out!("| Measurement | Result |\n|---|---|");
        group_benchmarks(&pools);
        return;
    }
    if std::env::var_os("SPIKE_EDIT_ONLY").is_some() {
        header();
        scenario_random_p2(&pools, false);
        scenario_editing_p2(&pools);
        scenario_optimistic_values(&pools);
        return;
    }
    out!(
        "## Main thread: draws of the measured subtree (ms), {WINDOW_WIDTH}×{WINDOW_HEIGHT} at 2×, {ROWS} rows × {COLUMNS} columns, {FRAMES} frames of {} ms\n",
        FRAME.as_millis()
    );
    header();
    let s1_p50 = scenario_visible_updates(&pools, false);
    // The S1 load with direction arrows.
    scenario_directional(&pools, s1_p50);
    scenario_hover_tooltip(&pools);
    let s10_p50 = scenario_a11y(&pools, s1_p50);
    scenario_random_p2(&pools, false);
    scenario_quick_filter_typing(&pools);
    // A cell editor open over P2, the order held.
    scenario_editing_p2(&pools);
    // Optimistic values over P2.
    scenario_optimistic_values(&pools);
    // Dragging rows.
    scenario_row_drag_over(&pools);
    scenario_row_drag_start(&pools);
    scenario_churn(&pools, false);
    scenario_fling(&pools, false);
    scenario_horizontal(&pools);
    scenario_resize(&pools);
    scenario_resize_right_zone(&pools);
    scenario_header_drag(&pools);
    scenario_group_panel_drag(&pools);
    scenario_page_down(&pools);
    scenario_pinned_progress(&pools, true);
    scenario_pinned_progress(&pools, false);
    // The same loads with every row selected and the focus outline shown;
    // after the plain rows, so a chart reads those first.
    scenario_visible_updates(&pools, true);
    scenario_random_p2(&pools, true);
    scenario_churn(&pools, true);
    scenario_fling(&pools, true);
    scenario_datatable(&pools, false);
    scenario_datatable(&pools, true);
    scenario_fade(&pools);
    // Groups on screen.
    let s16_p50 = scenario_grouped_p2(&pools, false);
    let s16_a11y_p50 = scenario_grouped_p2(&pools, true);
    out!(
        "| S16 accessibility planned over not | | | {:+.2} | | | | | p50 of the draws |",
        ms(s16_a11y_p50) - ms(s16_p50)
    );
    scenario_grouped_two_levels(&pools);
    // Totals and group footers on screen.
    scenario_totals_footers(&pools);
    // Removed rows fading out in a grouped view.
    scenario_fading_grouped(&pools);
    scenario_expand_frames(&pools);
    scenario_a11y_grouped(&pools, s10_p50);
    // The tree on screen.
    scenario_tree_frames(&pools, true);
    scenario_tree_frames(&pools, false);
    // Icons and times; elements in cells.
    scenario_typed_cells(&pools);
    scenario_cell_elements(&pools, false);
    scenario_cell_elements(&pools, true);
    // Frames while an export runs.
    scenario_export_under_load(&pools);
    // Images in cells.
    scenario_cell_images(&pools);

    out!("\n## One-shot frames\n");
    out!("| Measurement | Result |\n|---|---|");
    scenario_autofit(&pools);
    scenario_select_all(&pools);
    scenario_copy_all(&pools);
    scenario_copy_expanded(&pools);
    // Exports.
    scenario_export_csv(&pools);
    scenario_export_with(&pools);
    // The grid without its delegate type.
    scenario_handle();
    // The column chooser.
    scenario_column_chooser();

    out!("\n## Background threads\n");
    out!("| Measurement | Result |\n|---|---|");
    background_benchmarks(&pools);
    scenario_quick_filter_rebuild(&pools);

    // S24: the update rate sweep, after every other scenario.
    scenario_rate_sweep(&pools);
    scenario_keyed_storm();
}
