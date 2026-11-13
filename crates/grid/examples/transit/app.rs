//! The transit example itself: the vehicles table, the map, the station
//! board and the status bar over the producer and its source, Munich's
//! timetable or Helsinki's live feed ([`Choice`]). The native `main`
//! (`main.rs`) opens it in a window; the browser build
//! (`crates/grid-web/apps/transit`, the page `transit/`) in the page's
//! canvas. Both call `setup` once, then `open` with their [`Settings`],
//! whose [`Sources`] make the source of each choice.

use std::{sync::Arc, time::Duration};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size,
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    status_bar::StatusBar,
    switch::Switch,
    tab::{Tab, TabBar},
    tag::Tag,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, Keystroke, MouseButton, ParentElement as _,
    Pixels, Render, SharedString, StyleRefinement, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{
    GridStore, LiveGrid, LiveGridEvent, LiveGridState, SelectionMode, SortDirection,
};

pub mod board;
pub mod hsl;
mod map;
// Shared with the ships example: the live source's MQTT, in `hsl.rs`.
#[path = "../mqtt/mod.rs"]
pub mod mqtt;
#[cfg(not(target_family = "wasm"))]
pub mod native;
pub mod producer;
pub mod rows;
pub mod schedule;
#[cfg(feature = "test-support")]
pub mod snapshot;
pub mod source;
mod table;
mod theme;
pub mod timetable;
mod ui;
pub mod zone;

pub use self::theme::TransitTheme;
use self::{
    board::Board,
    map::{MapEvent, MapZoom, TransitMap},
    producer::{Control, Producer, TransitFrame, Warp, mode_counts, real_now_ms},
    rows::{TransitColumn, VehicleRow},
    source::{City, Mode, SourceClock, VehicleKey, VehicleSource},
    table::{Arrangement, Vehicles, line_colors, mode_color},
    ui::{GAP, PAGE_PADDING, ROW_HEIGHT, thousands},
    zone::{Day, Zone},
};
use crate::trading::theme::TradingTheme as _;

gpui_kit::actions!(
    transit,
    [
        /// Time ×1, ×10, ×60, then ×1 again.
        CycleWarp,
        /// Groups by line, by mode, or not at all.
        CycleGrouping,
        /// Shows the city on the map, or the whole region.
        ToggleZoom,
        /// Moves the focus to the search box.
        FocusSearch,
        /// Munich, then Helsinki, then Munich again.
        CycleCity,
        /// Munich's regional trains and buses, or Helsinki's buses, on or
        /// off.
        ToggleMoreLines,
    ]
);

/// The key context of the window content.
pub const KEY_CONTEXT: &str = "Transit";
/// Around the table: its letter keys work there, never in the search box.
const TABLE_CONTEXT: &str = "TransitTable";
const TABLE_KEYS: &str = "TransitTable > LiveGrid";

/// The keys, as data: the bindings and the app bar's hints are both made
/// from this table. The letters act in the table, which has the keyboard
/// at start; Enter in the search box gives it back.
const KEYS: [(&str, Command); 6] = [
    ("w", Command::Warp),
    ("g", Command::Grouping),
    ("z", Command::Zoom),
    ("c", Command::City),
    ("o", Command::MoreLines),
    ("secondary-f", Command::FocusSearch),
];

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Warp,
    Grouping,
    Zoom,
    City,
    MoreLines,
    FocusSearch,
}

impl Command {
    fn binding(self, keystrokes: &str) -> KeyBinding {
        let table = Some(TABLE_KEYS);
        match self {
            Self::Warp => KeyBinding::new(keystrokes, CycleWarp, table),
            Self::Grouping => KeyBinding::new(keystrokes, CycleGrouping, table),
            Self::Zoom => KeyBinding::new(keystrokes, ToggleZoom, table),
            Self::City => KeyBinding::new(keystrokes, CycleCity, table),
            Self::MoreLines => KeyBinding::new(keystrokes, ToggleMoreLines, table),
            Self::FocusSearch => KeyBinding::new(keystrokes, FocusSearch, Some(KEY_CONTEXT)),
        }
    }

    /// The app bar's word for it.
    fn hint(self) -> &'static str {
        match self {
            Self::Warp => "Time",
            Self::Grouping => "Group",
            Self::Zoom => "Map",
            Self::City => "City",
            Self::MoreLines => "More lines",
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

/// The script, for the hint when the timetable is missing.
pub const FETCH_SCRIPT: &str = "python3 -I script/fetch-transit-data";
/// Below this width the map goes above the table.
const NARROW: Pixels = px(900.);
/// The station names the board's search box suggests at most.
const SUGGESTIONS: usize = 6;

/// Which vehicles: the city, and the lines it adds as an option (Munich's
/// regional trains and buses, Helsinki's buses).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    city: City,
    regional: bool,
    buses: bool,
}

impl Choice {
    pub fn new(city: City) -> Self {
        Self {
            city,
            ..Self::default()
        }
    }

    /// Munich's regional trains and buses too.
    pub fn with_regional(mut self, regional: bool) -> Self {
        self.regional = regional;
        self
    }

    /// Helsinki's buses too.
    pub fn with_buses(mut self, buses: bool) -> Self {
        self.buses = buses;
        self
    }

    pub fn city(&self) -> City {
        self.city
    }

    pub fn is_regional(&self) -> bool {
        self.regional
    }

    pub fn has_buses(&self) -> bool {
        self.buses
    }

    /// The city's option: Munich's regional lines, Helsinki's buses.
    pub fn has_more_lines(&self) -> bool {
        match self.city {
            City::Munich => self.regional,
            City::Helsinki => self.buses,
        }
    }

    /// The same with the city's option turned `on` or off.
    pub fn with_more_lines(self, on: bool) -> Self {
        match self.city {
            City::Munich => self.with_regional(on),
            City::Helsinki => self.with_buses(on),
        }
    }

    /// The option's name in the app bar.
    pub fn more_lines_label(&self) -> &'static str {
        match self.city {
            City::Munich => "Regional",
            City::Helsinki => "Buses",
        }
    }

    /// The page's query of it: `?city=helsinki&buses`, `?regional`, or
    /// empty for Munich's city lines.
    #[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
    pub fn query(&self) -> String {
        let mut parts = Vec::new();
        if self.city != City::Munich {
            parts.push(format!("city={}", self.city.id()));
        }
        if self.regional {
            parts.push("regional".into());
        }
        if self.buses {
            parts.push("buses".into());
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        }
    }
}

/// Makes the source of a [`Choice`]: natively from the files and the
/// network (`native.rs`), in the browser from what the page fetched.
pub trait Sources: 'static {
    /// The source of `choice`, or why there is none; `None` when the host
    /// starts over with it instead (the page reloads with its query).
    fn source(&mut self, choice: Choice) -> Option<Result<Box<dyn VehicleSource>, String>>;
}

/// What the example starts with: natively from the environment
/// (`native::settings_from_env`), in the browser from the page.
pub struct Settings {
    start_ms: Option<f64>,
    choice: Choice,
    sources: Box<dyn Sources>,
}

impl Settings {
    /// `choice`'s vehicles from `sources`, the clock from `start` (Munich
    /// time: `07:45` today or `2026-11-10T07:45`; the real clock if `None`
    /// or unreadable, and always for Helsinki's live feed).
    pub fn new(choice: Choice, sources: Box<dyn Sources>, start: Option<&str>) -> Self {
        let today = Day::in_munich(real_now_ms());
        let start_ms = start
            .filter(|text| !text.trim().is_empty())
            .and_then(|text| {
                let parsed = zone::parse_munich(text, today);
                if parsed.is_none() {
                    err!("TRANSIT_TIME={text}: not 07:45 or 2026-11-10T07:45; the clock is used");
                }
                parsed
            });
        Self {
            start_ms,
            choice,
            sources,
        }
    }

    #[cfg_attr(not(feature = "test-support"), allow(dead_code))]
    pub fn choice(&self) -> Choice {
        self.choice
    }

    /// The same, starting at `start_ms` (Unix ms): the snapshot's fixed
    /// time.
    #[cfg_attr(not(feature = "test-support"), allow(dead_code))]
    pub fn with_start(mut self, start_ms: f64) -> Self {
        self.start_ms = Some(start_ms);
        self
    }

    #[cfg_attr(not(feature = "test-support"), allow(dead_code))]
    pub fn start_ms(&self) -> Option<f64> {
        self.start_ms
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
    crate::trading::theme::install::<TransitTheme>(cx);
    TransitTheme::Paper.apply(cx);
    cx.bind_keys(key_bindings());
}

/// The transit view in `window`, the keyboard in its table, its producer
/// started.
pub fn open(settings: Settings, window: &mut Window, cx: &mut App) -> Entity<Transit> {
    let transit = cx.new(|cx| Transit::new(settings, window, cx));
    let focus = transit.read(cx).focus_handle(cx);
    window.focus(&focus, cx);
    transit
}

/// The grid's counters, refreshed every second.
#[derive(Default)]
struct Status {
    rebuild: Duration,
    frame: Duration,
}

/// What runs for one choice: its clock, its vehicles' and its board's
/// grids, the producer, and why it has nothing to show, if it has not.
struct Run {
    control: Arc<Control>,
    grid: Entity<LiveGridState<Vehicles>>,
    board: Entity<LiveGridState<Board>>,
    error: Option<SharedString>,
    tasks: Vec<Task<()>>,
    subscriptions: Vec<Subscription>,
}

pub struct Transit {
    sources: Box<dyn Sources>,
    choice: Choice,
    control: Arc<Control>,
    grid: Entity<LiveGridState<Vehicles>>,
    search: Entity<InputState>,
    map: Entity<TransitMap>,
    /// The station board: its grid, its search box, the stations that
    /// match what is typed there, and the station's name once chosen.
    board: Entity<LiveGridState<Board>>,
    stations: Entity<InputState>,
    suggestions: Vec<(u32, SharedString)>,
    station_name: Option<SharedString>,
    arrangement: Arrangement,
    frame: Arc<TransitFrame>,
    status: Status,
    /// Why there is nothing to show, if there is not.
    error: Option<SharedString>,
    /// The producer's tasks and the grids' subscriptions, for the choice.
    _run_tasks: Vec<Task<()>>,
    _run_subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Transit {
    fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Settings {
            start_ms,
            choice,
            mut sources,
        } = settings;
        let arrangement = Arrangement::default();
        let source = sources
            .source(choice)
            .unwrap_or_else(|| Err("no source for this choice".into()));
        let run = Self::run(choice, source, start_ms, arrangement, "".into(), cx);
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search line, destination or station")
        });
        let map = cx.new(|_| TransitMap::new());
        let stations = cx.new(|cx| InputState::new(window, cx).placeholder("Find a station"));
        let subscriptions = vec![
            cx.subscribe_in(
                &map,
                window,
                |transit, _, event: &MapEvent, window, cx| match event {
                    MapEvent::Select(key) => transit.select(*key, cx),
                    MapEvent::Station(station) => transit.set_station(Some(*station), window, cx),
                },
            ),
            cx.subscribe_in(
                &stations,
                window,
                |transit, stations, event, window, cx| match event {
                    InputEvent::Change => {
                        let text = stations.read(cx).value();
                        transit.suggest_stations(&text, window, cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        if let Some((station, _)) = transit.suggestions.first().cloned() {
                            transit.set_station(Some(station), window, cx);
                        }
                    }
                    _ => {}
                },
            ),
            cx.subscribe_in(&search, window, |transit, search, event, window, cx| {
                match event {
                    InputEvent::Change => {
                        let text = search.read(cx).value();
                        transit
                            .grid
                            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
                    }
                    // Back to the table, where the letter keys work.
                    InputEvent::PressEnter { .. } => {
                        let focus = transit.grid.focus_handle(cx);
                        window.focus(&focus, cx);
                    }
                    _ => {}
                }
            }),
        ];
        Self {
            sources,
            choice,
            control: run.control,
            grid: run.grid,
            search,
            map,
            board: run.board,
            stations,
            suggestions: Vec::new(),
            station_name: None,
            arrangement,
            frame: Arc::default(),
            status: Status::default(),
            error: run.error,
            _run_tasks: run.tasks,
            _run_subscriptions: run.subscriptions,
            _tasks: vec![Self::poll(cx)],
            _subscriptions: subscriptions,
        }
    }

    /// What runs for `choice` from `source`: a clock from `start_ms`, the
    /// vehicles' grid (arranged as `arrangement`, filtered by `filter`)
    /// and the board's, and the producer writing them.
    fn run(
        choice: Choice,
        source: Result<Box<dyn VehicleSource>, String>,
        start_ms: Option<f64>,
        arrangement: Arrangement,
        filter: SharedString,
        cx: &mut Context<Self>,
    ) -> Run {
        let city = choice.city();
        let control = Control::new(start_ms);
        let store = GridStore::new(table::store_options(), cx);
        let grid = cx.new(|cx| {
            let mut grid = LiveGridState::new(table::schema(city), Vehicles, store.clone(), cx)
                .with_view_id("vehicles")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(table::departure_changes(cx))
                .with_quick_filter_columns([
                    TransitColumn::Line,
                    TransitColumn::Mode,
                    TransitColumn::Destination,
                    TransitColumn::Leg,
                ])
                .with_view(table::view(arrangement, table::default_sort(city), city));
            grid.set_column_visible(TransitColumn::Mode, arrangement.shows_mode(), cx);
            grid.set_quick_filter(filter, cx);
            grid
        });
        let board_store = GridStore::new(board::store_options(), cx);
        let board = cx.new(|cx| {
            LiveGridState::new(board::schema(), Board, board_store.clone(), cx)
                .with_view_id("departures")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(table::departure_changes(cx))
                .with_view(board::view())
        });
        let subscriptions = vec![
            cx.subscribe(&grid, Self::on_grid_event),
            cx.subscribe(&board, Self::on_board_event),
        ];
        let (tasks, error) = match source {
            Ok(source) => {
                let producer = Producer::new(source, store.writer(), control.clone())
                    .with_board(board_store.writer());
                (vec![producer::start(producer, cx)], None)
            }
            Err(error) => {
                err!("transit: no source ({error})");
                (Vec::new(), Some(error.into()))
            }
        };
        Run {
            control,
            grid,
            board,
            error,
            tasks,
            subscriptions,
        }
    }

    /// Switches to `choice`: a new source, clock and tables, the search
    /// kept. In the browser the page starts over with it instead.
    pub fn set_choice(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if choice == self.choice {
            return;
        }
        let Some(source) = self.sources.source(choice) else {
            return;
        };
        self.choice = choice;
        let filter = self.search.read(cx).value();
        let run = Self::run(choice, source, None, self.arrangement, filter, cx);
        // The old grids go, and with their stores the old producer.
        self.control = run.control;
        self.grid = run.grid;
        self.board = run.board;
        self.error = run.error;
        self._run_tasks = run.tasks;
        self._run_subscriptions = run.subscriptions;
        self.frame = Arc::default();
        let frame = self.frame.clone();
        self.map.update(cx, |map, cx| map.set_frame(frame, cx));
        self.suggestions.clear();
        self.station_name = None;
        self.stations
            .update(cx, |input, cx| input.set_value("", window, cx));
        let focus = self.grid.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    #[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
    pub fn choice(&self) -> Choice {
        self.choice
    }

    /// Is the source live, its clock the real one?
    fn is_live(&self) -> bool {
        self.choice.city() == City::Helsinki
            || self
                .frame
                .info
                .as_ref()
                .is_some_and(|info| info.clock == SourceClock::LiveOnly)
    }

    /// Four times a second: a new frame from the producer for the map and
    /// the clock; the counters once a second.
    fn poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut seen = 0;
            let mut ticks = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                ticks += 1;
                let Ok(()) = this.update(cx, |transit, cx| {
                    let version = transit.control.version();
                    if version != seen {
                        seen = version;
                        transit.frame = transit.control.frame();
                        if let Some(error) = transit.control.error() {
                            transit.error = Some(error.into());
                        }
                        let frame = transit.frame.clone();
                        transit.map.update(cx, |map, cx| map.set_frame(frame, cx));
                    }
                    if ticks % 4 == 0 {
                        let stats = transit.grid.read(cx).stats();
                        transit.status = Status {
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

    /// Selects the vehicle `key` in the table and on the map, and scrolls
    /// the table to it.
    pub fn select(&mut self, key: VehicleKey, cx: &mut Context<Self>) {
        self.control.select(Some(key));
        self.grid.update(cx, |grid, cx| {
            grid.select_key(Some(key), cx);
            grid.scroll_to_key(&key, cx);
        });
        cx.notify();
    }

    /// Shows the board of `station`, or none; its name goes to the board's
    /// search box.
    pub fn set_station(
        &mut self,
        station: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.control.set_station(station);
        self.suggestions.clear();
        self.station_name = station.and_then(|station| {
            let info = self.frame.info.as_ref()?;
            info.network
                .names
                .get(station as usize)
                .map(|name| SharedString::from(name.clone()))
        });
        let text = self.station_name.clone().unwrap_or_default();
        self.stations
            .update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    /// The stations whose names match `text`: those that start with it
    /// first, each name once.
    fn suggest_stations(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() {
            self.suggestions.clear();
            if self.control.station().is_some() {
                self.set_station(None, window, cx);
            }
            cx.notify();
            return;
        }
        if self.station_name.as_deref() == Some(text) {
            self.suggestions.clear();
            cx.notify();
            return;
        }
        let Some(info) = self.frame.info.clone().filter(|info| info.has_board) else {
            return;
        };
        let wanted = text.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut found: Vec<(bool, u32, SharedString)> = info
            .network
            .names
            .iter()
            .enumerate()
            .filter_map(|(ix, name)| {
                let lower = name.to_lowercase();
                lower
                    .contains(&wanted)
                    .then(|| (!lower.starts_with(&wanted), ix as u32, name))
            })
            .filter(|(_, _, name)| seen.insert(Arc::clone(name)))
            .map(|(later, ix, name)| (later, ix, SharedString::from(name.clone())))
            .collect();
        found.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.2.len().cmp(&b.2.len()))
                .then(a.2.cmp(&b.2))
        });
        self.suggestions = found
            .into_iter()
            .take(SUGGESTIONS)
            .map(|(_, ix, name)| (ix, name))
            .collect();
        cx.notify();
    }

    /// Shows the vehicles as `arrangement`.
    pub fn set_arrangement(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        if self.arrangement == arrangement {
            return;
        }
        self.arrangement = arrangement;
        let city = self.choice.city();
        self.grid.update(cx, |grid, cx| {
            let sort = grid.sort().to_vec();
            grid.set_view(table::view(arrangement, sort, city), cx);
            grid.set_column_visible(TransitColumn::Mode, arrangement.shows_mode(), cx);
        });
        cx.notify();
    }

    /// Runs time `warp` times as fast; a live clock runs in real time only.
    pub fn set_warp(&mut self, warp: Warp, cx: &mut Context<Self>) {
        if self.is_live() {
            return;
        }
        self.control.set_warp(warp);
        cx.notify();
    }

    /// Shows the city or the region on the map.
    pub fn set_zoom(&mut self, zoom: MapZoom, cx: &mut Context<Self>) {
        self.map.update(cx, |map, cx| map.set_zoom(zoom, cx));
        cx.notify();
    }

    /// A selected vehicle goes to the map.
    fn on_grid_event(
        &mut self,
        _: Entity<LiveGridState<Vehicles>>,
        event: &LiveGridEvent<VehicleKey, TransitColumn>,
        cx: &mut Context<Self>,
    ) {
        if let LiveGridEvent::SelectionChange { selection, .. } = event {
            self.control.select(selection.iter().next().copied());
            cx.notify();
        }
    }

    /// A departure selected on the board selects its vehicle, if it is
    /// under way.
    fn on_board_event(
        &mut self,
        _: Entity<LiveGridState<Board>>,
        event: &LiveGridEvent<VehicleKey, board::BoardColumn>,
        cx: &mut Context<Self>,
    ) {
        if let LiveGridEvent::SelectionChange { selection, .. } = event
            && let Some(key) = selection.iter().next().copied()
            && self.grid.read(cx).snapshot().get(&key).is_some()
        {
            self.select(key, cx);
        }
    }

    fn on_cycle_warp(&mut self, _: &CycleWarp, _: &mut Window, cx: &mut Context<Self>) {
        let warp = self.control.warp().next();
        self.set_warp(warp, cx);
    }

    fn on_cycle_grouping(&mut self, _: &CycleGrouping, _: &mut Window, cx: &mut Context<Self>) {
        self.set_arrangement(self.arrangement.next(), cx);
    }

    fn on_toggle_zoom(&mut self, _: &ToggleZoom, _: &mut Window, cx: &mut Context<Self>) {
        let zoom = self.map.read(cx).zoom().toggled();
        self.set_zoom(zoom, cx);
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_cycle_city(&mut self, _: &CycleCity, window: &mut Window, cx: &mut Context<Self>) {
        let next = City::ALL[(self.choice.city().ix() + 1) % City::ALL.len()];
        self.set_choice(Choice::new(next), window, cx);
    }

    fn on_toggle_more_lines(
        &mut self,
        _: &ToggleMoreLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choice = self.choice.with_more_lines(!self.choice.has_more_lines());
        self.set_choice(choice, window, cx);
    }

    /// The mark, the city and its option, the clock and its warp (or the
    /// feed), and the keys.
    fn render_app_bar(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let warp = self.control.warp();
        let clock = self.zone().format(self.control.now());
        // A narrow bar has room for the time of day only.
        let clock = match clock.split_once(", ") {
            Some((_, time)) if narrow => time.to_owned(),
            _ => clock,
        };
        let live = self.is_live();
        let replay = self
            .frame
            .feed
            .as_ref()
            .is_some_and(|feed| feed.connection.starts_with("replay"));
        let subtitle = match self.choice.city() {
            City::Munich => "Munich by the timetable",
            // The recording's tag stands beside the clock; the title says
            // it too, so a replay never reads as live.
            City::Helsinki if replay => "Helsinki from a recording",
            City::Helsinki => "Helsinki live",
        };
        let choice = self.choice;
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
            // A narrow bar wraps: the clock on a line of its own.
            .when(narrow, |bar| {
                bar.flex_wrap().min_h(px(44.)).py(px(6.)).gap_y(px(6.))
            })
            .when(!narrow, |bar| bar.h(px(44.)))
            .px(PAGE_PADDING + px(4.))
            .gap_x(px(18.))
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .child(
                        div()
                            .size(px(12.))
                            .rounded_full()
                            .border_2()
                            .border_color(theme.primary),
                    )
                    .child(
                        div()
                            .text_size(px(19.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("transit"),
                    )
                    .when(!narrow, |mark| {
                        mark.child(div().text_color(theme.muted_foreground).child(subtitle))
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(10.))
                    .child(
                        TabBar::new("city")
                            .segmented()
                            .small()
                            .selected_index(choice.city().ix())
                            .children(City::ALL.map(|city| Tab::new().label(city.label())))
                            .on_click(cx.listener(|transit, ix: &usize, window, cx| {
                                if let Some(city) = City::ALL.get(*ix) {
                                    transit.set_choice(Choice::new(*city), window, cx);
                                }
                            })),
                    )
                    .child(
                        Switch::new("more-lines")
                            .small()
                            .checked(choice.has_more_lines())
                            .label(choice.more_lines_label())
                            .on_click(cx.listener(move |transit, checked: &bool, window, cx| {
                                transit.set_choice(choice.with_more_lines(*checked), window, cx);
                            })),
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
                        _ if replay => Tag::secondary().outline().small().child("replay"),
                        _ if live => Tag::success().outline().small().child("live"),
                        // The clock's speed, not the data's: the data is
                        // the timetable either way.
                        Warp::RealTime => Tag::secondary()
                            .outline()
                            .small()
                            .child(format!("time {}", warp.label())),
                        _ => Tag::primary()
                            .small()
                            .child(format!("time {}", warp.label())),
                    }),
            )
            .child(div().flex_1())
            .when(!narrow, |bar| {
                bar.child(
                    h_flex()
                        .flex_none()
                        .gap(px(12.))
                        .text_color(theme.muted_foreground)
                        .children(hints),
                )
            })
    }

    /// The search box, the grouping and the warp.
    fn render_toolbar(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        h_flex()
            .flex_none()
            .w_full()
            .px(ui::PANEL_PADDING)
            .py(px(8.))
            .gap(px(8.))
            .flex_wrap()
            .child(
                Input::new(&self.search)
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .small()
                    .aria_label("Search vehicles")
                    .w(px(if narrow { 220. } else { 280. })),
            )
            .child(
                TabBar::new("arrangement")
                    .segmented()
                    .small()
                    .selected_index(self.arrangement.ix())
                    .children(
                        Arrangement::ALL.map(|arrangement| Tab::new().label(arrangement.label())),
                    )
                    .on_click(cx.listener(|transit, ix: &usize, _, cx| {
                        if let Some(arrangement) = Arrangement::ALL.get(*ix) {
                            transit.set_arrangement(*arrangement, cx);
                        }
                    })),
            )
            .child(div().flex_1())
            // A live feed has no other time than now.
            .when(!self.is_live(), |bar| {
                bar.child(
                    TabBar::new("warp")
                        .segmented()
                        .small()
                        .selected_index(self.control.warp().ix())
                        .children(Warp::ALL.map(|warp| Tab::new().label(warp.label())))
                        .on_click(cx.listener(|transit, ix: &usize, _, cx| {
                            if let Some(warp) = Warp::ALL.get(*ix) {
                                transit.set_warp(*warp, cx);
                            }
                        })),
                )
            })
    }

    /// The table, or how to get the timetable it needs.
    fn render_table(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = &self.frame;
        let shown = self.grid.read(cx).projection().row_count();
        let detail = (frame.sweeps > 0).then(|| {
            let today = frame
                .trips_today
                .map(|trips| format!(" of {} trips today", thousands(u64::from(trips))))
                .unwrap_or_default();
            SharedString::from(format!(
                "{} under way{today} · {} shown",
                thousands(u64::from(frame.under_way)),
                thousands(shown as u64),
            ))
        });
        let body = match &self.error {
            Some(error) if self.choice.city() == City::Helsinki => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .p(px(24.))
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.danger)
                        .child("No Helsinki source"),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(error.clone()),
                )
                .into_any_element(),
            None if frame.sweeps > 0 && frame.under_way == 0 && frame.feed.is_some() => {
                let connection = frame
                    .feed
                    .as_ref()
                    .map(|feed| feed.connection.clone())
                    .unwrap_or_default();
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .p(px(24.))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child("Waiting for HSL's vehicles"),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(connection)),
                    )
                    .into_any_element()
            }
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
                        .child("No timetable"),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(error.clone()),
                )
                .child(div().child(
                    "Download MVV's timetable (about 19 MB) once and compile it, then start again:",
                ))
                .child(
                    ui::figures(cx)
                        .text_color(theme.foreground)
                        .child(FETCH_SCRIPT),
                )
                .into_any_element(),
            None if frame.sweeps > 0 && frame.under_way == 0 => {
                let range = frame
                    .info
                    .as_ref()
                    .map(|info| info.attribution.clone())
                    .unwrap_or_default();
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .p(px(24.))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child("Nothing under way at this time"),
                    )
                    .child(div().text_color(theme.muted_foreground).child(
                        "Night service is thin, and the timetable covers a few months only. Try another time with ×60.",
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(range)),
                    )
                    .into_any_element()
            }
            None => ui::grid_frame(cx)
                .key_context(TABLE_CONTEXT)
                .child(
                    LiveGrid::new(&self.grid)
                        .with_size(Size::Size(ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Vehicles under way"),
                )
                .into_any_element(),
        };
        ui::panel(cx)
            .size_full()
            .child(ui::panel_header("Under way", detail, cx))
            .child(self.render_toolbar(narrow, cx))
            .child(body)
    }

    /// The map with its zoom, its legend and the data's credit.
    fn render_map(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = &self.frame;
        let zoom = self.map.read(cx).zoom();
        let detail = (frame.sweeps > 0).then(|| {
            SharedString::from(
                mode_counts(frame)
                    .map(|(mode, count)| format!("{} {count}", mode.label()))
                    .collect::<Vec<_>>()
                    .join(" · "),
            )
        });
        let modes = frame.info.as_ref().map_or_else(Vec::new, |info| {
            Mode::ALL
                .into_iter()
                .filter(|mode| info.network.lines.iter().any(|line| line.mode == *mode))
                .collect()
        });
        let legend = modes.into_iter().map(|mode| {
            h_flex()
                .gap(px(5.))
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(mode_legend_color(mode, frame, cx)),
                )
                .child(mode.label())
        });
        let selected = frame.selected.as_ref().map(|selected| {
            let mut next = String::new();
            rows::Countdown::between(frame.sim_ms, selected.next_arrival_ms).write(&mut next);
            let (color, text) = line_colors(&selected.line, theme);
            h_flex()
                .gap(px(8.))
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .px(px(5.))
                        .rounded(px(3.))
                        .bg(color)
                        .text_color(text)
                        .font_weight(FontWeight::BOLD)
                        .child(SharedString::from(selected.line.name.clone())),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(SharedString::from(match selected.live {
                            Some(live) => format!(
                                "to {} · next stop {}{}",
                                selected.headsign,
                                selected.leg,
                                live.delay_s.map_or_else(String::new, |delay| match delay {
                                    0 => " · on time".into(),
                                    late if late > 0 => format!(" · {late} s late"),
                                    early => format!(" · {} s early", -early),
                                })
                            ),
                            None => format!(
                                "to {} · {} · next in {next}",
                                selected.headsign, selected.leg
                            ),
                        })),
                )
        });
        let credit = frame
            .info
            .as_ref()
            .map(|info| SharedString::from(info.credit.clone()));
        ui::panel(cx)
            .size_full()
            .child(
                ui::panel_header("Map", detail, cx).child(
                    TabBar::new("zoom")
                        .segmented()
                        .small()
                        .selected_index(zoom.ix())
                        .children(MapZoom::ALL.map(|zoom| Tab::new().label(zoom.label())))
                        .on_click(cx.listener(|transit, ix: &usize, _, cx| {
                            if let Some(zoom) = MapZoom::ALL.get(*ix) {
                                transit.set_zoom(*zoom, cx);
                            }
                        })),
                ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    self.map
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                ),
            )
            .child(
                v_flex()
                    .flex_none()
                    .px(ui::PANEL_PADDING)
                    .py(px(6.))
                    .gap(px(4.))
                    .border_t_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child(
                        h_flex()
                            .gap(px(14.))
                            .text_color(theme.muted_foreground)
                            .children(legend)
                            .child(div().flex_1())
                            .child(if frame.info.as_ref().is_some_and(|info| info.has_board) {
                                "Click a vehicle or a station"
                            } else {
                                "Click a vehicle to follow it"
                            }),
                    )
                    .children(selected)
                    .when_some(credit, |footer, credit| {
                        footer.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(credit),
                        )
                    }),
            )
    }

    /// The board of the chosen station by the timetable, with its search
    /// box and the names that match what is typed there.
    fn render_board(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = &self.frame;
        let station = self.control.station();
        let detail = self.station_name.clone().map(|name| {
            SharedString::from(format!(
                "{name} · {} in the next hour · by the timetable",
                frame.board
            ))
        });
        let suggestions = self.suggestions.iter().map(|(station, name)| {
            let station = *station;
            div()
                .px(px(8.))
                .py(px(3.))
                .rounded(px(3.))
                .cursor_default()
                .hover(|style| style.bg(theme.list_hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |transit, _, window, cx| {
                        transit.set_station(Some(station), window, cx)
                    }),
                )
                .child(name.clone())
        });
        let body = match station {
            Some(_) => ui::grid_frame(cx)
                .child(
                    LiveGrid::new(&self.board)
                        .with_size(Size::Size(ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Scheduled departures"),
                )
                .into_any_element(),
            None => div()
                .px(ui::PANEL_PADDING)
                .pb(px(10.))
                .text_color(theme.muted_foreground)
                .child("Search a station, or click one on the map: its next departures by the timetable.")
                .into_any_element(),
        };
        ui::panel(cx)
            .size_full()
            .child(ui::panel_header("Scheduled departures", detail, cx))
            .child(
                h_flex().flex_none().px(ui::PANEL_PADDING).py(px(8.)).child(
                    Input::new(&self.stations)
                        .prefix(Icon::new(IconName::Search).small())
                        .cleanable(true)
                        .small()
                        .aria_label("Find a station")
                        .w(px(260.)),
                ),
            )
            .when(!self.suggestions.is_empty(), |panel| {
                panel.child(
                    v_flex()
                        .flex_none()
                        .mx(ui::PANEL_PADDING)
                        .mb(px(8.))
                        .p(px(2.))
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .children(suggestions),
                )
            })
            .child(body)
    }

    /// Where the clock shows the time: the source's city.
    fn zone(&self) -> Zone {
        self.frame
            .info
            .as_ref()
            .map_or(Zone::Munich, |info| info.zone)
    }

    /// Does the source have boards, and so the board its panel?
    fn has_board(&self) -> bool {
        self.frame.info.as_ref().is_some_and(|info| info.has_board)
    }

    /// The counts, the warp, the timings and the attribution; on a narrow
    /// window the vehicles, the warp and a short attribution (the map's
    /// footer carries the full credit).
    fn render_status(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = &self.frame;
        let attribution: SharedString = match (&frame.info, self.choice.city()) {
            (Some(info), _) if narrow => info.short.clone().into(),
            (Some(info), _) => info.attribution.clone().into(),
            (None, City::Munich) => "Timetable data: MVV (CC BY). Scheduled, not real-time.".into(),
            (None, City::Helsinki) => "Vehicle positions © HSL, CC BY 4.0. Live.".into(),
        };
        let under_way = SharedString::from(format!(
            "{} under way",
            thousands(u64::from(frame.under_way))
        ));
        // A live feed's connection and rate where the clock cannot warp.
        let warp = SharedString::from(match &frame.feed {
            Some(feed) => format!("{} · {:.0} msg/s", feed.connection, feed.rate),
            None => format!("time {}", self.control.warp().label()),
        });
        if narrow {
            return StatusBar::new()
                .font_features(ui::tabular_figures())
                .left(under_way)
                .left(warp)
                .right(div().text_color(theme.muted_foreground).child(attribution));
        }
        StatusBar::new()
            .font_features(ui::tabular_figures())
            .left(under_way)
            .when_some(frame.trips_today, |bar, trips| {
                bar.left(SharedString::from(format!(
                    "{} trips today",
                    thousands(u64::from(trips))
                )))
            })
            .left(warp)
            .left(SharedString::from(format!(
                "sweep {:.1} ms · write {:.1} ms · rebuild {:.1} ms · frame {:.2} ms",
                ms(frame.compute),
                ms(frame.write),
                ms(self.status.rebuild),
                ms(self.status.frame),
            )))
            .left(SharedString::from(format!(
                "{} started · {} departed · {} ended",
                frame.started, frame.departures, frame.finished
            )))
            .right(div().text_color(theme.muted_foreground).child(attribution))
    }
}

/// A mode's colour in the legend: a line's own colour from the data where
/// the mode's lines have one (the S-Bahn's green), else the theme's.
fn mode_legend_color(mode: Mode, frame: &TransitFrame, cx: &App) -> gpui_kit::Hsla {
    let theme = cx.theme();
    frame
        .info
        .as_ref()
        .and_then(|info| {
            info.network
                .lines
                .iter()
                .find(|line| line.mode == mode && line.color.is_some())
        })
        .map_or_else(
            || mode_color(mode, theme),
            |line| line_colors(line, theme).0,
        )
}

/// What the snapshot and the browser page drive and read the example
/// with: natively the snapshot calls part of it, with `test-support` only.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
impl Transit {
    pub fn grid(&self) -> &Entity<LiveGridState<Vehicles>> {
        &self.grid
    }

    pub fn control(&self) -> &Arc<Control> {
        &self.control
    }

    pub fn arrangement(&self) -> Arrangement {
        self.arrangement
    }

    pub fn zoom(&self, cx: &App) -> MapZoom {
        self.map.read(cx).zoom()
    }

    /// Sorts the table by `column`.
    pub fn set_sort(
        &mut self,
        column: TransitColumn,
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

    /// The vehicle of `mode` in the table with the most stations ahead, if
    /// one is under way: the longest run to follow.
    pub fn longest_run(&self, mode: Mode, cx: &App) -> Option<VehicleKey> {
        self.grid
            .read(cx)
            .snapshot()
            .iter()
            .map(|(_, row)| row)
            .filter(|row| row.line.mode == mode)
            .max_by_key(|row| (row.stops_left, std::cmp::Reverse(row.key)))
            .map(|row| row.key)
    }

    /// The station named `name`, if the source has boards.
    pub fn find_station(&self, name: &str) -> Option<u32> {
        let info = self.frame.info.as_ref().filter(|info| info.has_board)?;
        let ix = info
            .network
            .names
            .iter()
            .position(|station| station.as_ref() == name)?;
        Some(ix as u32)
    }

    pub fn board(&self) -> &Entity<LiveGridState<Board>> {
        &self.board
    }

    /// The board as one line, for the snapshot and the page: the station
    /// and its first departures.
    pub fn board_line(&self, cx: &App) -> String {
        let board = self.board.read(cx);
        let mut rows: Vec<_> = board
            .snapshot()
            .iter()
            .map(|(_, row)| row.clone())
            .collect();
        rows.sort_by_key(|row| row.departs);
        let first: Vec<String> = rows
            .iter()
            .take(4)
            .map(|row| {
                let mut departs = String::new();
                row.departs.write(&mut departs);
                format!(
                    "{} {} pl. {} in {departs}",
                    row.line.name,
                    row.headsign,
                    if row.platform.is_empty() {
                        "-"
                    } else {
                        &row.platform
                    }
                )
            })
            .collect();
        format!(
            "{} · {} departures · {}",
            self.station_name.as_deref().unwrap_or("no station"),
            rows.len(),
            first.join(" · ")
        )
    }

    /// The latest live vehicle in the table, if one reports its delay.
    pub fn latest(&self, cx: &App) -> Option<VehicleKey> {
        self.grid
            .read(cx)
            .snapshot()
            .iter()
            .filter_map(|(_, row)| Some((row.live?.delay_s?, row.key)))
            .max()
            .map(|(_, key)| key)
    }

    /// The row of `key`, if it is in the table.
    pub fn row(&self, key: VehicleKey, cx: &App) -> Option<Arc<VehicleRow>> {
        self.grid.read(cx).snapshot().get(&key).cloned()
    }

    /// The status as one line, for the snapshot and the page.
    pub fn status_line(&self, cx: &App) -> String {
        let frame = &self.frame;
        let shown = self.grid.read(cx).projection().row_count();
        format!(
            "{} · {} under way ({}) · {} trips today · {} shown · {} rows/s · sweep {:.2} ms · write {:.2} ms · rebuild {:.2} ms · frame {:.2} ms · load {:.0} ms · time {}",
            self.zone().format(frame.sim_ms),
            frame.under_way,
            mode_counts(frame)
                .map(|(mode, count)| format!("{} {count}", mode.label()))
                .collect::<Vec<_>>()
                .join(", "),
            frame.trips_today.unwrap_or(0),
            shown,
            frame.rate,
            ms(frame.compute),
            ms(frame.write),
            ms(self.status.rebuild),
            ms(self.status.frame),
            ms(frame.load),
            self.control.warp().label(),
        ) + &frame.feed.as_ref().map_or_else(String::new, |feed| {
            format!(
                " · feed {} · {:.1} msg/s · {} messages{}",
                feed.connection,
                feed.rate,
                feed.messages,
                feed.notice
                    .as_ref()
                    .map_or_else(String::new, |notice| format!(" · {notice}"))
            )
        })
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

impl Focusable for Transit {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for Transit {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.viewport_size().width;
        let narrow = width < NARROW;
        // The map takes a third of a wide window.
        let side = (width * 0.33).clamp(px(400.), px(560.));
        let theme = cx.theme();
        let (background, foreground, family, text_size) = (
            theme.background,
            theme.foreground,
            theme.font_family.clone(),
            theme.font_size,
        );
        let has_board = self.has_board();
        let board_open = self.control.station().is_some() || !self.suggestions.is_empty();
        let main = if narrow {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .p(PAGE_PADDING)
                .gap(GAP)
                .child(
                    div()
                        .flex_none()
                        .h(px(if has_board && board_open { 180. } else { 260. }))
                        .child(self.render_map(cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(self.render_table(narrow, cx)),
                )
                .when(has_board, |main| {
                    main.child(
                        div()
                            .flex_none()
                            .h(px(if board_open { 240. } else { 120. }))
                            .child(self.render_board(cx)),
                    )
                })
        } else {
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
                        .child(self.render_table(narrow, cx)),
                )
                .child(
                    v_flex()
                        .flex_none()
                        .w(side)
                        .h_full()
                        .gap(GAP)
                        .child(div().flex_1().min_h_0().child(self.render_map(cx)))
                        .when(has_board, |side| {
                            side.child(
                                div()
                                    .when(board_open, |board| board.flex_1().min_h_0())
                                    .when(!board_open, |board| board.flex_none().h(px(120.)))
                                    .child(self.render_board(cx)),
                            )
                        }),
                )
        };
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_cycle_warp))
            .on_action(cx.listener(Self::on_cycle_grouping))
            .on_action(cx.listener(Self::on_toggle_zoom))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_cycle_city))
            .on_action(cx.listener(Self::on_toggle_more_lines))
            .bg(background)
            .text_color(foreground)
            .font_family(family)
            .text_size(text_size)
            .child(self.render_app_bar(narrow, cx))
            .child(main)
            .child(self.render_status(narrow, cx))
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

    /// No sources: every choice reloads.
    struct Nothing;

    impl Sources for Nothing {
        fn source(&mut self, _: Choice) -> Option<Result<Box<dyn VehicleSource>, String>> {
            None
        }
    }

    fn settings(start: &str) -> Settings {
        Settings::new(Choice::default(), Box::new(Nothing), Some(start))
    }

    #[test]
    fn test_settings_fall_back_to_the_clock() {
        assert_eq!(settings("soon").start_ms, None);
        assert_eq!(
            settings("2026-11-10T07:45").start_ms,
            zone::parse_munich("2026-11-10T07:45", Day::from_ymd(2026, 11, 10))
        );
        assert_eq!(settings(" ").start_ms, None);
    }

    #[test]
    fn test_choices_name_their_query_and_option() {
        assert_eq!(Choice::default().query(), "");
        assert_eq!(
            Choice::new(City::Munich).with_regional(true).query(),
            "?regional"
        );
        let helsinki = Choice::new(City::Helsinki).with_more_lines(true);
        assert!(helsinki.has_buses() && !helsinki.is_regional());
        assert_eq!(helsinki.query(), "?city=helsinki&buses");
        assert_eq!(helsinki.more_lines_label(), "Buses");
        assert!(!Choice::new(City::Munich).has_more_lines());
    }
}
