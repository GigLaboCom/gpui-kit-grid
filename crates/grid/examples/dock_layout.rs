//! Grids in a dock, saved and restored: two tabs of one trade view over a
//! shared live store, and a notes panel. The dock, with every grid's layout
//! inside its panel, and the named layouts are written to a file half a
//! second after each change and when the window closes; the next start
//! restores them. A file that cannot be read is reported and the example
//! opens with the default dock.
//!
//! ```sh
//! cargo run --release -p gpui-kit-grid --example dock_layout
//! DOCK_LAYOUT=/tmp/dock.json cargo run --release -p gpui-kit-grid --example dock_layout
//! ```
//!
//! `DOCK_E2E_LOG=<file>` reports the trades grids to the Linux end-to-end
//! run (`script/e2e`); see the `e2e` module at the end.
//!
//! Each trades tab has a toolbar: "Save as" stores the tab's layout in the
//! layout library under the typed name, "Save as default" also makes it the
//! layout a trades panel opens with when it has none of its own, a saved
//! name applies that layout, and "Reset columns" restores the schema's.
//! Above each grid is its group panel: drag a header there to group the
//! trades by that column; captions sum the quantity and the fee and
//! average the price. The second tab starts grouped by instrument, so a
//! restart shows the grouping and the collapsed groups of a tab that was
//! not open: the layout saves them too.
//! Columns resize and move with the mouse; the column Actions act on the
//! header cell pressed last and are bound here, under the grid's key
//! context: Alt+H hides it, Alt+A shows all, Alt+L and Alt+R freeze it left
//! or right, Alt+U unfreezes it, Alt+0 resets the layout.

use std::{
    fs::File,
    io::Write as _,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{
        BasePanel, DockArea, DockAreaState, DockEvent, DockLayout, DockSkin, Panel, PanelEvent,
        PanelInfo, PanelState, panel_handle, register_panel,
    },
    input::{Input, InputState},
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Global,
    IntoElement, KeyBinding, ParentElement as _, Render, SharedString, Styled as _, Subscription,
    Task, WeakEntity, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_kit_grid::{
    Aggregate, CellValue, ColumnBand, ColumnIx, GridColumn, GridColumns, GridLayout, GridRow,
    GridSchema, GridStore, GroupBy, LayoutLibrary, LiveGrid, LiveGridDelegate, LiveGridEvent,
    LiveGridState, SortDirection, StoreOptions, ViewSpec,
    actions::{
        FreezeColumnLeft, FreezeColumnRight, HideColumn, ResetLayout, ShowAllColumns,
        UnfreezeColumn,
    },
};
use serde::{Deserialize, Serialize};

// A print to a closed stdout or stderr (a parent that exited, `| head`)
// panics, and a panic in a GPUI callback aborts the app: write and ignore
// the error instead of `println!` and `eprintln!`.
macro_rules! out {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}

macro_rules! err {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}

/// The view id of every trades grid, and the name of its dock panel.
const TRADES: &str = "trades";
const NOTES: &str = "notes";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TradeColumn {
    Id,
    Instrument,
    Side,
    Quantity,
    Price,
    Venue,
    Fee,
}

impl GridColumns for TradeColumn {
    fn all() -> &'static [Self] {
        use TradeColumn::*;
        &[Id, Instrument, Side, Quantity, Price, Venue, Fee]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Instrument => "instrument",
            Self::Side => "side",
            Self::Quantity => "quantity",
            Self::Price => "price",
            Self::Venue => "venue",
            Self::Fee => "fee",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Clone)]
struct Trade {
    id: u64,
    instrument: Arc<str>,
    sell: bool,
    quantity: i64,
    price: f64,
    venue: &'static str,
}

impl GridRow for Trade {
    type Key = u64;
    type Column = TradeColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: TradeColumn) -> CellValue<'_> {
        match column {
            TradeColumn::Id => CellValue::Int(self.id as i64),
            TradeColumn::Instrument => CellValue::Text(&self.instrument),
            TradeColumn::Side => CellValue::Text(if self.sell { "Sell" } else { "Buy" }),
            TradeColumn::Quantity => CellValue::Int(self.quantity),
            TradeColumn::Price => CellValue::Float(self.price),
            TradeColumn::Venue => CellValue::Text(self.venue),
            TradeColumn::Fee => CellValue::Float(self.price * self.quantity as f64 * 0.0003),
        }
    }
}

struct Trades;

impl LiveGridDelegate for Trades {
    type Row = Trade;
}

// #region banded-schema
fn schema() -> GridSchema<TradeColumn> {
    GridSchema::new()
        .with_frozen(1)
        .column(GridColumn::new(TradeColumn::Id, "Id").with_width(px(80.)))
        .column(GridColumn::new(TradeColumn::Instrument, "Instrument").with_width(px(120.)))
        .band(
            ColumnBand::new("execution", "Execution")
                .column(GridColumn::new(TradeColumn::Side, "Side").with_width(px(70.)))
                .column(GridColumn::new(TradeColumn::Quantity, "Quantity"))
                .column(GridColumn::new(TradeColumn::Price, "Price")),
        )
        .column(GridColumn::new(TradeColumn::Venue, "Venue").with_width(px(90.)))
        .column(
            GridColumn::new(TradeColumn::Fee, "Fee")
                .with_width(px(90.))
                .with_visible(false),
        )
}
// #endregion banded-schema

/// What the example writes: the dock (each grid's layout is inside its
/// panel) and the named layouts.
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    dock: Option<DockAreaState>,
    #[serde(default)]
    library: LayoutLibrary,
}

/// What every trades panel shares, also one the dock registry builds.
struct Shared {
    store: GridStore<Trade>,
    library: LayoutLibrary,
    workspace: WeakEntity<Workspace>,
}

impl Global for Shared {}

/// Asks the workspace to save soon (after the grid's `LayoutChange`,
/// `SortChange`, `GroupChange` and `ExpansionChange`, the save triggers of
/// a grid).
fn schedule_save(cx: &mut App) {
    if let Some(workspace) = cx.global::<Shared>().workspace.upgrade() {
        workspace.update(cx, |workspace, cx| workspace.schedule_save(cx));
    }
}

struct TradesPanel {
    grid: Entity<LiveGridState<Trades>>,
    name: Entity<InputState>,
    focus_handle: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl TradesPanel {
    /// A trades panel with its saved layout, else the library's default
    /// layout for the view, else the schema's grouped by `group_by`. A
    /// layout holds the grouping: one without it opens ungrouped.
    fn new(
        layout: Option<GridLayout>,
        group_by: &[TradeColumn],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let shared = cx.global::<Shared>();
        let layout = layout.or_else(|| {
            shared
                .library
                .default_layout(TRADES)
                .map(|(_, layout)| layout.clone())
        });
        let store = shared.store.clone();
        let group_by: Vec<GroupBy<Trade>> = group_by.iter().copied().map(GroupBy::column).collect();
        let grid = cx.new(|cx| {
            let grid = LiveGridState::new(schema(), Trades, store, cx)
                .with_view_id(TRADES)
                .with_view(
                    ViewSpec::new()
                        .with_sort([(TradeColumn::Price, SortDirection::Descending)])
                        .with_group_by(group_by)
                        .with_aggregates([
                            (TradeColumn::Quantity, Aggregate::Sum),
                            (TradeColumn::Price, Aggregate::Avg),
                            (TradeColumn::Fee, Aggregate::Sum),
                        ]),
                );
            match layout {
                Some(layout) => grid.with_layout(layout),
                None => grid,
            }
        });
        let save_on_change = cx.subscribe(
            &grid,
            |_, _, event: &LiveGridEvent<u64, TradeColumn>, cx| {
                if matches!(
                    event,
                    LiveGridEvent::LayoutChange { .. }
                        | LiveGridEvent::SortChange { .. }
                        | LiveGridEvent::GroupChange { .. }
                        | LiveGridEvent::ExpansionChange { .. }
                ) {
                    schedule_save(cx);
                }
            },
        );
        // The list of saved layouts lives in the global.
        let refresh = cx.observe_global::<Shared>(|_, cx| cx.notify());
        e2e::register(&grid, cx);
        Self {
            grid,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Layout name")),
            focus_handle: cx.focus_handle(),
            _subscriptions: [save_on_change, refresh],
        }
    }

    fn save_as(&mut self, default: bool, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let layout = self.grid.read(cx).layout();
        let library = &mut cx.global_mut::<Shared>().library;
        if let Err(error) = library.insert(name.clone(), layout) {
            err!("layout \"{name}\" not saved: {error}");
            return;
        }
        if default {
            library.set_default(TRADES, Some(&name));
        }
        schedule_save(cx);
    }

    fn apply(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(layout) = cx.global::<Shared>().library.get(TRADES, name).cloned() else {
            return;
        };
        self.grid
            .update(cx, |grid, cx| match grid.set_layout(&layout, cx) {
                Ok(report) if !report.is_exact() => {
                    err!("layout \"{name}\" adjusted to the schema: {report:?}");
                }
                Ok(_) => {}
                Err(error) => err!("layout \"{name}\" not applied: {error}"),
            });
    }
}

impl BasePanel for TradesPanel {
    fn panel_name(&self) -> &'static str {
        TRADES
    }

    /// The dock pulls the layout when it saves; the grid needs no render.
    fn dump(&self, cx: &App) -> PanelState {
        let mut state = PanelState::new(TRADES);
        let layout = self.grid.read(cx).layout();
        state.info = PanelInfo::panel(serde_json::json!({ "grid": layout }));
        state
    }
}

impl Panel for TradesPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Trades"
    }
}

impl EventEmitter<PanelEvent> for TradesPanel {}

impl Focusable for TradesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TradesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let library = &cx.global::<Shared>().library;
        let default = library.default_layout(TRADES).map(|(name, _)| name);
        let names: Vec<SharedString> = library
            .names(TRADES)
            .map(|name| {
                if Some(name) == default {
                    format!("{name} (default)").into()
                } else {
                    SharedString::from(name.to_owned())
                }
            })
            .collect();
        let saved: Vec<String> = library.names(TRADES).map(str::to_owned).collect();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .p_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Input::new(&self.name).small().w(px(160.)))
                    .child(
                        Button::new("save-as")
                            .small()
                            .label("Save as")
                            .on_click(cx.listener(|this, _, _, cx| this.save_as(false, cx))),
                    )
                    .child(
                        Button::new("save-as-default")
                            .small()
                            .label("Save as default")
                            .on_click(cx.listener(|this, _, _, cx| this.save_as(true, cx))),
                    )
                    .child(
                        Button::new("reset-columns")
                            .small()
                            .label("Reset columns")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.grid.update(cx, |grid, cx| grid.reset_layout(cx));
                            })),
                    )
                    .children(names.into_iter().zip(saved).enumerate().map(
                        |(ix, (label, name))| {
                            Button::new(("layout", ix))
                                .small()
                                .ghost()
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| this.apply(&name, cx)))
                        },
                    )),
            )
            .child(
                div().flex_1().min_h_0().child(
                    LiveGrid::new(&self.grid)
                        .small()
                        .bordered(false)
                        .group_panel(true),
                ),
            )
    }
}

/// Another kind of panel, so the dock holds more than grids.
struct NotesPanel {
    focus_handle: FocusHandle,
}

impl BasePanel for NotesPanel {
    fn panel_name(&self) -> &'static str {
        NOTES
    }
}

impl Panel for NotesPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Notes"
    }
}

impl EventEmitter<PanelEvent> for NotesPanel {}

impl Focusable for NotesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NotesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Rearrange the panels and change the columns, the grouping and the collapsed groups of both trades tabs, the hidden one too. Restart: everything is where you left it.")
            .child("Press a header cell to make its column current, then Alt+H hides it, Alt+A shows all, Alt+L or Alt+R freezes it, Alt+U unfreezes it, Alt+0 resets the layout.")
    }
}

/// The saved grid layout of a trades panel, if this build can read it. A bad
/// one opens the panel with a default layout instead of failing the load.
fn saved_layout(info: &PanelInfo) -> Option<GridLayout> {
    let PanelInfo::Panel(value) = info else {
        return None;
    };
    let grid = value.get("grid")?;
    serde_json::from_value::<GridLayout>(grid.clone())
        .inspect_err(|error| err!("a saved grid layout is not readable: {error}"))
        .ok()
}

struct Workspace {
    area: Entity<DockArea>,
    path: PathBuf,
    save: Option<Task<()>>,
    _subscription: Option<Subscription>,
}

impl Workspace {
    fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (area, _) = DockSkin::dock_area("dock-layout", None, window, cx);
        Self {
            area,
            path,
            save: None,
            _subscription: None,
        }
    }

    /// Restores a saved dock, or builds the default one.
    fn restore(
        &mut self,
        dock: Option<DockAreaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let restored = dock.map(|state| {
            self.area
                .update(cx, |area, cx| area.load(state, window, cx))
        });
        if let Some(Err(error)) = &restored {
            err!("the saved dock is not usable: {error}");
        }
        if !matches!(restored, Some(Ok(()))) {
            let first = cx.new(|cx| TradesPanel::new(None, &[], window, cx));
            // Grouped, and behind the first tab: its grouping is saved and
            // restored without the tab being shown.
            let second =
                cx.new(|cx| TradesPanel::new(None, &[TradeColumn::Instrument], window, cx));
            let notes = cx.new(|cx| NotesPanel {
                focus_handle: cx.focus_handle(),
            });
            let layout = DockLayout::h_split()
                .child(
                    DockLayout::tabs()
                        .panel_view(panel_handle(first), cx)
                        .panel_view(panel_handle(second), cx),
                    None,
                )
                .child(
                    DockLayout::tabs().panel_view(panel_handle(notes), cx),
                    Some(px(260.)),
                );
            self.area
                .update(cx, |area, cx| area.set_center(layout, window, cx));
        }
        // The dock saves on its own edits; the grids add theirs.
        self._subscription = Some(cx.subscribe(&self.area, |this, _, event: &DockEvent, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.schedule_save(cx);
            }
        }));
    }

    /// Saves in half a second; a newer request replaces this one.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| this.save_now(cx));
        }));
    }

    fn save_now(&mut self, cx: &mut Context<Self>) {
        self.save = None;
        let saved = Saved {
            dock: Some(self.area.read(cx).dump(cx)),
            library: cx.global::<Shared>().library.clone(),
        };
        let written = serde_json::to_string_pretty(&saved)
            .map_err(|error| error.to_string())
            .and_then(|text| std::fs::write(&self.path, text).map_err(|error| error.to_string()));
        if let Err(error) = written {
            err!("{} not saved: {error}", self.path.display());
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .child(self.area.clone())
    }
}

/// Reads the saved file; a missing one is a first start, a bad one is
/// reported.
fn load_saved(path: &PathBuf) -> Saved {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Saved::default();
    };
    serde_json::from_str(&text).unwrap_or_else(|error| {
        err!("{} is not usable, starting over: {error}", path.display());
        Saved::default()
    })
}

/// Writes trades from a producer thread: a snapshot, then price updates.
fn produce(store: &GridStore<Trade>) {
    let writer = store.writer();
    std::thread::spawn(move || {
        let instruments: Vec<Arc<str>> = ["SBER", "GAZP", "LKOH", "YNDX", "VTBR"]
            .map(Arc::from)
            .to_vec();
        let venues = ["MOEX", "SPB", "OTC"];
        let mut rng = fastrand::Rng::new();
        let mut trades: Vec<Trade> = (0..2_000)
            .map(|id| Trade {
                id,
                instrument: instruments[rng.usize(..instruments.len())].clone(),
                sell: rng.bool(),
                quantity: rng.i64(1..10_000),
                price: rng.f64() * 300.,
                venue: venues[rng.usize(..venues.len())],
            })
            .collect();
        writer.replace_all(trades.iter().cloned().map(Arc::new));
        while !writer.is_closed() {
            let trade = &mut trades[rng.usize(..2_000)];
            trade.price *= 1. + (rng.f64() - 0.5) / 100.;
            writer.upsert(trade.clone());
            std::thread::sleep(Duration::from_millis(5));
        }
    });
}

fn main() {
    let path = std::env::var_os("DOCK_LAYOUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gpui-kit-grid-dock-layout.json"));
    let saved = load_saved(&path);
    out!("dock layout file: {}", path.display());

    gpui_kit::application()
        // gpui-kit's icons (sort arrows, menu checks): without them icons are blank.
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            // The grid's key bindings: arrows, Page, Home/End, Enter, Escape.
            gpui_kit_grid::init(cx);
            cx.bind_keys([
                KeyBinding::new("alt-h", HideColumn, Some("LiveGrid")),
                KeyBinding::new("alt-a", ShowAllColumns, Some("LiveGrid")),
                KeyBinding::new("alt-l", FreezeColumnLeft, Some("LiveGrid")),
                KeyBinding::new("alt-r", FreezeColumnRight, Some("LiveGrid")),
                KeyBinding::new("alt-u", UnfreezeColumn, Some("LiveGrid")),
                KeyBinding::new("alt-0", ResetLayout, Some("LiveGrid")),
            ]);

            let store = GridStore::new(StoreOptions::default(), cx);
            produce(&store);
            cx.set_global(Shared {
                store,
                library: saved.library,
                workspace: WeakEntity::new_invalid(),
            });
            // The registry builds the panels of a saved dock.
            register_panel(cx, TRADES, |context, window, cx| {
                let layout = saved_layout(context.info());
                panel_handle(cx.new(|cx| TradesPanel::new(layout, &[], window, cx)))
            });
            register_panel(cx, NOTES, |_, _, cx| {
                panel_handle(cx.new(|cx| NotesPanel {
                    focus_handle: cx.focus_handle(),
                }))
            });

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                    None,
                    size(px(1400.), px(900.)),
                    cx,
                ))),
                ..Default::default()
            };
            let dock = saved.dock;
            gpui_kit::open_window(options, cx, move |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(path, window, cx));
                cx.global_mut::<Shared>().workspace = workspace.downgrade();
                workspace.update(cx, |workspace, cx| workspace.restore(dock, window, cx));
                e2e::install(&workspace, cx);
                // Save when the window closes, then quit.
                let closing = workspace.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    if let Some(workspace) = closing.upgrade() {
                        workspace.update(cx, |workspace, cx| workspace.save_now(cx));
                    }
                    cx.quit();
                    true
                });
                workspace
            })
            .expect("open window");
            cx.activate(true);
        });
}

/// `DOCK_E2E_LOG=<file>`: the trades grids as the Linux end-to-end run sees
/// them (`script/e2e`), one JSON object per line. A `state`
/// line lists every trades grid in the order the panels were built: its
/// grouping, its lines and groups, and the frames it painted (none for a
/// tab never shown); it is written when any of that changed, polled every
/// 50 ms. The file `<log>.quit` saves the dock, as closing the window
/// does, and quits. Without the variable nothing here runs.
mod e2e {
    use super::*;

    /// The trades grids, in the order their panels were built.
    #[derive(Default)]
    struct Grids(Vec<WeakEntity<LiveGridState<Trades>>>);

    impl Global for Grids {}

    fn path() -> Option<PathBuf> {
        std::env::var_os("DOCK_E2E_LOG").map(PathBuf::from)
    }

    /// Lists `grid` in the log.
    pub fn register(grid: &Entity<LiveGridState<Trades>>, cx: &mut App) {
        if path().is_some() {
            cx.default_global::<Grids>().0.push(grid.downgrade());
        }
    }

    /// Starts the log: the polled `state` lines and the quit file.
    pub fn install(workspace: &Entity<Workspace>, cx: &mut App) {
        let Some(path) = path() else {
            return;
        };
        let mut file = match File::create(&path) {
            Ok(file) => file,
            Err(error) => {
                err!("DOCK_E2E_LOG={}: not written: {error}", path.display());
                return;
            }
        };
        let quit = PathBuf::from(format!("{}.quit", path.display()));
        let workspace = workspace.downgrade();
        let started = Instant::now();
        cx.spawn(async move |cx| {
            let mut last = None;
            let mut seq = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let grids = cx.update(grids_json);
                if last.as_ref() != Some(&grids) {
                    seq += 1;
                    let line = serde_json::json!({
                        "type": "state",
                        "seq": seq,
                        "t": started.elapsed().as_millis() as u64,
                        "grids": grids,
                    });
                    if let Err(error) = writeln!(file, "{line}") {
                        err!("DOCK_E2E_LOG: not written: {error}");
                    }
                    last = Some(grids);
                }
                if quit.exists() {
                    cx.update(|cx| {
                        if let Some(workspace) = workspace.upgrade() {
                            workspace.update(cx, |workspace, cx| workspace.save_now(cx));
                        }
                        cx.quit();
                    });
                    break;
                }
            }
        })
        .detach();
    }

    fn grids_json(cx: &mut App) -> serde_json::Value {
        let grids = cx.default_global::<Grids>().0.clone();
        grids
            .iter()
            .filter_map(|grid| grid.upgrade())
            .map(|grid| {
                let grid = grid.read(cx);
                let projection = grid.projection();
                serde_json::json!({
                    "group_by": grid
                        .group_by()
                        .iter()
                        .map(|level| level.id().as_str().to_owned())
                        .collect::<Vec<_>>(),
                    "rows": projection.len(),
                    "groups": projection.groups().len(),
                    "frames": grid.stats().frames(),
                })
            })
            .collect()
    }
}
