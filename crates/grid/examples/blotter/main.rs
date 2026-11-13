//! A trade blotter: every feature of the grid of stages 1 and 2 in one
//! window over a live stream, for manual checks and an unattended
//! end-to-end run.
//!
//! ```sh
//! cargo run --release -p gpui-kit-grid --example blotter
//! ROWS=100000 RATE=10000 cargo run --release -p gpui-kit-grid --example blotter
//! BLOTTER_GROUP=instrument,side BLOTTER_TOTALS=1 cargo run --release -p gpui-kit-grid --example blotter
//! BLOTTER_FADE=800 CHURN=200 BLOTTER_GROUP=instrument cargo run --release -p gpui-kit-grid --example blotter
//! BLOTTER_GROUP=instrument BLOTTER_FOOTERS=1 cargo run --release -p gpui-kit-grid --example blotter
//! BLOTTER_LAYOUT=target/blotter-layout.json cargo run --release -p gpui-kit-grid --example blotter
//! BLOTTER_SECONDS=10 BLOTTER_REORDER=manual cargo run --release -p gpui-kit-grid --example blotter
//! ```
//!
//! A producer thread writes the trades: the first load, then quote updates
//! and trades replaced at the rates below. Index is pinned above the
//! trades, and the application's Total row below them, or with
//! `BLOTTER_TOTALS=1` the grid's totals row; `BLOTTER_FOOTERS=1` closes
//! every expanded group with a footer of its aggregates. The toolbar sets
//! the reorder policy (Live, Hold, Manual; Refresh order shows an order
//! held back), reloads the trades under the live stream (Reload or F5,
//! with progress; Cancel reload), shows the group panel (drag a header
//! there to group by its column), and its Groups menu expands or collapses
//! every group, shows the captions only and copies everything expanded; it
//! sets the row size, the selection mode, the change arrows and the theme;
//! the filter field (Cmd/Ctrl+F) is the grid's quick filter. Group captions
//! and the totals sum Volume, average Last, and show the lowest Bid and the
//! highest Ask. A cell's context menu opens the selected trades (the
//! primary command, like Enter and a double click) and cancels them; a
//! group caption's opens and cancels the trades of its group. A Trader
//! cell's tooltip names the trader and the desk; Cmd/Ctrl+C copies the
//! selection, or the group of a focused caption. The status bar shows the
//! grid's counters, the groups, a held order, the selection, the last
//! opened trades, the last copy and the last click (`LiveGridEvent::Click`:
//! its target and click count). The toolbar and the status bar reach the
//! grid through its `LiveGridHandle`, without the delegate type: the
//! reorder policy, Refresh order, the Groups menu, Columns… (the grid's
//! column chooser) and the filter; the rows shown of the rows loaded, the
//! selection count and a held order from the handle's events.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `ROWS` | 20000 | Trades at the first load |
//! | `RATE` | 2000 | Quote updates per second |
//! | `CHURN` | 20 | Trades replaced per second (one removed, one inserted) |
//! | `RELOAD_MS` | 1500 | How long a reload waits for the "server" |
//! | `BLOTTER_REORDER` | `live` | The reorder policy at start: `live`, `hold` or `manual` |
//! | `BLOTTER_COPY` | `raw` | `displayed`: copy the painted text instead of raw values |
//! | `BLOTTER_GROUP` | none | Grouping levels at start: column ids, comma separated (`instrument,side`); `-` after an id sorts its groups descending (`instrument-`). An unknown id is reported and skipped |
//! | `BLOTTER_GROUP_PANEL` | `1` | `0` hides the group panel at start |
//! | `BLOTTER_TOTALS` | `0` | `1`: the grid's totals row at the bottom instead of the application's Total row |
//! | `BLOTTER_FOOTERS` | `0` | `1`: a footer with the group's aggregates under every expanded group (`EveryGroupFooter`) |
//! | `BLOTTER_PAINT` | `directional` | `uniform`: one colour per change kind, without arrows |
//! | `BLOTTER_FADE` | `0` | Removed trades fade out for this many milliseconds |
//! | `BLOTTER_EXPAND` | `all` | Groups expanded at start: `all`, `none`, `captions` or a number of levels |
//! | `BLOTTER_SECONDS` | none | Print the status every half second and quit after N seconds |
//! | `BLOTTER_LAYOUT` | none | The layout file, below |
//! | `BLOTTER_E2E_LOG` | none | The end-to-end log of `script/e2e` (`e2e.rs`) |
//!
//! `BLOTTER_LAYOUT=<file>` keeps the grid layout (column widths, order,
//! frozen zones, hidden columns, the sort, the grouping and which groups
//! are collapsed) across restarts: the file is read at startup and written
//! half a second after each change, when the window closes and when the
//! blotter quits. A saved layout overrides `BLOTTER_GROUP` and
//! `BLOTTER_EXPAND`, as it does the sort. A file that cannot be read or
//! written is reported on stderr and the blotter keeps running.

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

mod model;
mod toolbar;

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Sizable as _, Size,
    input::{InputEvent, InputState},
    menu::PopupMenu,
    select::{SelectEvent, SelectState},
    status_bar::StatusBar,
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, KeyBinding, MouseButton, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px,
    size,
};
use gpui_kit_grid::{
    AggregateStyleContext, CellExport, CellStyle, CellStyleContext, CellTooltip,
    CellTooltipContext, ChangePaint, DirectionalChanges, DisplayedCells, EveryGroupFooter,
    ExpandDepth, ExportScope, GridHandleEvent, GridLayout, GridMenuContext, GridRow as _,
    GridTarget, GroupBy, GroupPath, HighlightPolicy, JsonLayoutCodec, LayoutCodec as _,
    LayoutError, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridHandle, LiveGridState,
    OwnedValue, PinnedEdge, SelectionMode, SortDirection, StoreOptions, UniformChanges, ViewSpec,
    actions,
};
use smol::channel::Sender;

use crate::{
    model::{Feed, Quote, QuoteColumn},
    toolbar::{Reorder, SELECTION_MODES, SIZES},
};

// A closed stdout or stderr, for example a parent that exited, must not abort the example.
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

// After the macros, which it uses.
mod e2e;

gpui_kit::actions!(
    blotter,
    [
        /// Reloads the trades under the live stream (F5).
        Reload,
        /// Cancels the trades of the command scope (the cell context menu).
        CancelTrades,
        /// Moves the focus to the filter field (Cmd/Ctrl+F).
        FocusFilter,
        /// Shows or hides the group panel (the toolbar's Group panel).
        ToggleGroupPanel,
        /// Copies every line with every group expanded, the rows of
        /// collapsed groups under their captions (the Groups menu).
        CopyExpanded,
    ]
);

/// The key context of the window content.
const KEY_CONTEXT: &str = "Blotter";

/// The grid's hooks. They take `&self` and never read the grid: commands
/// leave as Actions (`CancelTrades`) and events (`Confirm`).
struct Blotter;

impl LiveGridDelegate for Blotter {
    type Row = Quote;

    fn cell_style(&self, cell: &CellStyleContext<'_, Quote>, style: &mut CellStyle, cx: &App) {
        if cell.row().is_sell() {
            style.set_foreground(cx.theme().blue);
        }
        // BLOTTER_E2E_LOG: the change painted, read after the change paint.
        e2e::painted_cell(cell, style);
    }

    /// Only for the end-to-end log: the aggregates keep the grid's style.
    fn aggregate_style(
        &self,
        cell: &AggregateStyleContext<'_, Quote>,
        style: &mut CellStyle,
        _: &App,
    ) {
        e2e::painted_aggregate(cell, style);
    }

    /// "Open trade", or "Open 3 trades": the bold first item, run by Enter
    /// and a double click too (`LiveGridEvent::Confirm`).
    fn primary_command(&self, menu: &GridMenuContext<'_, Quote>, _: &App) -> Option<SharedString> {
        Some(trades_label("Open", menu.selection().len()).into())
    }

    /// On a cell or a group caption: Cancel trades (a caption's: its
    /// group's), then the standard items. The Action reads the command
    /// scope when it runs, not this menu's.
    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, Quote>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let menu = match menu_context.target() {
            GridTarget::Cell { .. } | GridTarget::GroupCaption { .. } => {
                let count = menu_context.selection().len();
                menu.menu_with_disabled(
                    trades_label("Cancel", count),
                    Box::new(CancelTrades),
                    count == 0,
                )
                .separator()
            }
            _ => menu,
        };
        menu_context.standard_items(menu, window, cx)
    }

    /// The trader's name and desk on a Trader cell; elsewhere the full text
    /// of a truncated cell.
    fn tooltip(&self, cell: &CellTooltipContext<'_, Quote>, _: &App) -> Option<CellTooltip> {
        match (cell.column(), cell.row().trader()) {
            (QuoteColumn::Trader, Some(trader)) => Some(CellTooltip::text(trader.description())),
            _ => cell.default_tooltip(),
        }
    }
}

/// "Open trade", "Open 3 trades", "Open trades" (none).
fn trades_label(verb: &str, count: usize) -> String {
    match count {
        1 => format!("{verb} trade"),
        0 => format!("{verb} trades"),
        count => format!("{verb} {count} trades"),
    }
}

/// "Click ×1: trade 42, Bid", "Click ×2: Last header", "Right click ×1:
/// Total row, Volume", "Click ×1: group INS042 › Buy": a click's button,
/// click count and target.
fn click_label(
    target: &GridTarget<u64, QuoteColumn>,
    button: MouseButton,
    click_count: usize,
) -> String {
    let place = match target {
        GridTarget::Cell { key, column } => format!("trade {key}, {column:?}"),
        // The blotter pins Index above the trades and Total below them.
        GridTarget::Pinned { edge, column, .. } => match edge {
            PinnedEdge::Top => format!("Index row, {column:?}"),
            PinnedEdge::Bottom => format!("Total row, {column:?}"),
        },
        GridTarget::Header { column } => format!("{column:?} header"),
        GridTarget::GroupCaption { group, .. } => format!("group {}", path_label(group)),
        GridTarget::GroupFooter { group, column } => {
            format!("footer of {}, {column:?}", path_label(group))
        }
        GridTarget::Totals { column, .. } => format!("totals, {column:?}"),
        GridTarget::Empty => "empty area".into(),
        _ => "grid".into(),
    };
    let click = match button {
        MouseButton::Left => "Click",
        MouseButton::Right => "Right click",
        MouseButton::Middle => "Middle click",
        _ => "Other click",
    };
    format!("{click} ×{click_count}: {place}")
}

/// "INS042 › Buy": the key values of a group's path, the top level first.
fn path_label(path: &GroupPath) -> String {
    let values: Vec<String> = path
        .levels()
        .iter()
        .map(|(_, value)| value_text(value))
        .collect();
    values.join(" › ")
}

/// A key value as text: the trades group by text and number columns.
fn value_text(value: &OwnedValue) -> String {
    match value {
        OwnedValue::Text(text) => text.to_string(),
        OwnedValue::Int(value) => value.to_string(),
        OwnedValue::Float(value) => value.to_string(),
        OwnedValue::Bool(value) => value.to_string(),
        _ => String::new(),
    }
}

/// "1 row", "20000 rows".
fn rows_label(count: usize) -> String {
    if count == 1 {
        "1 row".into()
    } else {
        format!("{count} rows")
    }
}

/// What the status bar shows of the grid, from its handle's events
/// (`GridHandleEvent`): the rows shown of the rows loaded, the selected
/// rows, a held order, and how many orders were shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GridStatus {
    /// Data rows of the order shown.
    shown: usize,
    /// Live rows of the snapshot shown.
    total: usize,
    selected: usize,
    order_held: bool,
    /// Orders shown (`ProjectionChange`), for the counters.
    orders_shown: u64,
}

impl GridStatus {
    /// Takes in an event of the grid, `total` the rows loaded now. Returns
    /// true if the status bar should show it at once; the counts of a new
    /// order wait for the next half-second refresh.
    fn apply(&mut self, event: &GridHandleEvent, total: usize) -> bool {
        match *event {
            GridHandleEvent::ProjectionChange { row_count, .. } => {
                self.orders_shown += 1;
                self.shown = row_count;
                self.total = total;
                false
            }
            GridHandleEvent::SelectionChange { count, .. } => {
                self.selected = count;
                true
            }
            GridHandleEvent::OrderHeldChange { held } => {
                self.order_held = held;
                true
            }
            _ => false,
        }
    }

    /// "120 of 20000 rows", "Order held", "3 rows selected".
    fn items(&self) -> Vec<String> {
        let shown = if self.shown == self.total {
            rows_label(self.total)
        } else {
            format!("{} of {}", self.shown, rows_label(self.total))
        };
        [
            Some(shown),
            self.order_held.then(|| "Order held".to_owned()),
            (self.selected > 0).then(|| format!("{} selected", rows_label(self.selected))),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// `BLOTTER_EXPAND`: `all`, `none`, `captions` or a number of levels.
fn expand_depth(value: &str) -> Option<ExpandDepth> {
    match value {
        "all" => Some(ExpandDepth::All),
        "none" => Some(ExpandDepth::Levels(0)),
        "captions" => Some(ExpandDepth::Captions),
        levels => levels.parse().ok().map(ExpandDepth::Levels),
    }
}

// #region grouping-view
/// The view at start: by Last descending, grouped by `group_by` with the
/// aggregates of `model::aggregates`; with `footers`, a footer under every
/// expanded group.
fn view(group_by: Vec<GroupBy<Quote>>, footers: bool) -> ViewSpec<Quote> {
    let view = ViewSpec::new()
        .with_sort([(QuoteColumn::Last, SortDirection::Descending)])
        .with_group_by(group_by)
        .with_aggregates(model::aggregates());
    if footers {
        view.with_group_footer(EveryGroupFooter)
    } else {
        view
    }
}
// #endregion grouping-view

/// The change colours: rise and fall with arrows, or stage 1's.
// #region change-paint
fn change_paint(arrows: bool) -> ChangePaint {
    if arrows {
        ChangePaint::Directional(DirectionalChanges)
    } else {
        ChangePaint::Uniform(UniformChanges)
    }
}
// #endregion change-paint

/// What the environment sets.
struct Settings {
    feed: Feed,
    reload_delay: Duration,
    reorder: Reorder,
    copy_displayed: bool,
    group_by: Vec<GroupBy<Quote>>,
    group_panel: bool,
    totals: bool,
    footers: bool,
    arrows: bool,
    removal_fade: Duration,
    expand: ExpandDepth,
    quit_after: Option<u64>,
    layout_path: Option<PathBuf>,
    saved_layout: Option<GridLayout>,
}

impl Settings {
    fn from_env() -> Self {
        let layout_path = std::env::var_os("BLOTTER_LAYOUT").map(PathBuf::from);
        let saved_layout = layout_path.as_deref().and_then(read_layout);
        let reorder = std::env::var("BLOTTER_REORDER")
            .ok()
            .map_or(Reorder::Live, |value| {
                Reorder::from_env(&value).unwrap_or_else(|| {
                    err!("BLOTTER_REORDER={value}: not live, hold or manual; live is used");
                    Reorder::Live
                })
            });
        let group_by = std::env::var("BLOTTER_GROUP").map_or_else(
            |_| Vec::new(),
            |value| {
                let (levels, unknown) = model::group_levels(&value);
                for part in unknown {
                    err!("BLOTTER_GROUP: {part:?} is no column id; the level is skipped");
                }
                levels
            },
        );
        let arrows = match std::env::var("BLOTTER_PAINT").as_deref() {
            Ok("directional") | Err(_) => true,
            Ok("uniform") => false,
            Ok(other) => {
                err!("BLOTTER_PAINT={other}: not directional or uniform; directional is used");
                true
            }
        };
        let expand = std::env::var("BLOTTER_EXPAND")
            .ok()
            .map_or(ExpandDepth::All, |value| {
                expand_depth(&value).unwrap_or_else(|| {
                    err!(
                        "BLOTTER_EXPAND={value}: not all, none, captions or a number; all is used"
                    );
                    ExpandDepth::All
                })
            });
        Self {
            feed: Feed {
                rows: env("ROWS", 20_000),
                rate: env("RATE", 2_000),
                churn: env("CHURN", 20),
            },
            reload_delay: Duration::from_millis(env("RELOAD_MS", 1_500)),
            reorder,
            copy_displayed: std::env::var("BLOTTER_COPY").is_ok_and(|value| value == "displayed"),
            group_by,
            group_panel: env("BLOTTER_GROUP_PANEL", 1) != 0,
            totals: env("BLOTTER_TOTALS", 0) != 0,
            footers: env("BLOTTER_FOOTERS", 0) != 0,
            arrows,
            removal_fade: Duration::from_millis(env("BLOTTER_FADE", 0)),
            expand,
            quit_after: std::env::var("BLOTTER_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok()),
            layout_path,
            saved_layout,
        }
    }
}

struct Root {
    grid: Entity<LiveGridState<Blotter>>,
    /// The grid without its delegate type: what the toolbar and the status
    /// bar use.
    handle: LiveGridHandle,
    reorder: Reorder,
    /// The group panel shows (Group panel).
    group_panel: bool,
    /// Rises and falls show arrows (Arrows).
    arrows: bool,
    size: Size,
    size_select: Entity<SelectState<Vec<&'static str>>>,
    selection_select: Entity<SelectState<Vec<&'static str>>>,
    filter: Entity<InputState>,
    /// The counts and the held order, from the handle's events.
    status: GridStatus,
    /// The running reload; dropping it cancels the reload.
    reload: Option<Task<()>>,
    reload_delay: Duration,
    /// Tells the producer which trades were cancelled.
    cancelled: Sender<Vec<u64>>,
    /// The last `Confirm`, for the status bar.
    last_confirm: Option<SharedString>,
    /// The size of the last copy, for the status bar.
    last_copy: Option<SharedString>,
    /// The last `Click`, for the status bar.
    last_click: Option<SharedString>,
    /// The grid's counters, refreshed every half second.
    counters: SharedString,
    /// Root renders and the window's visibility, to tell "the grid does not
    /// paint" from "the window draws no frames at all" (occluded, locked).
    renders: u64,
    window_visible: bool,
    _tasks: [Task<()>; 2],
    /// Set by `BLOTTER_LAYOUT`.
    _layout_file: Option<Entity<LayoutFile>>,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Settings {
            feed,
            reload_delay,
            reorder,
            copy_displayed,
            group_by,
            group_panel,
            totals,
            footers,
            arrows,
            removal_fade,
            expand,
            quit_after,
            layout_path,
            saved_layout,
        } = settings;
        let grid = cx.new(|cx| {
            // The fade of removed trades is a store setting: at start only.
            // #region removal-fade
            let store = StoreOptions::default()
                .with_highlight(HighlightPolicy::default().with_removal_fade(removal_fade));
            // #endregion removal-fade
            let grid = LiveGridState::new_standalone(model::schema(), Blotter, store, cx)
                .with_selection_mode(SelectionMode::RowSet)
                .with_cell_export(if copy_displayed {
                    CellExport::Displayed(DisplayedCells)
                } else {
                    CellExport::default()
                })
                .with_view_id(VIEW_ID)
                .with_layout_codec(layout_codec())
                .with_change_paint(change_paint(arrows))
                .with_view(view(group_by, footers))
                .with_expand_depth(expand);
            // The grid's totals replace the application's Total row: two
            // "total" rows would confuse.
            let grid = if totals {
                grid.with_totals(PinnedEdge::Bottom)
            } else {
                grid
            };
            // Last: a saved layout overrides the sort, the grouping and the
            // expansion above.
            match saved_layout {
                Some(layout) => grid.with_layout(layout),
                None => grid,
            }
        });
        let handle = LiveGridHandle::new(&grid);
        reorder.apply(&handle, cx);
        let layout_file =
            layout_path.map(|path| cx.new(|cx| LayoutFile::new(path, grid.clone(), cx)));

        let size = Size::Small;
        let size_select = cx.new(|cx| {
            let selected = IndexPath::default().row(toolbar::position(&SIZES, &size));
            SelectState::new(toolbar::labels(&SIZES), Some(selected), window, cx)
        });
        let selection_select = cx.new(|cx| {
            let mode = grid.read(cx).selection_mode();
            let selected = IndexPath::default().row(toolbar::position(&SELECTION_MODES, &mode));
            SelectState::new(
                toolbar::labels(&SELECTION_MODES),
                Some(selected),
                window,
                cx,
            )
        });
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let this = cx.weak_entity();
        let subscriptions = vec![
            cx.subscribe(&grid, Self::on_grid_event),
            // The status bar's counts and the held order, without the
            // delegate type.
            // #region handle-subscribe
            handle.subscribe(cx, move |event, cx| {
                let _ = this.update(cx, |root, cx| {
                    let total = root.handle.total_row_count(cx);
                    if root.status.apply(event, total) {
                        cx.notify();
                    }
                });
            }),
            // #endregion handle-subscribe
            cx.subscribe(
                &size_select,
                |root, _, event: &SelectEvent<Vec<&'static str>>, cx| {
                    let SelectEvent::Confirm(Some(label)) = event else {
                        return;
                    };
                    if let Some(size) = toolbar::choice(&SIZES, label) {
                        root.size = size;
                        cx.notify();
                    }
                },
            ),
            cx.subscribe(
                &selection_select,
                |root, _, event: &SelectEvent<Vec<&'static str>>, cx| {
                    let SelectEvent::Confirm(Some(label)) = event else {
                        return;
                    };
                    if let Some(mode) = toolbar::choice(&SELECTION_MODES, label) {
                        root.grid
                            .update(cx, |grid, cx| grid.set_selection_mode(mode, cx));
                    }
                },
            ),
            cx.subscribe(&filter, |root, filter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = filter.read(cx).value();
                    root.handle.set_quick_filter(text, cx);
                }
            }),
        ];

        // BLOTTER_E2E_LOG: events, state and geometry for script/e2e.
        e2e::install(&grid, cx);

        let writer = grid.read(cx).writer();
        let (loaded, first_load) = smol::channel::bounded(1);
        let (cancelled, cancelled_keys) = smol::channel::unbounded();
        // The tests write their own trades: a thread writing into the store
        // would make the test scheduler nondeterministic.
        if !cfg!(test) {
            std::thread::spawn(move || {
                model::produce(writer, feed, !totals, loaded, cancelled_keys);
            });
        }
        // The first data set: once it is on screen, select its first row.
        let loading = cx.spawn(async move |this, cx| {
            if first_load.recv().await.is_err() {
                return;
            }
            let Ok(flushed) = this.update(cx, |root, cx| {
                root.grid.update(cx, |grid, cx| grid.flush(cx))
            }) else {
                return;
            };
            if flushed.await.is_some() {
                let _ = this.update(cx, |root, cx| root.select_first_row(cx));
            }
        });

        Self {
            grid,
            handle,
            reorder,
            group_panel,
            arrows,
            size,
            size_select,
            selection_select,
            filter,
            status: GridStatus::default(),
            reload: None,
            reload_delay,
            cancelled,
            last_confirm: None,
            last_copy: None,
            last_click: None,
            counters: SharedString::default(),
            renders: 0,
            window_visible: false,
            _tasks: [loading, Self::refresh_status(feed.rate, quit_after, cx)],
            _layout_file: layout_file,
            _subscriptions: subscriptions,
        }
    }

    fn on_grid_event(
        &mut self,
        _: Entity<LiveGridState<Blotter>>,
        event: &LiveGridEvent<u64, QuoteColumn>,
        cx: &mut Context<Self>,
    ) {
        // The counts and the held order come through the handle
        // (`GridStatus`); these events name rows.
        match event {
            LiveGridEvent::Confirm { keys, secondary } => {
                const SHOWN: usize = 5;
                let mut ids: Vec<String> = keys.iter().take(SHOWN).map(u64::to_string).collect();
                if keys.len() > SHOWN {
                    ids.push("…".into());
                }
                // Cmd/Ctrl+Enter is the secondary command.
                let how = if *secondary { " (secondary)" } else { "" };
                let trades = trades_label("Opened", keys.len());
                self.last_confirm = Some(format!("{trades}{how}: {}", ids.join(", ")).into());
            }
            LiveGridEvent::Click {
                target,
                button,
                click_count,
                ..
            } => self.last_click = Some(click_label(target, *button, *click_count).into()),
            _ => return,
        }
        cx.notify();
    }

    /// Updates the counters every half second; with `BLOTTER_SECONDS=N`
    /// prints them and quits after N seconds (an unattended end-to-end
    /// check on real threads).
    fn refresh_status(rate: u64, quit_after: Option<u64>, cx: &mut Context<Self>) -> Task<()> {
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let mut last_frames = 0;
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let Ok(done) = this.update(cx, |root: &mut Root, cx| {
                    let grid = root.grid.read(cx);
                    let stats = grid.stats();
                    let snapshot = grid.snapshot();
                    let fps = (stats.frames() - last_frames) * 2;
                    last_frames = stats.frames();
                    let projection = grid.projection();
                    // Groups, the captions of the last frame and the default
                    // expansion: O(1), every half second.
                    let groups = if projection.is_grouped() {
                        format!(
                            " · groups {}, captions shown {}, expanded {}",
                            projection.groups().len(),
                            stats.painted_captions(),
                            depth_label(grid.expand_depth()),
                        )
                    } else {
                        String::new()
                    };
                    root.counters = format!(
                        "window renders {} (visible {}) · {} rows, {rate} updates/s · {fps} fps · rebuilds {} (last {:.1} ms, max {:.1} ms) · prepaint {:.2} ms · cells {} · snapshots {} · orders shown {} · order held {} · pinned {}+{}{groups}",
                        root.renders,
                        root.window_visible,
                        snapshot.len(),
                        stats.rebuilds(),
                        stats.last_rebuild().as_secs_f64() * 1e3,
                        stats.max_rebuild().as_secs_f64() * 1e3,
                        stats.last_prepaint().as_secs_f64() * 1e3,
                        stats.visible_cells(),
                        stats.snapshots(),
                        root.status.orders_shown,
                        grid.is_order_held(),
                        snapshot.pinned_rows(PinnedEdge::Top).len(),
                        snapshot.pinned_rows(PinnedEdge::Bottom).len(),
                    )
                    .into();
                    if quit_after.is_some() {
                        out!("{:>5.1}s  {}", started.elapsed().as_secs_f64(), root.counters);
                    }
                    cx.notify();
                    quit_after.is_some_and(|seconds| started.elapsed().as_secs() >= seconds)
                }) else {
                    break;
                };
                if done {
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
            }
        })
    }

    /// Selects the first trade shown, after a load: under group captions,
    /// the first line that is a trade.
    fn select_first_row(&mut self, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| {
            let projection = grid.projection();
            let first = (0..projection.len())
                .find_map(|ix| projection.row(ix))
                .and_then(|id| grid.snapshot().row(id))
                .map(|row| *row.key());
            grid.select_key(first, cx);
        });
    }

    fn set_group_panel(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.group_panel = shown;
        cx.notify();
    }

    fn on_toggle_group_panel(
        &mut self,
        _: &ToggleGroupPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_group_panel(!self.group_panel, cx);
    }

    /// Arrows: rises and falls in their own colours with an arrow, or one
    /// colour per change kind. From the next frame, without a rebuild.
    fn set_arrows(&mut self, arrows: bool, cx: &mut Context<Self>) {
        self.arrows = arrows;
        self.grid.update(cx, |grid, cx| {
            grid.set_change_paint(change_paint(arrows), cx)
        });
        cx.notify();
    }

    fn set_reorder(&mut self, reorder: Reorder, cx: &mut Context<Self>) {
        self.reorder = reorder;
        reorder.apply(&self.handle, cx);
        cx.notify();
    }

    /// Reloads the trades while the producer keeps writing: the store
    /// buffers its writes until the "server" answers, then replays them
    /// over the answer (`ReplayLive`), so no live change is lost. Prices
    /// freeze on screen meanwhile; the progress leaves with the new rows.
    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.reload.is_some() {
            return;
        }
        let writer = self.grid.read(cx).writer();
        writer.set_progress(Some(
            gpui_kit_grid::GridProgress::indeterminate().with_label("Loading snapshot"),
        ));
        let reload = writer.begin_reload();
        let delay = self.reload_delay;
        self.reload = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            // The store as the reload found it: its writes since are buffered.
            let snapshot = writer.snapshot();
            let rows = cx
                .background_spawn(async move { model::server_snapshot(&snapshot) })
                .await;
            // One batch: the progress leaves in the snapshot of the new rows.
            writer.batch(|batch| {
                batch.finish_reload(reload, rows);
                batch.set_progress(None);
            });
            // A new data set shows under any reorder policy. `flush` after
            // `finish`: it does not wait for writes still buffered.
            let Ok(flushed) = this.update(cx, |root, cx| {
                root.grid.update(cx, |grid, cx| grid.flush(cx))
            }) else {
                return;
            };
            let flushed = flushed.await;
            let _ = this.update(cx, |root, cx| {
                root.reload = None;
                // The selection is by key and survives the reload; select
                // the first row only if none of it did.
                if flushed.is_some() && root.grid.read(cx).selection().is_empty() {
                    root.select_first_row(cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Drops the running reload without `finish`: the buffered writes apply
    /// as usual, and the progress leaves.
    fn cancel_reload(&mut self, cx: &mut Context<Self>) {
        if self.reload.take().is_some() {
            self.grid.read(cx).writer().set_progress(None);
            cx.notify();
        }
    }

    fn on_reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    /// Removes the trades of the command scope as it is when the command
    /// runs (`scoped_selection`), not as the menu saw it: rows may have
    /// left or been filtered out since. A product would ask first
    /// (`AlertDialog`); this is an example.
    fn on_cancel_trades(&mut self, _: &CancelTrades, _: &mut Window, cx: &mut Context<Self>) {
        let grid = self.grid.read(cx);
        let keys = command_keys(grid);
        if keys.is_empty() {
            return;
        }
        let writer = grid.writer();
        for key in &keys {
            writer.remove(*key);
        }
        let _ = self.cancelled.try_send(keys);
    }

    fn on_focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter
            .update(cx, |filter, cx| filter.focus(window, cx));
    }

    /// Sees `Copy` on its way to the grid, to show what it copies: the
    /// selection, or the trades of a focused caption's group.
    fn on_copy(&mut self, _: &actions::Copy, _: &mut Window, cx: &mut Context<Self>) {
        let rows = command_keys(self.grid.read(cx)).len();
        if rows > 0 {
            self.last_copy = Some(format!("Copied {}", rows_label(rows)).into());
            cx.notify();
        }
    }

    /// Sees `CopyAll` on its way to the grid: every shown line (group
    /// captions too) and the pinned rows, under a header line.
    fn on_copy_all(&mut self, _: &actions::CopyAll, _: &mut Window, cx: &mut Context<Self>) {
        let grid = self.grid.read(cx);
        let snapshot = grid.snapshot();
        let rows = grid.projection().len()
            + snapshot.pinned_rows(PinnedEdge::Top).len()
            + snapshot.pinned_rows(PinnedEdge::Bottom).len();
        if rows > 0 {
            self.last_copy = Some(format!("Copied all {}", rows_label(rows)).into());
            cx.notify();
        }
    }

    /// Copy all expanded (the Groups menu): every line with every group
    /// expanded, the totals last. A large copy is written in the background.
    fn on_copy_expanded(&mut self, _: &CopyExpanded, _: &mut Window, cx: &mut Context<Self>) {
        let copied = self
            .grid
            .update(cx, |grid, cx| grid.copy_scope(ExportScope::Expanded, cx));
        if copied {
            let trades = self.grid.read(cx).projection().row_count();
            self.last_copy = Some(format!("Copied all expanded: {}", rows_label(trades)).into());
            cx.notify();
        }
    }

    fn render_status(&self) -> impl IntoElement + use<> {
        let state: Vec<SharedString> = self
            .status
            .items()
            .into_iter()
            .map(SharedString::from)
            .chain(
                [
                    self.last_confirm.clone(),
                    self.last_copy.clone(),
                    self.last_click.clone(),
                ]
                .into_iter()
                .flatten(),
            )
            .collect();
        StatusBar::new()
            .left(self.counters.clone())
            .right(SharedString::from(state.join(" · ")))
    }
}

/// The keys a command acts on now: the trades of a focused group caption,
/// else the scoped selection. O(rows of the group), as Cmd/Ctrl+A.
fn command_keys(grid: &LiveGridState<Blotter>) -> Vec<u64> {
    let caption = grid.focused_cell().and_then(|cell| cell.group());
    match caption.and_then(|path| grid.projection().groups().find(path)) {
        Some(group) => {
            let snapshot = grid.snapshot();
            // A fading trade of the group has no row any more.
            group
                .rows()
                .iter()
                .filter_map(|id| snapshot.row(*id))
                .map(|row| *row.key())
                .collect()
        }
        None => grid.scoped_selection().keys().to_vec(),
    }
}

/// The default expansion for the status bar: `all`, `captions`, `levels 1`.
fn depth_label(depth: ExpandDepth) -> String {
    match depth {
        ExpandDepth::All => "all".into(),
        ExpandDepth::Captions => "captions".into(),
        ExpandDepth::Levels(levels) => format!("levels {levels}"),
        _ => format!("{depth:?}"),
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        self.window_visible = window.is_visible();
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_reload))
            .on_action(cx.listener(Self::on_cancel_trades))
            .on_action(cx.listener(Self::on_focus_filter))
            .on_action(cx.listener(Self::on_toggle_group_panel))
            .on_action(cx.listener(Self::on_copy_expanded))
            .capture_action(cx.listener(Self::on_copy))
            .capture_action(cx.listener(Self::on_copy_all))
            .bg(cx.theme().background)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        LiveGrid::new(&self.grid)
                            .with_size(self.size)
                            .bordered(false)
                            .group_panel(self.group_panel)
                            // The grid's name for assistive technology.
                            .accessibility_label("Trades"),
                    )
                    .map(|this| e2e::measure_grid(this, &self.grid, self.size, self.group_panel)),
            )
            .child(self.render_status())
    }
}

fn main() {
    let settings = Settings::from_env();

    gpui_kit::application()
        // gpui-kit's icons (sort arrows, menu checks): without them icons are blank.
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
            cx.bind_keys([
                KeyBinding::new("f5", Reload, Some(KEY_CONTEXT)),
                KeyBinding::new("secondary-f", FocusFilter, Some(KEY_CONTEXT)),
            ]);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                    None,
                    size(px(1400.), px(900.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                let root = cx.new(|cx| Root::new(settings, window, cx));
                // The keyboard starts in the grid.
                let grid = root.read(cx).grid.focus_handle(cx);
                window.focus(&grid, cx);
                root
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use gpui_kit_grid::{SelectionOrigin, SortOrigin};

    use super::*;

    #[test]
    fn test_grid_status() {
        let mut status = GridStatus::default();
        // A new order waits for the half-second refresh; the selection and
        // a held order show at once.
        assert!(!status.apply(
            &GridHandleEvent::ProjectionChange {
                row_count: 120,
                store_version: 3
            },
            20_000
        ));
        assert!(status.apply(
            &GridHandleEvent::SelectionChange {
                count: 3,
                origin: SelectionOrigin::User
            },
            20_000
        ));
        assert!(status.apply(&GridHandleEvent::OrderHeldChange { held: true }, 20_000));
        assert!(!status.apply(
            &GridHandleEvent::SortChange {
                origin: SortOrigin::User
            },
            20_000
        ));
        assert_eq!(status.orders_shown, 1);
        assert_eq!(
            status.items(),
            ["120 of 20000 rows", "Order held", "3 rows selected"]
        );
        // Every row shown, nothing selected, no order held.
        let status = GridStatus {
            shown: 1,
            total: 1,
            ..GridStatus::default()
        };
        assert_eq!(status.items(), ["1 row"]);
    }
}

// The toolbar on a headless window: its commands reach the grid
// through the handle.
#[cfg(all(test, feature = "test-support"))]
mod toolbar_tests {
    use gpui_kit::{AnyWindowHandle, TestAppContext, test::TestWindowExt as _};
    use gpui_kit_grid::LayoutGroup;

    use super::*;
    use crate::model::Feed;

    struct Fixture {
        window: AnyWindowHandle,
        root: Entity<Root>,
    }

    impl Fixture {
        fn settle(&self, cx: &mut TestAppContext) {
            for _ in 0..3 {
                cx.executor().advance_clock(Duration::from_millis(20));
                cx.run_until_parked();
                self.input(cx, |window, cx| window.render_frame(cx));
            }
        }

        fn input<R>(
            &self,
            cx: &mut TestAppContext,
            f: impl FnOnce(&mut Window, &mut App) -> R,
        ) -> R {
            cx.update_window(self.window, |_, window, cx| f(window, cx))
                .expect("window is open")
        }

        fn handle(&self, cx: &mut TestAppContext) -> LiveGridHandle {
            self.root.read_with(cx, |root, _| root.handle.clone())
        }
    }

    /// The blotter of `rows` trades without a stream.
    fn open(cx: &mut TestAppContext, rows: u64) -> Fixture {
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
        });
        let settings = Settings {
            feed: Feed {
                rows,
                rate: 0,
                churn: 0,
            },
            reload_delay: Duration::from_millis(10),
            reorder: Reorder::Live,
            copy_displayed: false,
            group_by: Vec::new(),
            group_panel: true,
            totals: false,
            footers: false,
            arrows: true,
            removal_fade: Duration::ZERO,
            expand: ExpandDepth::All,
            quit_after: None,
            layout_path: None,
            saved_layout: None,
        };
        let (window, root) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Default::default(),
                            size: size(px(1400.), px(900.)),
                        })),
                        ..Default::default()
                    },
                    cx,
                    |window, cx| cx.new(|cx| Root::new(settings, window, cx)),
                )
            })
            .expect("open window");
        let fixture = Fixture { window, root };
        // The first load, as the producer writes it.
        let writer = fixture
            .root
            .read_with(cx, |root, cx| root.grid.read(cx).writer());
        writer.replace_all(model::test_trades(rows));
        fixture.settle(cx);
        let handle = fixture.handle(cx);
        assert_eq!(cx.update(|cx| handle.total_row_count(cx)), rows as usize);
        fixture
    }

    #[gpui_kit::test]
    fn test_toolbar_reaches_grid(cx: &mut TestAppContext) {
        let fixture = open(cx, 40);
        let handle = fixture.handle(cx);
        let grid = fixture.root.read_with(cx, |root, _| root.grid.clone());

        // Manual: the reorder policy reaches the grid through the handle.
        fixture.input(cx, |window, cx| window.click("Manual", cx));
        fixture.settle(cx);
        assert_eq!(
            fixture.root.read_with(cx, |root, _| root.reorder),
            Reorder::Manual
        );
        // New prices reverse the order: held, and the status bar says so.
        let trades: Vec<Quote> = grid.read_with(cx, |grid, _| {
            grid.snapshot()
                .iter()
                .map(|(_, trade)| trade.with_last(1_000. - trade.last()))
                .collect()
        });
        let writer = grid.read_with(cx, |grid, _| grid.writer());
        for trade in trades {
            writer.upsert(trade);
        }
        fixture.settle(cx);
        assert!(cx.update(|cx| handle.is_order_held(cx)));
        assert!(fixture.root.read_with(cx, |root, _| root.status.order_held));
        assert!(fixture.root.read_with(cx, |root, _| {
            root.status.items().contains(&"Order held".to_owned())
        }));
        // Refresh order shows it.
        fixture.input(cx, |window, cx| window.click("refresh-order", cx));
        fixture.settle(cx);
        assert!(!cx.update(|cx| handle.is_order_held(cx)));
        assert!(!fixture.root.read_with(cx, |root, _| root.status.order_held));
        assert_eq!(fixture.root.read_with(cx, |root, _| root.status.shown), 40);

        // Columns… opens the grid's column chooser.
        fixture.input(cx, |window, cx| window.click("columns", cx));
        fixture.settle(cx);
        assert!(cx.update(|cx| handle.is_column_chooser_open(cx)));
        fixture.input(cx, |window, cx| window.press("escape", cx));
        fixture.settle(cx);

        // The Groups menu reads the grouping through the handle.
        assert!(cx.update(|cx| toolbar::is_flat(&handle, cx)));
        cx.update(|cx| {
            assert!(handle.set_group_by(
                [LayoutGroup::new("instrument", SortDirection::Ascending)],
                cx
            ))
        });
        fixture.settle(cx);
        assert!(!cx.update(|cx| toolbar::is_flat(&handle, cx)));

        // The filter is the grid's quick filter.
        fixture.input(cx, |window, cx| {
            window.dispatch_action(Box::new(FocusFilter), cx)
        });
        fixture.settle(cx);
        fixture.input(cx, |window, cx| window.input("INS001", cx));
        fixture.settle(cx);
        assert_eq!(cx.update(|cx| handle.quick_filter(cx)), "INS001");
    }
}

// BLOTTER_LAYOUT: the grid layout survives a restart.
//
// Everything for it is in this part. The rest of the example names the view,
// sets the codec and passes the saved layout (`with_layout` last) when it
// builds the grid, and keeps the `LayoutFile` entity in `Root`.

/// The view id of the blotter grid, written into its saved layout.
const VIEW_ID: &str = "quotes";

/// How long the layout file waits after the last change before it is written.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// The format of the layout file: the grid's JSON, indented to read by hand.
fn layout_codec() -> JsonLayoutCodec {
    JsonLayoutCodec::default().pretty(true)
}

/// The layout saved in `path`. A missing file is a first start. A file that
/// cannot be used is reported, and the grid opens with its default layout.
fn read_layout(path: &Path) -> Option<GridLayout> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            out!("layout file {}: new", path.display());
            return None;
        }
        Err(error) => {
            err!("layout file {}: not read: {error}", path.display());
            return None;
        }
    };
    // `with_layout` ignores a layout of another view silently; say why here.
    let layout = layout_codec()
        .decode(&text)
        .and_then(|layout| match layout.view_id() {
            Some(found) if found != VIEW_ID => Err(LayoutError::ViewMismatch {
                expected: VIEW_ID.into(),
                found: found.into(),
            }),
            _ => Ok(layout),
        });
    match layout {
        Ok(layout) => {
            out!("layout file {}: restored", path.display());
            Some(layout)
        }
        Err(error) => {
            err!(
                "layout file {}: {error}; the default layout is used",
                path.display()
            );
            None
        }
    }
}

/// Writes the grid layout to the `BLOTTER_LAYOUT` file: `SAVE_DELAY` after
/// the grid's `LayoutChange`, `SortChange`, `GroupChange` or
/// `ExpansionChange` (a newer change restarts the wait), when the
/// application quits and when the window closes. Errors go to stderr; the
/// next save tries again.
struct LayoutFile {
    path: PathBuf,
    grid: Entity<LiveGridState<Blotter>>,
    save: Option<Task<()>>,
    /// Dropped on quit, which closes the window too: quitting saves once.
    save_on_close: Option<Subscription>,
    _subscriptions: [Subscription; 2],
}

impl LayoutFile {
    fn new(path: PathBuf, grid: Entity<LiveGridState<Blotter>>, cx: &mut Context<Self>) -> Self {
        let save_on_change = cx.subscribe(
            &grid,
            |this, _, event: &LiveGridEvent<u64, QuoteColumn>, cx| {
                if matches!(
                    event,
                    LiveGridEvent::LayoutChange { .. }
                        | LiveGridEvent::SortChange { .. }
                        | LiveGridEvent::GroupChange { .. }
                        | LiveGridEvent::ExpansionChange { .. }
                ) {
                    this.schedule_save(cx);
                }
            },
        );
        // `BLOTTER_SECONDS` quits through `App::quit` too.
        let save_on_quit = cx.on_app_quit(|this, cx| {
            this.save_on_close = None;
            this.save_now(cx);
            async {}
        });
        // Closing the window releases `Root` and this entity. On macOS the
        // application keeps running without a window.
        let save_on_close = cx.on_release(|this, cx| this.save_now(cx));
        Self {
            path,
            grid,
            save: None,
            save_on_close: Some(save_on_close),
            _subscriptions: [save_on_change, save_on_quit],
        }
    }

    /// Saves in `SAVE_DELAY`; a newer request replaces this one.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.save_now(cx));
        }));
    }

    fn save_now(&mut self, cx: &App) {
        self.save = None;
        let written = self
            .grid
            .read(cx)
            .export_layout()
            .map_err(|error| error.to_string())
            .and_then(|text| std::fs::write(&self.path, text).map_err(|error| error.to_string()));
        if let Err(error) = written {
            err!("layout file {}: not saved: {error}", self.path.display());
        }
    }
}
