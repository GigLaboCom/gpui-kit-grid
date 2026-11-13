//! The wiki example itself: the wikis table, the pulse, the selected wiki
//! and the status bar, over the pump. The native `main` (`main.rs`) opens
//! it in a window; the browser build (`crates/grid-web/apps/wiki`, the page
//! `wiki/`) in the page's canvas. Both call `setup` once, then `open`, and
//! hand over what differs between them in [`Settings`]: the transport of
//! the live stream ([`StreamTransport`]), a recording, whether the page is
//! watched ([`Presence`]) and which titles can be drawn.
//!
//! Live, the stream opens only when asked (Start, or natively the command
//! line), one connection at a time; a pause closes it, and so do a tab
//! hidden for a minute (it opens again when the tab is shown) and five
//! minutes without interaction (it waits for Resume) where a [`Presence`]
//! says so (`attendance.rs`).

use std::{any::Any, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Size,
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
    Clock, GridStore, LiveGrid, LiveGridEvent, LiveGridState, PinnedEdge, SelectionMode,
    SortDirection,
};
use web_time::Instant;

mod attendance;
mod detail;
pub mod edit;
mod languages;
pub mod pulse;
pub mod pump;
pub mod rows;
pub mod source;
pub mod sse;
mod strip;
mod table;
mod theme;
mod ui;

#[cfg(not(target_family = "wasm"))]
pub mod native;
#[cfg(feature = "test-support")]
pub mod snapshot;

pub use self::theme::WikiTheme;
use self::{
    attendance::{Attendance, Stream, Verdict},
    edit::TitleGlyphs,
    pump::{Control, PulseFrame, Pump, STEP},
    rows::WikiColumn,
    source::{FeedState, Inbox, LiveEdits, Recording, Replay},
    table::{Arrangement, WikiTable},
    ui::{GAP, PAGE_PADDING, ROW_HEIGHT, thousands},
};
use crate::trading::{theme::TradingTheme as _, worker};

gpui_kit::actions!(
    wiki,
    [
        /// Starts the live stream, pauses it, or resumes it.
        ToggleLive,
        /// Plays the recording, or stops it.
        ToggleReplay,
        /// Groups the table by project family, or not.
        ToggleGrouping,
        /// Opens the selected wiki's last page.
        OpenPage,
        /// Moves the focus to the search box.
        FocusSearch,
    ]
);

/// The key context of the window content.
pub const KEY_CONTEXT: &str = "Wiki";
/// Around the table: its letter keys work there, never in the search box.
const TABLE_CONTEXT: &str = "WikiTable";
const TABLE_KEYS: &str = "WikiTable > LiveGrid";

/// The keys, as data: the bindings and the app bar's hints are both made
/// from this table.
const KEYS: [(&str, Command); 5] = [
    ("s", Command::Live),
    ("r", Command::Replay),
    ("g", Command::Grouping),
    ("o", Command::Open),
    ("secondary-f", Command::Search),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Live,
    Replay,
    Grouping,
    Open,
    Search,
}

impl Command {
    fn binding(self, keystrokes: &str) -> KeyBinding {
        let table = Some(TABLE_KEYS);
        match self {
            Self::Live => KeyBinding::new(keystrokes, ToggleLive, table),
            Self::Replay => KeyBinding::new(keystrokes, ToggleReplay, table),
            Self::Grouping => KeyBinding::new(keystrokes, ToggleGrouping, table),
            Self::Open => KeyBinding::new(keystrokes, OpenPage, table),
            Self::Search => KeyBinding::new(keystrokes, FocusSearch, Some(KEY_CONTEXT)),
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::Replay => "Replay",
            Self::Grouping => "Group",
            Self::Open => "Open page",
            Self::Search => "Search",
        }
    }
}

fn key_bindings() -> Vec<KeyBinding> {
    KEYS.iter()
        .map(|(keystrokes, command)| command.binding(keystrokes))
        .collect()
}

/// The attribution the status bar and the page show: the source, and the
/// licence of the titles, which are the wikis' content.
pub const ATTRIBUTION: &str =
    "Data: Wikimedia EventStreams · content CC BY-SA 4.0, Wikimedia contributors";
/// The view's clock for the pulse and the status: ten times a second.
const POLL: Duration = Duration::from_millis(100);
/// Below this width the pulse goes above the table and the detail away.
const NARROW: f32 = 980.;
/// The side panels' width: a share of the window, within bounds.
const SIDE_SHARE: f32 = 0.3;
const SIDE_MIN: f32 = 360.;
const SIDE_MAX: f32 = 470.;
/// The attribution where the status bar has little room.
const SHORT_ATTRIBUTION: &str = "Wikimedia EventStreams · CC BY-SA 4.0";

/// Opens the live stream: natively an HTTP client on a thread of its own
/// (`native.rs`), in the browser an `EventSource`. One connection, kept
/// open and reconnected as needed, until the returned handle is dropped.
pub trait StreamTransport: 'static {
    fn open(&self, inbox: Arc<Inbox>, cx: &mut App) -> Box<dyn Any>;
}

/// Whether someone is watching: in the browser, the page's visibility and
/// a count of presses, keys and scrolls. Without one the stream stays
/// open until paused. What the view does with it: [`Attendance`].
pub trait Presence: 'static {
    fn is_hidden(&self) -> bool;
    /// Rises with every interaction.
    fn interactions(&self) -> u64;
}

/// What the example does at start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Start {
    /// Waits for Start (the browser: never on page load).
    #[default]
    Idle,
    Live,
    Replay,
}

/// What the example starts with: natively from the environment, in the
/// browser from the page.
#[derive(Default)]
pub struct Settings {
    start: Start,
    transport: Option<Rc<dyn StreamTransport>>,
    recording: Option<Arc<Recording>>,
    presence: Option<Rc<dyn Presence>>,
    glyphs: TitleGlyphs,
    speed: u32,
}

impl Settings {
    pub fn new(start: Start) -> Self {
        Self {
            start,
            speed: 1,
            ..Self::default()
        }
    }

    pub fn with_transport(mut self, transport: impl StreamTransport) -> Self {
        self.transport = Some(Rc::new(transport));
        self
    }

    pub fn with_recording(mut self, recording: Option<Arc<Recording>>) -> Self {
        self.recording = recording.filter(|recording| !recording.is_empty());
        self
    }

    #[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
    pub fn with_presence(mut self, presence: impl Presence) -> Self {
        self.presence = Some(Rc::new(presence));
        self
    }

    #[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
    pub fn with_glyphs(mut self, glyphs: TitleGlyphs) -> Self {
        self.glyphs = glyphs;
        self
    }

    /// The replay's speed, ×1 to ×16.
    pub fn with_speed(mut self, speed: u32) -> Self {
        self.speed = speed.clamp(1, 16);
        self
    }
}

/// gpui-kit, the grid's key bindings, then `configure`. Call once.
pub fn setup(cx: &mut App) {
    gpui_kit::init(cx);
    gpui_kit_grid::init(cx);
    configure(cx);
}

/// After `gpui_kit::init` and `gpui_kit_grid::init`: the theme with its
/// fonts, and the keys.
pub fn configure(cx: &mut App) {
    crate::trading::theme::install::<WikiTheme>(cx);
    WikiTheme::Folio.apply(cx);
    cx.bind_keys(key_bindings());
}

/// The wiki view in `window`, the keyboard in its table.
pub fn open(settings: Settings, window: &mut Window, cx: &mut App) -> Entity<Wiki> {
    let wiki = cx.new(|cx| Wiki::new(settings, window, cx));
    let focus = wiki.read(cx).focus_handle(cx);
    window.focus(&focus, cx);
    wiki
}

/// The characters the faces the text system has can draw, for
/// [`Settings::with_glyphs`]: the theme's families and GPUI's default
/// (the browser draws CJK besides).
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub fn loaded_glyphs(cx: &App) -> TitleGlyphs {
    let theme = cx.theme();
    let families = [
        theme.font_family.clone(),
        theme.mono_font_family.clone(),
        ".SystemUIFont".into(),
    ];
    let text = cx.text_system();
    let fonts: Vec<_> = families
        .iter()
        .map(|family| text.resolve_font(&gpui_kit::font(family.clone())))
        .collect();
    TitleGlyphs::covered(|ch| {
        fonts
            .iter()
            .any(|font| text.advance(*font, px(12.), ch).is_ok())
    })
}

/// Why the live stream is paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseReason {
    User,
    Hidden,
    Idle,
}

/// What the view is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Live,
    Paused(PauseReason),
    /// `fallback`: the live stream could not be reached.
    Replay {
        fallback: bool,
    },
}

impl Mode {
    #[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Live => "live",
            Self::Paused(_) => "paused",
            Self::Replay { .. } => "replay",
        }
    }
}

/// The grid's counters, refreshed every second.
#[derive(Clone, Copy, Default)]
struct Status {
    rebuild: Duration,
    frame: Duration,
}

pub struct Wiki {
    control: Arc<Control>,
    grid: Entity<LiveGridState<WikiTable>>,
    search: Entity<InputState>,
    clock: Arc<dyn Clock>,
    arrangement: Arrangement,
    transport: Option<Rc<dyn StreamTransport>>,
    recording: Option<Arc<Recording>>,
    presence: Option<Rc<dyn Presence>>,
    mode: Mode,
    /// The live connection while live; dropping it closes the stream.
    connection: Option<Box<dyn Any>>,
    /// The live clock's origin, kept across a pause.
    live_origin: Option<Instant>,
    frame: Arc<PulseFrame>,
    frame_seen: Instant,
    status: Status,
    /// The last interaction count and when it changed.
    /// Who watches the live stream.
    attendance: Attendance,
    /// The live stream's inbox, kept across a pause: a resume continues
    /// from its last event while the gap is short.
    inbox: Option<Arc<Inbox>>,
    /// The window is a phone's width: fewer columns.
    narrow: bool,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Wiki {
    fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Settings {
            start,
            transport,
            recording,
            presence,
            glyphs,
            speed,
        } = settings;
        let control = Control::new();
        control
            .speed()
            .store(speed.max(1), std::sync::atomic::Ordering::Relaxed);
        let store = GridStore::new(table::store_options(), cx);
        let clock: Arc<dyn Clock> = Arc::new(cx.background_executor().clone());
        let arrangement = Arrangement::default();
        let grid = cx.new(|cx| {
            let mut grid = LiveGridState::new(table::schema(), WikiTable, store.clone(), cx)
                .with_view_id("wikis")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(table::wiki_changes())
                .with_quick_filter_columns([
                    WikiColumn::Wiki,
                    WikiColumn::Family,
                    WikiColumn::Lang,
                    WikiColumn::Language,
                ])
                .with_view(table::view(arrangement, table::default_sort()))
                .with_totals(PinnedEdge::Bottom);
            grid.set_column_visible(WikiColumn::Family, arrangement.shows_family(), cx);
            grid
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter by wiki, family or language")
        });
        let subscriptions = vec![
            cx.observe_window_bounds(window, |wiki, window, cx| wiki.fit(window, cx)),
            cx.subscribe(&grid, Self::on_grid_event),
            cx.subscribe_in(
                &search,
                window,
                |wiki, search, event, window, cx| match event {
                    InputEvent::Change => {
                        let text = search.read(cx).value();
                        wiki.grid
                            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
                    }
                    InputEvent::PressEnter { .. } => {
                        let focus = wiki.grid.focus_handle(cx);
                        window.focus(&focus, cx);
                    }
                    _ => {}
                },
            ),
        ];
        let mut pump = Pump::new(control.clone(), store.writer(), clock.clone(), glyphs);
        let tasks = vec![
            worker::repeat(STEP, move || pump.step(), cx),
            Self::poll(cx),
        ];
        let now = clock.now();
        let mut wiki = Self {
            control,
            grid,
            search,
            clock,
            arrangement,
            transport,
            recording,
            presence,
            mode: Mode::Idle,
            connection: None,
            live_origin: None,
            frame: Arc::default(),
            frame_seen: now,
            status: Status::default(),
            attendance: Attendance::new(now),
            inbox: None,
            narrow: false,
            _tasks: tasks,
            _subscriptions: subscriptions,
        };
        wiki.fit(window, cx);
        match start {
            Start::Idle => {}
            Start::Live => wiki.start_live(cx),
            Start::Replay => wiki.start_replay(false, cx),
        }
        wiki
    }

    /// Ten times a second: a new frame from the pump, the page's presence,
    /// and the fallback when the stream is unreachable; the grid's counters
    /// once a second.
    fn poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut seen = 0;
            let mut ticks = 0u64;
            loop {
                cx.background_executor().timer(POLL).await;
                ticks += 1;
                let Ok(()) = this.update(cx, |wiki, cx| {
                    let version = wiki.control.version();
                    let fresh = version != seen;
                    if fresh {
                        seen = version;
                        wiki.frame = wiki.control.frame();
                        wiki.frame_seen = wiki.clock.now();
                    }
                    wiki.watch_presence(cx);
                    if wiki.mode == Mode::Live && wiki.frame.feed == Some(FeedState::Unreachable) {
                        wiki.fall_back(cx);
                    }
                    if ticks % 10 == 0 {
                        let stats = wiki.grid.read(cx).stats();
                        wiki.status = Status {
                            rebuild: stats.last_rebuild(),
                            frame: stats.last_prepaint(),
                        };
                    }
                    let moving = matches!(wiki.mode, Mode::Live | Mode::Replay { .. });
                    if fresh || moving || ticks % 10 == 0 {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        })
    }

    /// Leaves the language columns out at a phone's width.
    fn fit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let narrow = f32::from(window.viewport_size().width) < NARROW;
        if narrow == self.narrow {
            return;
        }
        self.narrow = narrow;
        self.grid.update(cx, |grid, cx| {
            for column in table::WIDE_ONLY {
                grid.set_column_visible(column, !narrow, cx);
            }
        });
    }

    /// Pauses a live stream nobody watches, and opens one again that a
    /// hidden tab closed when the tab is shown ([`Attendance`]).
    fn watch_presence(&mut self, cx: &mut Context<Self>) {
        let Some(presence) = self.presence.clone() else {
            return;
        };
        let stream = match self.mode {
            Mode::Live => Stream::Live,
            Mode::Paused(reason) => Stream::Paused(reason),
            Mode::Idle | Mode::Replay { .. } => Stream::Other,
        };
        let verdict = self.attendance.step(
            self.clock.now(),
            presence.is_hidden(),
            presence.interactions(),
            stream,
        );
        match verdict {
            Verdict::Keep => {}
            Verdict::Pause(reason) => self.pause(reason, cx),
            Verdict::Resume => self.start_live(cx),
        }
    }

    /// The stream gave up: the recording instead, if there is one.
    fn fall_back(&mut self, cx: &mut Context<Self>) {
        self.connection = None;
        if self.recording.is_some() {
            self.start_replay(true, cx);
        }
    }

    /// Opens the live stream: a fresh table from idle or a replay, the same
    /// table on from a pause. One connection at a time.
    pub fn start_live(&mut self, cx: &mut Context<Self>) {
        let Some(transport) = self.transport.clone() else {
            return;
        };
        if self.connection.is_some() {
            return;
        }
        let resumed = matches!(self.mode, Mode::Paused(_));
        let now = self.clock.now();
        let origin = match (resumed, self.live_origin) {
            (true, Some(origin)) => origin,
            _ => now,
        };
        self.live_origin = Some(origin);
        self.attendance.started(now);
        // A resume keeps the inbox: the transport continues from its last
        // event (`Last-Event-ID`, `?since=`) while the gap is short.
        let inbox = match (resumed, &self.inbox) {
            (true, Some(inbox)) => inbox.clone(),
            _ => Inbox::new(self.clock.clone()),
        };
        self.inbox = Some(inbox.clone());
        self.connection = Some(transport.open(inbox.clone(), cx));
        self.control
            .set_source(Some(Box::new(LiveEdits::new(inbox, origin))), !resumed);
        self.mode = Mode::Live;
        cx.notify();
    }

    /// Closes the live stream, holding the table as it is.
    pub fn pause(&mut self, reason: PauseReason, cx: &mut Context<Self>) {
        if self.mode != Mode::Live {
            return;
        }
        self.connection = None;
        self.control.set_paused(true);
        self.mode = Mode::Paused(reason);
        cx.notify();
    }

    /// Plays the recording on a fresh table, closing the live stream.
    pub fn start_replay(&mut self, fallback: bool, cx: &mut Context<Self>) {
        let Some(recording) = self.recording.clone() else {
            return;
        };
        self.connection = None;
        self.inbox = None;
        let replay = Replay::new(recording, self.control.speed().clone());
        self.control.set_source(Some(Box::new(replay)), true);
        self.mode = Mode::Replay { fallback };
        cx.notify();
    }

    /// Stops reading: an empty table, waiting for Start.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.connection = None;
        self.inbox = None;
        self.control.set_source(None, true);
        self.mode = Mode::Idle;
        cx.notify();
    }

    /// The recording, handed over after the start (the browser fetches it
    /// in the background).
    #[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
    pub fn set_recording(&mut self, recording: Option<Arc<Recording>>, cx: &mut Context<Self>) {
        self.recording = recording.filter(|recording| !recording.is_empty());
        cx.notify();
    }

    /// The replay's speed, ×1 to ×16.
    pub fn set_speed(&mut self, speed: u32, cx: &mut Context<Self>) {
        self.control
            .speed()
            .store(speed.clamp(1, 16), std::sync::atomic::Ordering::Relaxed);
        cx.notify();
    }

    pub fn speed(&self) -> u32 {
        self.control
            .speed()
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_arrangement(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        if self.arrangement == arrangement {
            return;
        }
        self.arrangement = arrangement;
        self.grid.update(cx, |grid, cx| {
            let sort = grid.sort().to_vec();
            grid.set_view(table::view(arrangement, sort), cx);
            grid.set_column_visible(WikiColumn::Family, arrangement.shows_family(), cx);
        });
        cx.notify();
    }

    fn toggle_live(&mut self, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Live => self.pause(PauseReason::User, cx),
            Mode::Idle | Mode::Paused(_) | Mode::Replay { .. } => self.start_live(cx),
        }
    }

    fn toggle_replay(&mut self, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Replay { .. } => self.stop(cx),
            _ => self.start_replay(false, cx),
        }
    }

    /// Opens the last page of `wiki` (natively in the system's browser, on
    /// the web in a new tab).
    fn open_page(&self, wiki: &Arc<str>, cx: &mut App) {
        let url = self
            .grid
            .read(cx)
            .snapshot()
            .get(wiki)
            .and_then(|row| row.url().cloned());
        if let Some(url) = url {
            cx.open_url(&url);
        }
    }

    fn on_grid_event(
        &mut self,
        _: Entity<LiveGridState<WikiTable>>,
        event: &LiveGridEvent<Arc<str>, WikiColumn>,
        cx: &mut Context<Self>,
    ) {
        match event {
            LiveGridEvent::SelectionChange { selection, .. } => {
                self.control.select(selection.iter().next().cloned());
                cx.notify();
            }
            LiveGridEvent::Confirm { keys, .. } => {
                if let Some(wiki) = keys.first() {
                    self.open_page(wiki, cx);
                }
            }
            _ => {}
        }
    }

    fn on_toggle_live(&mut self, _: &ToggleLive, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_live(cx);
    }

    fn on_toggle_replay(&mut self, _: &ToggleReplay, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_replay(cx);
    }

    fn on_toggle_grouping(&mut self, _: &ToggleGrouping, _: &mut Window, cx: &mut Context<Self>) {
        self.set_arrangement(self.arrangement.toggled(), cx);
    }

    fn on_open_page(&mut self, _: &OpenPage, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(wiki) = self.control.selected() {
            self.open_page(&wiki, cx);
        }
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The source's clock for the pulse now: the frame's, moved on by the
    /// time since it came (at the replay's speed), at most two steps.
    fn strip_now(&self) -> f64 {
        let frame_ms = self.frame.now_ms as f64;
        let speed = match self.mode {
            Mode::Live => 1.,
            Mode::Replay { .. } => f64::from(self.speed()),
            Mode::Idle | Mode::Paused(_) => return frame_ms,
        };
        let since = self
            .clock
            .now()
            .saturating_duration_since(self.frame_seen)
            .min(STEP * 2);
        frame_ms + since.as_secs_f64() * 1e3 * speed
    }

    /// What the stream is doing, in a few words.
    fn feed_text(&self) -> String {
        let frame = &self.frame;
        match self.mode {
            Mode::Idle => "idle".into(),
            Mode::Paused(PauseReason::User) => "paused".into(),
            Mode::Paused(PauseReason::Hidden) => "paused: tab hidden".into(),
            Mode::Paused(PauseReason::Idle) => "paused: no interaction".into(),
            Mode::Replay { fallback } => format!(
                "replay ×{}{}",
                self.speed(),
                if fallback {
                    " (stream unreachable)"
                } else {
                    ""
                }
            ),
            Mode::Live => match &frame.feed {
                None | Some(FeedState::Connecting) => "connecting".into(),
                Some(FeedState::Live) => match frame.counters.reconnects {
                    0 => "live".into(),
                    1 => "live · 1 reconnect".into(),
                    count => format!("live · {count} reconnects"),
                },
                Some(FeedState::Reconnecting { attempt, wait }) => format!(
                    "reconnecting in {:.0} s (attempt {attempt}){}",
                    wait.as_secs_f64().ceil(),
                    frame
                        .error
                        .as_ref()
                        .map_or_else(String::new, |error| format!(": {error}")),
                ),
                Some(FeedState::Unreachable) => "unreachable".into(),
                Some(FeedState::Replay { .. }) => "replay".into(),
            },
        }
    }

    /// A line under the toolbar that explains a pause or a fallback.
    fn notice(&self) -> Option<&'static str> {
        match self.mode {
            Mode::Paused(PauseReason::Hidden) => Some(
                "Paused while the tab was hidden for a minute: the stream opens again when the tab is shown.",
            ),
            Mode::Paused(PauseReason::Idle) => Some(
                "Paused after five minutes without interaction: the stream is closed. Resume to reconnect.",
            ),
            Mode::Replay { fallback: true } => {
                Some("Wikimedia's stream could not be reached: replaying a recording made earlier.")
            }
            Mode::Live if self.frame.feed == Some(FeedState::Unreachable) => {
                Some("Wikimedia's stream could not be reached. Try again later.")
            }
            _ => None,
        }
    }

    /// The mark, the mode and the keys.
    fn render_app_bar(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let hints = KEYS.iter().filter_map(|(keystrokes, command)| {
            let keystroke = Keystroke::parse(keystrokes).ok()?;
            Some(
                h_flex()
                    .gap(px(5.))
                    .child(Kbd::new(keystroke).outline())
                    .child(command.hint()),
            )
        });
        let mode = match self.mode {
            Mode::Live => Tag::success().small().child("live"),
            Mode::Replay { .. } => Tag::info().small().child("replay"),
            Mode::Paused(_) => Tag::warning().outline().small().child("paused"),
            Mode::Idle => Tag::secondary().outline().small().child("idle"),
        };
        h_flex()
            .flex_none()
            .w_full()
            .h(px(44.))
            .px(PAGE_PADDING + px(4.))
            .gap(px(14.))
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
                            .child("wiki"),
                    )
                    .when(!narrow, |mark| {
                        mark.child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child("the whole world editing Wikimedia, live"),
                        )
                    }),
            )
            .child(mode)
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

    /// Start, pause or resume; the replay; the search; the arrangement and
    /// the replay's speed.
    fn render_toolbar(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let has_live = self.transport.is_some();
        let has_recording = self.recording.is_some();
        let (label, tooltip) = match self.mode {
            Mode::Idle => ("Start live", "Open Wikimedia's live stream (S)"),
            Mode::Live => ("Pause", "Close the stream, keep the table (S)"),
            Mode::Paused(_) => ("Resume", "Reconnect to the stream (S)"),
            Mode::Replay { .. } => ("Go live", "Open Wikimedia's live stream (S)"),
        };
        let replaying = matches!(self.mode, Mode::Replay { .. });
        v_flex()
            .flex_none()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .px(ui::PANEL_PADDING)
                    .py(px(8.))
                    .gap(px(8.))
                    .child(
                        Button::new("live")
                            .label(label)
                            .small()
                            .primary()
                            .disabled(!has_live)
                            .tooltip(tooltip)
                            .on_click(cx.listener(|wiki, _, _, cx| wiki.toggle_live(cx))),
                    )
                    .when(has_recording, |bar| {
                        bar.child(
                            Button::new("replay")
                                .label("Replay")
                                .small()
                                .toggled(replaying)
                                .map(|button| {
                                    if replaying {
                                        button.primary()
                                    } else {
                                        button.outline()
                                    }
                                })
                                .tooltip("Play a recording instead of the live stream (R)")
                                .on_click(cx.listener(|wiki, _, _, cx| wiki.toggle_replay(cx))),
                        )
                    })
                    .child(
                        Input::new(&self.search)
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true)
                            .small()
                            .aria_label("Filter wikis")
                            .w(px(if narrow { 180. } else { 260. })),
                    )
                    .child(
                        TabBar::new("arrangement")
                            .segmented()
                            .small()
                            .selected_index(self.arrangement.ix())
                            .children(
                                Arrangement::ALL
                                    .map(|arrangement| Tab::new().label(arrangement.label())),
                            )
                            .on_click(cx.listener(|wiki, ix: &usize, _, cx| {
                                if let Some(arrangement) = Arrangement::ALL.get(*ix) {
                                    wiki.set_arrangement(*arrangement, cx);
                                }
                            })),
                    )
                    .when(replaying, |bar| {
                        const SPEEDS: [u32; 3] = [1, 4, 16];
                        let speed = self.speed();
                        bar.child(
                            TabBar::new("speed")
                                .segmented()
                                .small()
                                .selected_index(
                                    SPEEDS.iter().position(|each| *each == speed).unwrap_or(0),
                                )
                                .children(SPEEDS.map(|speed| Tab::new().label(format!("×{speed}"))))
                                .on_click(cx.listener(|wiki, ix: &usize, _, cx| {
                                    if let Some(speed) = SPEEDS.get(*ix) {
                                        wiki.set_speed(*speed, cx);
                                    }
                                })),
                        )
                    }),
            )
            .when_some(self.notice(), |toolbar, notice| {
                toolbar.child(
                    div()
                        .px(ui::PANEL_PADDING)
                        .pb(px(8.))
                        .text_color(theme.warning)
                        .child(notice),
                )
            })
    }

    /// Before the first start: what Start does, and the recording.
    fn render_start(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let has_live = self.transport.is_some();
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .p(px(24.))
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child("Every Wikimedia wiki, edited live"),
            )
            .child(
                div()
                    .max_w(px(560.))
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(
                        "Start opens one connection to Wikimedia's public EventStreams service \
                         and counts every edit per wiki: Wikipedia in every language, Wikidata, \
                         Commons and the rest, ordered by how busy they are right now. User names \
                         are never read or kept.",
                    ),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        Button::new("start")
                            .label("Start live")
                            .primary()
                            .disabled(!has_live)
                            .on_click(cx.listener(|wiki, _, _, cx| wiki.start_live(cx))),
                    )
                    .when(self.recording.is_some(), |buttons| {
                        buttons.child(
                            Button::new("start-replay")
                                .label("Replay a recording")
                                .outline()
                                .on_click(
                                    cx.listener(|wiki, _, _, cx| wiki.start_replay(false, cx)),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(ATTRIBUTION),
            )
    }

    /// The table, or the start.
    fn render_table(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let frame = &self.frame;
        let shown = self.grid.read(cx).projection().row_count();
        let detail = (frame.wikis > 0).then(|| {
            SharedString::from(format!(
                "{} wikis · {} edits/min · {} shown",
                thousands(frame.wikis as u64),
                thousands(frame.per_minute as u64),
                thousands(shown as u64),
            ))
        });
        let body = if self.mode == Mode::Idle && frame.wikis == 0 {
            self.render_start(cx).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .key_context(TABLE_CONTEXT)
                .child(
                    LiveGrid::new(&self.grid)
                        .with_size(Size::Size(ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Wikimedia wikis by edits a minute"),
                )
                .into_any_element()
        };
        ui::panel(cx)
            .size_full()
            .child(ui::panel_header("Wikis".into(), detail, cx))
            .child(self.render_toolbar(narrow, cx))
            .child(body)
    }

    fn render_pulse(&self, lanes: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let frame = &self.frame;
        let detail = (frame.events > 0).then(|| {
            SharedString::from(format!(
                "last minute · {:.0} events/s · ↑ bytes added, ↓ removed",
                frame.events_per_second
            ))
        });
        ui::panel(cx)
            .flex_none()
            .child(ui::panel_header("Pulse".into(), detail, cx))
            .child(strip::pulse_strip(
                self.frame.clone(),
                self.strip_now(),
                lanes,
                cx,
            ))
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let selected = self.control.selected();
        let detail = self
            .frame
            .detail
            .as_ref()
            .filter(|detail| Some(&detail.wiki) == selected.as_ref());
        let title = selected.map_or_else(|| SharedString::from("Wiki"), SharedString::from);
        ui::panel(cx)
            .flex_1()
            .child(ui::panel_header(title, None, cx))
            .child(detail::render_detail(detail, cx))
    }

    fn render_status(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = &self.frame;
        StatusBar::new()
            .font_features(ui::tabular_figures())
            .when(!narrow, |bar| {
                bar.left(SharedString::from(format!(
                    "{:.0} events/s · {} wikis · {} edits/min",
                    frame.events_per_second,
                    thousands(frame.wikis as u64),
                    thousands(frame.per_minute as u64),
                )))
                .left(SharedString::from(format!(
                    "{} rows/s · rebuild {:.1} ms · frame {:.2} ms",
                    thousands(frame.rows_per_second),
                    ms(self.status.rebuild),
                    ms(self.status.frame),
                )))
            })
            .left(SharedString::from(self.feed_text()))
            .right(div().text_color(theme.muted_foreground).child(if narrow {
                SHORT_ATTRIBUTION
            } else {
                ATTRIBUTION
            }))
    }
}

/// What the snapshot and the browser page drive and read the example
/// with: natively the snapshot calls part of it, with `test-support` only.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
impl Wiki {
    pub fn grid(&self) -> &Entity<LiveGridState<WikiTable>> {
        &self.grid
    }

    pub fn control(&self) -> &Arc<Control> {
        &self.control
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn arrangement(&self) -> Arrangement {
        self.arrangement
    }

    pub fn has_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn set_sort(
        &mut self,
        column: WikiColumn,
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

    /// Selects `wiki` in the table, if it has a row.
    pub fn select(&mut self, wiki: &str, cx: &mut Context<Self>) {
        let key: Arc<str> = wiki.into();
        if self.grid.read(cx).snapshot().get(&key).is_none() {
            return;
        }
        self.control.select(Some(key.clone()));
        self.grid.update(cx, |grid, cx| {
            grid.select_key(Some(key.clone()), cx);
            grid.scroll_to_key(&key, cx);
        });
        cx.notify();
    }

    /// The first wiki on screen, if any: the busiest, sorted by edits a
    /// minute.
    pub fn first_shown(&self, cx: &App) -> Option<Arc<str>> {
        self.grid.read(cx).visible_keys().iter().next().cloned()
    }

    /// The status as one line, for the snapshot and the page.
    pub fn status_line(&self, cx: &App) -> String {
        let frame = &self.frame;
        let shown = self.grid.read(cx).projection().row_count();
        format!(
            "{} · {:.1} events/s · {} wikis · {} shown · {} edits/min · {} edits · {} events · {} rows/s, {} in {} steps · rebuild {:.1} ms · frame {:.2} ms · step {:.2} ms · canary {} · malformed {}",
            self.feed_text(),
            frame.events_per_second,
            thousands(frame.wikis as u64),
            thousands(shown as u64),
            thousands(frame.per_minute as u64),
            thousands(frame.edits),
            thousands(frame.events),
            thousands(frame.rows_per_second),
            thousands(frame.rows_written),
            thousands(frame.steps),
            ms(self.status.rebuild),
            ms(self.status.frame),
            ms(frame.step),
            frame.counters.canary,
            frame.counters.malformed,
        )
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

impl Drop for Wiki {
    fn drop(&mut self) {
        self.control.close();
    }
}

impl Focusable for Wiki {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for Wiki {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        let narrow = width < NARROW;
        let side = (width * SIDE_SHARE).clamp(SIDE_MIN, SIDE_MAX);
        let theme = cx.theme();
        let (background, foreground, family, text_size) = (
            theme.background,
            theme.foreground,
            theme.font_family.clone(),
            theme.font_size,
        );
        let main = if narrow {
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .p(PAGE_PADDING)
                .gap(GAP)
                .child(self.render_pulse(3, cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(self.render_table(narrow, cx)),
                )
                .into_any_element()
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
                        .w(px(side))
                        .h_full()
                        .gap(GAP)
                        .child(self.render_pulse(8, cx))
                        .child(self.render_detail(cx)),
                )
                .into_any_element()
        };
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_toggle_live))
            .on_action(cx.listener(Self::on_toggle_replay))
            .on_action(cx.listener(Self::on_toggle_grouping))
            .on_action(cx.listener(Self::on_open_page))
            .on_action(cx.listener(Self::on_focus_search))
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

    #[test]
    fn test_settings_clamp_the_speed_and_skip_an_empty_recording() {
        let settings = Settings::new(Start::Replay)
            .with_speed(99)
            .with_recording(Some(Arc::new(Recording::default())));
        assert_eq!(settings.speed, 16);
        assert!(settings.recording.is_none());
        assert_eq!(Settings::new(Start::Idle).with_speed(0).speed, 1);
        assert_eq!(Start::default(), Start::Idle, "never live without asking");
    }
}
