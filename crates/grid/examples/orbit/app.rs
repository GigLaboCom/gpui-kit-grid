//! The orbit example itself: the sky table, the sky plot, the selected
//! object's detail and the status bar, over the producer. The native
//! `main` (`main.rs`) opens it in a window; the browser build
//! (`crates/grid-web/apps/orbit`, the page `orbit/`) in the page's canvas.
//! Both call `setup` once, then `open`.

use std::{sync::Arc, time::Duration};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    status_bar::StatusBar,
    tab::{Tab, TabBar},
    tag::Tag,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, Keystroke, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{
    GridStore, GridTarget, LiveGrid, LiveGridEvent, LiveGridState, PinnedEdge, SelectionMode,
    SortDirection,
};

mod detail;
mod plot;
pub mod producer;
pub mod rows;
pub mod sky;
#[cfg(feature = "test-support")]
pub mod snapshot;
mod table;
mod theme;
mod ui;

pub use self::theme::OrbitTheme;
use self::{
    producer::{Control, Producer, SkyFrame, Warp, real_now_ms},
    rows::{SkyColumn, SkyKey},
    sky::{Observer, format_span, format_utc, thousands},
    table::{Arrangement, SkyTable},
    ui::{GAP, PAGE_PADDING, ROW_HEIGHT},
};
use crate::trading::{theme::TradingTheme as _, worker};

gpui_kit::actions!(
    orbit,
    [
        /// Time ×1, ×10, ×60, then ×1 again.
        CycleWarp,
        /// Shows only the objects above the horizon, or every object.
        ToggleUpOnly,
        /// Groups the table by constellation, or not.
        ToggleGrouping,
        /// Shows the table as a tree of constellations and shells, or flat.
        ToggleTree,
        /// The selected object's pass after the one shown.
        NextPass,
        /// Moves the focus to the search box.
        FocusSearch,
    ]
);

/// The key context of the window content.
pub const KEY_CONTEXT: &str = "Orbit";
/// Around the table: its letter keys work there, never in the search box.
const TABLE_CONTEXT: &str = "OrbitSky";
const TABLE_KEYS: &str = "OrbitSky > LiveGrid";

/// The keys, as data: the bindings and the app bar's hints are both made
/// from this table. The letters act in the table, which has the keyboard
/// at start; Enter in the search box gives it back.
const KEYS: [(&str, Command); 6] = [
    ("w", Command::Warp),
    ("h", Command::UpOnly),
    ("g", Command::Grouping),
    ("t", Command::Tree),
    ("n", Command::NextPass),
    ("secondary-f", Command::FocusSearch),
];

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Warp,
    UpOnly,
    Grouping,
    Tree,
    NextPass,
    FocusSearch,
}

impl Command {
    fn binding(self, keystrokes: &str) -> KeyBinding {
        let table = Some(TABLE_KEYS);
        match self {
            Self::Warp => KeyBinding::new(keystrokes, CycleWarp, table),
            Self::UpOnly => KeyBinding::new(keystrokes, ToggleUpOnly, table),
            Self::Grouping => KeyBinding::new(keystrokes, ToggleGrouping, table),
            Self::Tree => KeyBinding::new(keystrokes, ToggleTree, table),
            Self::NextPass => KeyBinding::new(keystrokes, NextPass, table),
            Self::FocusSearch => KeyBinding::new(keystrokes, FocusSearch, Some(KEY_CONTEXT)),
        }
    }

    /// The app bar's word for it.
    fn hint(self) -> &'static str {
        match self {
            Self::Warp => "Time",
            Self::UpOnly => "Above horizon",
            Self::Grouping => "Group",
            Self::Tree => "Tree",
            Self::NextPass => "Next pass",
            Self::FocusSearch => "Search",
        }
    }
}

/// Every binding of `KEYS`.
fn key_bindings() -> Vec<KeyBinding> {
    KEYS.iter()
        .map(|(keystrokes, command)| command.binding(keystrokes))
        .collect()
}

/// Where the native build finds the data `script/fetch-orbit-data`
/// downloads.
#[cfg(not(target_family = "wasm"))]
pub const DATA_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/data/celestrak-active.json"
);
/// The script, for the hint when the data is missing.
pub const FETCH_SCRIPT: &str = "python3 -I script/fetch-orbit-data";

/// What the example starts with: natively from the environment
/// (`from_env`), in the browser from the page.
pub struct Settings {
    observer: Observer,
    site: SharedString,
    start_ms: Option<f64>,
    data: Result<Vec<u8>, String>,
    /// When the data was downloaded, Unix ms, if known.
    fetched_ms: Option<f64>,
    /// Steps a sweep is spread over: 1 natively.
    chunks: usize,
}

impl Settings {
    /// The data, `ORBIT_SITE=lat,lon,alt_m` (Greenwich by default),
    /// `ORBIT_TIME` (a fixed start, UTC) and `ORBIT_DATA` (another data
    /// file).
    #[cfg(not(target_family = "wasm"))]
    pub fn from_env() -> Self {
        let path = std::env::var("ORBIT_DATA").unwrap_or_else(|_| DATA_FILE.into());
        let data = std::fs::read(&path).map_err(|error| format!("{path}: {error}"));
        let fetched_ms = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs_f64() * 1e3);
        Self::new(
            data,
            std::env::var("ORBIT_SITE").ok().as_deref(),
            std::env::var("ORBIT_TIME").ok().as_deref(),
            fetched_ms,
            1,
        )
    }

    /// `data` (OMM JSON, or why there is none) seen from `site`
    /// (`lat,lon,alt_m`; Greenwich if `None`), from `start` (UTC; now if
    /// `None`), its sweep spread over `chunks` steps.
    pub fn new(
        data: Result<Vec<u8>, String>,
        site: Option<&str>,
        start: Option<&str>,
        fetched_ms: Option<f64>,
        chunks: usize,
    ) -> Self {
        let parsed = site.filter(|site| !site.trim().is_empty()).map(|text| {
            Observer::parse(text).ok_or_else(|| {
                err!("ORBIT_SITE={text}: not lat,lon,alt_m; Greenwich is used");
            })
        });
        let (observer, site) = match parsed {
            Some(Ok(observer)) => (observer, "Observer".into()),
            _ => (Observer::greenwich(), "Royal Observatory Greenwich".into()),
        };
        let start_ms = start.and_then(|text| {
            let parsed = sky::parse_utc(text);
            if parsed.is_none() {
                err!("ORBIT_TIME={text}: not 2026-11-06T17:30:00Z; the clock is used");
            }
            parsed
        });
        Self {
            observer,
            site,
            start_ms,
            data,
            fetched_ms,
            chunks: chunks.max(1),
        }
    }

    /// The same, starting at `start_ms` (Unix ms): the snapshot's fixed
    /// time.
    #[cfg_attr(not(feature = "test-support"), allow(dead_code))]
    pub fn with_start(mut self, start_ms: f64) -> Self {
        self.start_ms = Some(start_ms);
        self
    }
}

/// gpui-kit, the grid's key bindings (arrows, Page, Home/End, Enter,
/// Escape, `+`/`-`), then `configure`. Call once.
pub fn setup(cx: &mut App) {
    gpui_kit::init(cx);
    gpui_kit_grid::init(cx);
    configure(cx);
}

/// After `gpui_kit::init` and `gpui_kit_grid::init`: the theme with its
/// fonts, and the keys.
pub fn configure(cx: &mut App) {
    crate::trading::theme::install::<OrbitTheme>(cx);
    OrbitTheme::Night.apply(cx);
    cx.bind_keys(key_bindings());
}

/// The orbit view in `window`, the keyboard in its table, its producer
/// started.
pub fn open(settings: Settings, window: &mut Window, cx: &mut App) -> Entity<Orbit> {
    let orbit = cx.new(|cx| Orbit::new(settings, window, cx));
    let focus = orbit.read(cx).focus_handle(cx);
    window.focus(&focus, cx);
    orbit
}

/// The grid's counters, refreshed every second.
#[derive(Default)]
struct Status {
    rebuild: Duration,
    frame: Duration,
}

pub struct Orbit {
    control: Arc<Control>,
    grid: Entity<LiveGridState<SkyTable>>,
    search: Entity<InputState>,
    arrangement: Arrangement,
    up_only: bool,
    frame: Arc<SkyFrame>,
    status: Status,
    observer: Observer,
    site: SharedString,
    fetched_ms: Option<f64>,
    /// Why there is nothing to show, if there is not.
    error: Option<SharedString>,
    /// The object selected in the table once its row arrives, and whether
    /// the table scrolls to it.
    pending_selection: Option<(u32, bool)>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Orbit {
    fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Settings {
            observer,
            site,
            start_ms,
            data,
            fetched_ms,
            chunks,
        } = settings;
        let control = Control::new(start_ms);
        let store = GridStore::new(table::store_options(), cx);
        let grid = cx.new(|cx| {
            let mut grid = LiveGridState::new(table::schema(), SkyTable, store.clone(), cx)
                .with_view_id("sky")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(table::rise_set_changes(cx))
                .with_quick_filter_columns([
                    SkyColumn::Name,
                    SkyColumn::Norad,
                    SkyColumn::Constellation,
                    SkyColumn::Shell,
                ])
                .with_view(table::view(Arrangement::Flat, false, table::default_sort()))
                .with_expand_depth(table::TREE_DEPTH);
            for (column, visible) in Arrangement::Flat.column_visibility() {
                grid.set_column_visible(column, visible, cx);
            }
            grid
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search name, NORAD or constellation")
        });
        let subscriptions = vec![
            cx.subscribe(&grid, Self::on_grid_event),
            cx.subscribe_in(&search, window, |orbit, search, event, window, cx| {
                match event {
                    InputEvent::Change => {
                        let text = search.read(cx).value();
                        orbit
                            .grid
                            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
                    }
                    // Back to the table, where the letter keys work.
                    InputEvent::PressEnter { .. } => {
                        let focus = orbit.grid.focus_handle(cx);
                        window.focus(&focus, cx);
                    }
                    _ => {}
                }
            }),
        ];
        let mut tasks = vec![Self::poll(cx)];
        let error = match data {
            Ok(data) => {
                let mut producer =
                    Producer::new(data, observer, store.writer(), control.clone(), chunks);
                tasks.push(worker::repeat(
                    producer.interval(),
                    move || producer.step(),
                    cx,
                ));
                None
            }
            Err(error) => {
                err!("no orbital data ({error}): run {FETCH_SCRIPT}");
                Some(error.into())
            }
        };
        Self {
            pending_selection: control.selected().map(|norad| (norad, false)),
            control,
            grid,
            search,
            arrangement: Arrangement::Flat,
            up_only: false,
            frame: Arc::default(),
            status: Status::default(),
            observer,
            site,
            fetched_ms,
            error,
            _tasks: tasks,
            _subscriptions: subscriptions,
        }
    }

    /// Four times a second: a new frame from the producer and the clock;
    /// the counters once a second.
    fn poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut seen = 0;
            let mut ticks = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                ticks += 1;
                let Ok(()) = this.update(cx, |orbit, cx| {
                    let version = orbit.control.version();
                    if version != seen {
                        seen = version;
                        orbit.frame = orbit.control.frame();
                        if let Some(error) = orbit.control.error() {
                            orbit.error = Some(error.into());
                        }
                    }
                    orbit.select_pending(cx);
                    if ticks % 4 == 0 {
                        let stats = orbit.grid.read(cx).stats();
                        orbit.status = Status {
                            rebuild: stats.last_rebuild(),
                            frame: stats.last_prepaint(),
                        };
                    }
                    cx.notify();
                }) else {
                    break;
                };
            }
        })
    }

    /// Selects the object asked for once its row is in the table, and
    /// scrolls to it if asked: not at start, where the table opens on
    /// what is overhead and the first selection is pinned on top.
    fn select_pending(&mut self, cx: &mut Context<Self>) {
        let Some((norad, scroll)) = self.pending_selection else {
            return;
        };
        let key = SkyKey::Object(norad);
        let present = self.grid.read(cx).snapshot().get(&key).is_some();
        if present {
            self.pending_selection = None;
            self.grid.update(cx, |grid, cx| {
                grid.select_key(Some(key), cx);
                if scroll {
                    grid.scroll_to_key(&key, cx);
                }
            });
        }
    }

    /// Selects the object `norad` in the table, the detail and the plot.
    pub fn select(&mut self, norad: u32, cx: &mut Context<Self>) {
        self.control.select(Some(norad));
        self.pending_selection = Some((norad, true));
        self.select_pending(cx);
        cx.notify();
    }

    /// Shows the objects as `arrangement`.
    pub fn set_arrangement(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        if self.arrangement != arrangement {
            self.arrangement = arrangement;
            self.apply_view(cx);
        }
    }

    /// Shows only the objects above the horizon, or every object.
    pub fn set_up_only(&mut self, up_only: bool, cx: &mut Context<Self>) {
        if self.up_only != up_only {
            self.up_only = up_only;
            self.apply_view(cx);
        }
    }

    /// Runs time `warp` times as fast.
    pub fn set_warp(&mut self, warp: Warp, cx: &mut Context<Self>) {
        self.control.set_warp(warp);
        cx.notify();
    }

    fn apply_view(&mut self, cx: &mut Context<Self>) {
        let (arrangement, up_only) = (self.arrangement, self.up_only);
        self.grid.update(cx, |grid, cx| {
            let sort = grid.sort().to_vec();
            grid.set_view(table::view(arrangement, up_only, sort), cx);
            for (column, visible) in arrangement.column_visibility() {
                grid.set_column_visible(column, visible, cx);
            }
            let totals = (arrangement == Arrangement::Tree).then_some(PinnedEdge::Bottom);
            grid.set_totals(totals, cx);
        });
        cx.notify();
    }

    /// A selected object goes to the detail and the plot; a click on a
    /// pinned row selects its object in the table.
    fn on_grid_event(
        &mut self,
        _: Entity<LiveGridState<SkyTable>>,
        event: &LiveGridEvent<SkyKey, SkyColumn>,
        cx: &mut Context<Self>,
    ) {
        match event {
            LiveGridEvent::SelectionChange { selection, .. } => {
                if let Some(norad) = selection.iter().find_map(|key| key.norad()) {
                    self.control.select(Some(norad));
                    cx.notify();
                }
            }
            LiveGridEvent::Click {
                target: GridTarget::Pinned { key, .. },
                ..
            } => {
                if let Some(norad) = key.norad() {
                    self.select(norad, cx);
                }
            }
            _ => {}
        }
    }

    fn on_cycle_warp(&mut self, _: &CycleWarp, _: &mut Window, cx: &mut Context<Self>) {
        let warp = self.control.warp().next();
        self.set_warp(warp, cx);
    }

    fn on_toggle_up_only(&mut self, _: &ToggleUpOnly, _: &mut Window, cx: &mut Context<Self>) {
        self.set_up_only(!self.up_only, cx);
    }

    fn on_toggle_grouping(&mut self, _: &ToggleGrouping, _: &mut Window, cx: &mut Context<Self>) {
        self.set_arrangement(self.arrangement.grouping_toggled(), cx);
    }

    fn on_toggle_tree(&mut self, _: &ToggleTree, _: &mut Window, cx: &mut Context<Self>) {
        self.set_arrangement(self.arrangement.tree_toggled(), cx);
    }

    fn on_next_pass(&mut self, _: &NextPass, _: &mut Window, cx: &mut Context<Self>) {
        self.control.next_pass();
        cx.notify();
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The mark, the clock and its warp, the observer and the keys.
    fn render_app_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let warp = self.control.warp();
        let clock = format!("{} UTC", format_utc(self.control.now()));
        let hints = KEYS.iter().filter_map(|(keystrokes, command)| {
            let keystroke = Keystroke::parse(keystrokes).ok()?;
            Some(
                h_flex()
                    .gap(px(5.))
                    .child(Kbd::new(keystroke).outline())
                    .child(command.hint()),
            )
        });
        h_flex()
            .flex_none()
            .w_full()
            .h(px(44.))
            .px(PAGE_PADDING + px(4.))
            .gap(px(18.))
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .child(div().size(px(9.)).rounded_full().bg(theme.primary))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("orbit"),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            // The clock and its warp beside it say how live it is.
                            .child("everything in orbit"),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(10.))
                    .child(
                        ui::figures(cx)
                            .text_color(theme.foreground)
                            .child(SharedString::from(clock)),
                    )
                    .child(match warp {
                        Warp::RealTime => Tag::secondary().outline().small().child("real time"),
                        _ => Tag::primary()
                            .small()
                            .child(format!("time {}", warp.label())),
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} · {}", self.site, self.observer.label())),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(12.))
                    .text_color(theme.muted_foreground)
                    .children(hints),
            )
    }

    /// The search box, the horizon filter, the arrangement and the warp.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let up_only = self.up_only;
        h_flex()
            .flex_none()
            .w_full()
            .px(ui::PANEL_PADDING)
            .py(px(8.))
            .gap(px(8.))
            .child(
                Input::new(&self.search)
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .small()
                    .aria_label("Search objects")
                    .w(px(280.)),
            )
            .child(
                Button::new("up-only")
                    .label("Above horizon")
                    .small()
                    .toggled(up_only)
                    .map(|button| {
                        if up_only {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })
                    .tooltip("Show only the objects above the horizon (H)")
                    .on_click(cx.listener(move |orbit, _, _, cx| orbit.set_up_only(!up_only, cx))),
            )
            .child(
                TabBar::new("arrangement")
                    .segmented()
                    .small()
                    .selected_index(self.arrangement.ix())
                    .children(
                        Arrangement::ALL.map(|arrangement| Tab::new().label(arrangement.label())),
                    )
                    .on_click(cx.listener(|orbit, ix: &usize, _, cx| {
                        if let Some(arrangement) = Arrangement::ALL.get(*ix) {
                            orbit.set_arrangement(*arrangement, cx);
                        }
                    })),
            )
            .child(div().flex_1())
            .child(
                TabBar::new("warp")
                    .segmented()
                    .small()
                    .selected_index(self.control.warp().ix())
                    .children(Warp::ALL.map(|warp| Tab::new().label(warp.label())))
                    .on_click(cx.listener(|orbit, ix: &usize, _, cx| {
                        if let Some(warp) = Warp::ALL.get(*ix) {
                            orbit.set_warp(*warp, cx);
                        }
                    })),
            )
    }

    /// The table, or how to get the data it needs.
    fn render_table(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = &self.frame;
        let shown = self.grid.read(cx).projection().row_count();
        let detail = (frame.sweeps > 0).then(|| {
            SharedString::from(format!(
                "{} objects · {} above the horizon · {} shown",
                thousands(u64::from(frame.objects)),
                thousands(u64::from(frame.up)),
                thousands(shown as u64),
            ))
        });
        let body = match &self.error {
            Some(error) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .p(px(24.))
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.danger)
                        .child("No orbital data"),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(error.clone()),
                )
                .child(div().child(
                    "Download CelesTrak's active satellites (about 7 MB) once, then start again:",
                ))
                .child(
                    ui::figures(cx)
                        .text_color(theme.foreground)
                        .child(FETCH_SCRIPT),
                )
                .into_any_element(),
            None => ui::grid_frame(cx)
                .key_context(TABLE_CONTEXT)
                .child(
                    LiveGrid::new(&self.grid)
                        .with_size(Size::Size(ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Objects in orbit"),
                )
                .into_any_element(),
        };
        ui::panel(cx)
            .size_full()
            .child(ui::panel_header("Sky", detail, cx))
            .child(self.render_toolbar(cx))
            .child(body)
    }

    /// The sky plot and the selected object's detail.
    fn render_side(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = self.frame.clone();
        let selected = self.control.selected();
        let sun = frame.sun_elevation;
        let twilight = match sun {
            sun if sun > 0. => "day",
            sun if sun > -6. => "civil twilight",
            sun if sun > -12. => "nautical twilight",
            sun if sun > -18. => "astronomical twilight",
            _ => "night",
        };
        let plot_detail = (frame.sweeps > 0).then(|| {
            SharedString::from(format!(
                "{} above the horizon · Sun {:.0}°, {twilight}",
                thousands(u64::from(frame.up)),
                sun
            ))
        });
        let legend = h_flex()
            .gap(px(14.))
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .child(div().text_color(theme.warning).child("● sunlit"))
            .child(div().text_color(theme.info).child("● in shadow"))
            .child(div().text_color(theme.foreground).child("● station"))
            .child(
                div()
                    .text_color(theme.primary)
                    .child("○ selected, its pass"),
            );
        let now_ms = if frame.sweeps > 0 {
            frame.sim_ms
        } else {
            self.control.now()
        };
        let detail = frame
            .detail
            .as_ref()
            .filter(|detail| Some(detail.norad) == selected);
        v_flex()
            .flex_none()
            .w(px(520.))
            .h_full()
            .gap(GAP)
            .child(
                ui::panel(cx)
                    .flex_none()
                    .child(ui::panel_header("Sky plot", plot_detail, cx))
                    .child(
                        v_flex()
                            .items_center()
                            .py(px(6.))
                            .gap(px(4.))
                            .child(plot::sky_plot(frame.clone(), selected, cx))
                            .child(legend),
                    ),
            )
            .child(
                ui::panel(cx)
                    .flex_1()
                    .child(ui::panel_header("Object", None, cx))
                    .child(detail::render_detail(detail, now_ms, cx)),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = &self.frame;
        let age = match self.fetched_ms {
            Some(fetched) => format!("data {} old", format_span(real_now_ms() - fetched)),
            None if frame.median_epoch_ms > 0. => format!(
                "elements {} old (median)",
                format_span(frame.sim_ms - frame.median_epoch_ms)
            ),
            None => "data age unknown".into(),
        };
        StatusBar::new()
            .font_features(ui::tabular_figures())
            .left(SharedString::from(format!(
                "{} objects · {} up{}",
                thousands(u64::from(frame.objects)),
                thousands(u64::from(frame.up)),
                match frame.failed {
                    0 => String::new(),
                    lost => format!(" · {lost} not computable"),
                }
            )))
            .left(SharedString::from(format!(
                "{} updates/s",
                thousands(frame.rate)
            )))
            .left(SharedString::from(format!(
                "propagate {:.1} ms · write {:.1} ms · rebuild {:.1} ms · frame {:.2} ms",
                ms(frame.propagate),
                ms(frame.write),
                ms(self.status.rebuild),
                ms(self.status.frame),
            )))
            .left(SharedString::from(format!(
                "{} rose · {} set",
                frame.rises, frame.sets
            )))
            .left(SharedString::from(format!(
                "{age} · loaded in {:.0} ms",
                ms(frame.load)
            )))
            .right(
                div()
                    .text_color(theme.muted_foreground)
                    .child("Orbital data: CelesTrak · SGP4 · positions approximate"),
            )
    }
}

/// What the snapshot and the browser page drive and read the example
/// with: natively the snapshot calls part of it, with `test-support` only.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
impl Orbit {
    pub fn grid(&self) -> &Entity<LiveGridState<SkyTable>> {
        &self.grid
    }

    pub fn control(&self) -> &Arc<Control> {
        &self.control
    }

    pub fn arrangement(&self) -> Arrangement {
        self.arrangement
    }

    pub fn is_up_only(&self) -> bool {
        self.up_only
    }

    /// Sorts the table by `column`.
    pub fn set_sort(
        &mut self,
        column: SkyColumn,
        direction: SortDirection,
        cx: &mut Context<Self>,
    ) {
        self.grid
            .update(cx, |grid, cx| grid.set_sort([(column, direction)], cx));
    }

    /// Types `text` into the search box, as the keys would.
    pub fn type_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.grid.update(cx, |grid, cx| {
            grid.set_quick_filter(SharedString::from(text.to_owned()), cx)
        });
    }

    /// The status as one line, for the snapshot and the page.
    pub fn status_line(&self, cx: &App) -> String {
        let frame = &self.frame;
        let shown = self.grid.read(cx).projection().row_count();
        format!(
            "{} objects · {} up · {} shown · {} updates/s · propagate {:.1} ms · write {:.1} ms · rebuild {:.1} ms · frame {:.2} ms · {} rose, {} set · load {:.0} ms · time {}",
            thousands(u64::from(frame.objects)),
            thousands(u64::from(frame.up)),
            thousands(shown as u64),
            thousands(frame.rate),
            ms(frame.propagate),
            ms(frame.write),
            ms(self.status.rebuild),
            ms(self.status.frame),
            frame.rises,
            frame.sets,
            ms(frame.load),
            self.control.warp().label(),
        )
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

impl Focusable for Orbit {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for Orbit {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, family, text_size) = (
            theme.background,
            theme.foreground,
            theme.font_family.clone(),
            theme.font_size,
        );
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_cycle_warp))
            .on_action(cx.listener(Self::on_toggle_up_only))
            .on_action(cx.listener(Self::on_toggle_grouping))
            .on_action(cx.listener(Self::on_toggle_tree))
            .on_action(cx.listener(Self::on_next_pass))
            .on_action(cx.listener(Self::on_focus_search))
            .bg(background)
            .text_color(foreground)
            .font_family(family)
            .text_size(text_size)
            .child(self.render_app_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_start()
                    .p(PAGE_PADDING)
                    .gap(GAP)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_table(cx)),
                    )
                    .child(self.render_side(cx)),
            )
            .child(self.render_status(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keys_parse_and_name_each_command_once() {
        let bindings = key_bindings();
        assert_eq!(bindings.len(), KEYS.len());
        for (ix, (keystrokes, command)) in KEYS.iter().enumerate() {
            assert!(Keystroke::parse(keystrokes).is_ok(), "{keystrokes}");
            assert!(
                KEYS[ix + 1..]
                    .iter()
                    .all(|(other, again)| other != keystrokes && again != command),
                "{keystrokes}"
            );
        }
    }

    #[test]
    fn test_settings_fall_back_to_greenwich_and_the_clock() {
        let settings = Settings::new(Ok(Vec::new()), Some("north"), Some("soon"), None, 0);
        assert_eq!(settings.observer, Observer::greenwich());
        assert_eq!(settings.start_ms, None);
        assert_eq!(settings.chunks, 1);
        let settings = Settings::new(
            Ok(Vec::new()),
            Some("35.6586,139.7454,40"),
            Some("2026-11-06T17:30:00Z"),
            None,
            4,
        );
        assert_eq!(settings.observer, Observer::new(35.6586, 139.7454, 40.));
        assert!(settings.start_ms.is_some());
    }
}
