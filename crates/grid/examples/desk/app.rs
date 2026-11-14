//! The desk itself: a dock of grid panels, the chart, the command line,
//! the help strip and the status bar, over the simulator. The native `main`
//! (`main.rs`) opens it in a window; the browser build (`crates/grid-web`)
//! in the page's canvas. Both call `setup` once, then `open`.

use std::{sync::Arc, time::Duration};

use giglabo_products::ProductsButton;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    dock::{DockArea, DockLayout, DockSkin, panel_handle},
    input::{Input, InputEvent, InputState},
    status_bar::StatusBar,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::GridStore;
// `std::time` natively; the browser's clock in wasm, where
// `std::time::{Instant, SystemTime}::now` panic.
use web_time::{Instant, SystemTime};

mod book;
mod command;
mod panels;
mod positions;
#[cfg(feature = "test-support")]
pub mod snapshot;
mod tape;
mod theme;

pub use self::theme::DeskTheme;
use self::{
    book::BookPanel,
    command::Command,
    panels::{ChartPanel, NewsPanel, WatchlistPanel},
    positions::PositionsPanel,
    tape::TapePanel,
};
use crate::trading::{
    bus::InstrumentBus,
    instruments::{self, InstrumentId},
    market::{self, ChartFeed, Control, Feeds, TapeScope},
    theme::TradingTheme as _,
    worker,
};

gpui_kit::actions!(
    desk,
    [
        /// Shows or hides the help strip (F1, `HELP`).
        ToggleHelp,
        /// The Amber theme (F2).
        AmberTheme,
        /// The Depth theme (F3).
        DepthTheme,
        /// The Heat theme (F4).
        HeatTheme,
        /// The chart's next timeframe: 5s, 1m, 5m (F5, `TF`).
        NextTimeframe,
        /// Moves the focus to the command line (F6).
        FocusCommandLine,
        /// Moves the focus to the watchlist (F7).
        FocusWatchlist,
        /// Moves the focus to the news (F8).
        FocusNews,
        /// Moves the focus to the order book (F9).
        FocusBook,
        /// Moves the focus to the trades tape (F10).
        FocusTape,
        /// Moves the focus to the positions (F12).
        FocusPositions,
    ]
);

/// The key context of the window content.
const KEY_CONTEXT: &str = "Desk";
/// The instrument made current once the first quotes are on screen.
const FIRST_TICKER: &str = "NVDA";

/// What the desk starts with: natively from the environment (`from_env`),
/// in the browser from the page's address.
pub struct Settings {
    rate: u64,
    theme: DeskTheme,
    seed: Option<u64>,
    quit_after: Option<u64>,
}

impl Settings {
    pub fn from_env() -> Self {
        let number = |name: &str| {
            std::env::var(name).ok().and_then(|value| {
                value
                    .parse()
                    .inspect_err(|_| err!("{name}={value}: not a number; ignored"))
                    .ok()
            })
        };
        let theme = std::env::var("DESK_THEME")
            .ok()
            .map_or(DeskTheme::Amber, |value| {
                DeskTheme::parse(&value).unwrap_or_else(|| {
                    err!("DESK_THEME={value}: not amber, depth or heat; amber is used");
                    DeskTheme::Amber
                })
            });
        Self {
            rate: number("DESK_RATE").unwrap_or(market::DEFAULT_RATE),
            theme,
            seed: number("DESK_SEED"),
            quit_after: number("DESK_SECONDS"),
        }
    }
}

/// The status line's counters, refreshed every half second.
#[derive(Default)]
struct Status {
    /// Updates per second the simulator wrote over the last interval.
    rate: u64,
    /// Frames the watchlist painted per second.
    fps: u64,
    clock: SharedString,
}

/// A command's answer on the command line.
struct Message {
    text: SharedString,
    error: bool,
}

pub struct Desk {
    area: Entity<DockArea>,
    bus: Entity<InstrumentBus>,
    control: Arc<Control>,
    watchlist: Entity<WatchlistPanel>,
    news: Entity<NewsPanel>,
    chart: Entity<ChartPanel>,
    book: Entity<BookPanel>,
    tape: Entity<TapePanel>,
    positions: Entity<PositionsPanel>,
    command: Entity<InputState>,
    message: Option<Message>,
    help: bool,
    status: Status,
    _tasks: [Task<()>; 3],
    _subscriptions: Vec<Subscription>,
}

impl Desk {
    fn new(settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let control = Control::new(settings.rate);
        let quotes = GridStore::new(panels::quote_store_options(), cx);
        let tape = GridStore::new(tape::store_options(), cx);
        let book = GridStore::new(book::store_options(), cx);
        let positions = GridStore::new(positions::store_options(), cx);
        let news = GridStore::new(panels::news_store_options(), cx);
        let chart = ChartFeed::default();
        let feeds = Feeds::new()
            .with_quotes(quotes.writer())
            .with_tape(tape.writer())
            .with_book(book.writer())
            .with_positions(positions.writer())
            .with_news(news.writer())
            .with_chart(chart.clone());

        let bus = cx.new(|_| InstrumentBus::new(control.clone()));
        let watchlist = cx.new(|cx| WatchlistPanel::new(quotes, &bus, cx));
        let news_panel = cx.new(|cx| NewsPanel::new(news, &bus, cx));
        let chart_panel = cx.new(|cx| ChartPanel::new(chart, cx));
        let book = cx.new(|cx| BookPanel::new(book, &bus, cx));
        let tape = cx.new(|cx| TapePanel::new(tape, &bus, control.clone(), cx));
        let positions = cx.new(|cx| PositionsPanel::new(positions, &bus, cx));

        // Watchlist and news on the left, the chart over the tape and the
        // positions in the middle, the book on the right.
        let (area, _) = DockSkin::dock_area("desk", None, window, cx);
        let tabs = |panel| DockLayout::tabs().panel_view(panel, cx);
        let layout = DockLayout::h_split()
            .child(
                DockLayout::v_split()
                    .child(tabs(panel_handle(watchlist.clone())), None)
                    .child(tabs(panel_handle(news_panel.clone())), Some(px(240.))),
                Some(px(660.)),
            )
            .child(
                DockLayout::v_split()
                    .child(tabs(panel_handle(chart_panel.clone())), None)
                    .child(
                        DockLayout::h_split()
                            .child(tabs(panel_handle(tape.clone())), None)
                            .child(tabs(panel_handle(positions.clone())), None),
                        Some(px(260.)),
                    ),
                None,
            )
            .child(tabs(panel_handle(book.clone())), Some(px(300.)));
        area.update(cx, |area, cx| area.set_center(layout, window, cx));

        let command =
            cx.new(|cx| InputState::new(window, cx).placeholder("Ticker or command (HELP)"));
        let subscriptions = vec![cx.subscribe_in(&command, window, Self::on_command_event)];

        let (first_load, simulator) =
            worker::start_market(settings.seed, feeds, control.clone(), cx);
        // Once the first quotes are on screen, make the first instrument
        // current: the watchlist selects it, the other panels follow.
        let first = cx.spawn(async move |this, cx| {
            if first_load.recv().await.is_err() {
                return;
            }
            let Ok(flushed) = this.update(cx, |desk, cx| {
                let grid = desk.watchlist.read(cx).grid().clone();
                grid.update(cx, |grid, cx| grid.flush(cx))
            }) else {
                return;
            };
            if flushed.await.is_some() {
                let _ = this.update(cx, |desk, cx| {
                    let first = instruments::find(FIRST_TICKER);
                    desk.bus.update(cx, |bus, cx| bus.set_current(first, cx));
                });
            }
        });

        Self {
            area,
            bus,
            control,
            watchlist,
            news: news_panel,
            chart: chart_panel,
            book,
            tape,
            positions,
            command,
            message: None,
            help: false,
            status: Status::default(),
            _tasks: [simulator, first, Self::refresh(settings.quit_after, cx)],
            _subscriptions: subscriptions,
        }
    }

    /// Every half second: the measured rate, the frames and the clock. With
    /// `DESK_SECONDS=N` prints the status every second and quits after N
    /// seconds.
    fn refresh(quit_after: Option<u64>, cx: &mut Context<Self>) -> Task<()> {
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let (mut last_updates, mut last_frames, mut ticks) = (0, 0, 0u64);
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let Ok(done) = this.update(cx, |desk, cx| {
                    let updates = desk.control.updates();
                    let frames = desk.watchlist.read(cx).grid().read(cx).stats().frames();
                    desk.status = Status {
                        rate: (updates - last_updates) * 2,
                        fps: (frames - last_frames) * 2,
                        clock: market::utc_time(SystemTime::now(), false).as_ref().into(),
                    };
                    (last_updates, last_frames) = (updates, frames);
                    ticks += 1;
                    if quit_after.is_some() && ticks % 2 == 0 {
                        out!(
                            "{:>5.1}s  {}",
                            started.elapsed().as_secs_f64(),
                            desk.status_line(cx)
                        );
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

    fn on_command_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::PressEnter { .. }) {
            return;
        }
        let text = input.read(cx).value();
        // An empty line asks for nothing, as at a shell prompt. In the
        // browser one Enter can also arrive twice (the key, then its `\r`),
        // and the second must not answer the first command with an error.
        if text.trim().is_empty() {
            return;
        }
        self.message = match command::parse(&text) {
            Ok(command) => self
                .run(command, cx)
                .map(|text| Message { text, error: false }),
            Err(error) => Some(Message {
                text: error.into(),
                error: true,
            }),
        };
        input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// Runs `command`; returns what the command line says about it, if
    /// anything.
    fn run(&mut self, command: Command, cx: &mut Context<Self>) -> Option<SharedString> {
        let message = match command {
            Command::Select(id) => {
                self.bus.update(cx, |bus, cx| bus.set_current(Some(id), cx));
                let instrument = id.get();
                format!(
                    "{} · {} · {}",
                    instrument.ticker,
                    instrument.name,
                    instrument.sector.label()
                )
                .into()
            }
            Command::Theme(theme) => {
                theme.apply(cx);
                format!("Theme {}", theme.label()).into()
            }
            Command::Rate(rate) => {
                let rate = self.control.set_rate(rate);
                format!("Rate {} updates/s", thousands(rate)).into()
            }
            Command::Group(column) => {
                let grid = self.watchlist.read(cx).grid().clone();
                grid.update(cx, |grid, cx| {
                    grid.set_group_by(panels::group_by(column), cx)
                });
                match column {
                    Some(column) => {
                        format!("Grouped by {}", gpui_kit_grid::GridColumns::id(column)).into()
                    }
                    None => "Not grouped".into(),
                }
            }
            Command::Timeframe(timeframe) => {
                self.chart
                    .update(cx, |chart, cx| chart.set_timeframe(timeframe, cx));
                format!("Chart {}", timeframe.label()).into()
            }
            Command::Tape(scope) => {
                self.tape.update(cx, |tape, cx| tape.set_scope(scope, cx));
                match scope {
                    TapeScope::Current => "Trades of the current instrument".into(),
                    TapeScope::All => "Trades of every instrument".into(),
                }
            }
            Command::Help => {
                self.help = !self.help;
                return None;
            }
        };
        Some(message)
    }

    fn on_toggle_help(&mut self, _: &ToggleHelp, _: &mut Window, cx: &mut Context<Self>) {
        self.help = !self.help;
        cx.notify();
    }

    fn set_theme(&mut self, theme: DeskTheme, cx: &mut Context<Self>) {
        theme.apply(cx);
        self.message = Some(Message {
            text: format!("Theme {}", theme.label()).into(),
            error: false,
        });
        cx.notify();
    }

    fn on_amber(&mut self, _: &AmberTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.set_theme(DeskTheme::Amber, cx);
    }

    fn on_depth(&mut self, _: &DepthTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.set_theme(DeskTheme::Depth, cx);
    }

    fn on_heat(&mut self, _: &HeatTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.set_theme(DeskTheme::Heat, cx);
    }

    fn on_next_timeframe(&mut self, _: &NextTimeframe, _: &mut Window, cx: &mut Context<Self>) {
        let timeframe = self.chart.read(cx).timeframe().next();
        self.message = self
            .run(Command::Timeframe(timeframe), cx)
            .map(|text| Message { text, error: false });
        cx.notify();
    }

    fn on_focus_command_line(
        &mut self,
        _: &FocusCommandLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command.update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_focus_watchlist(
        &mut self,
        _: &FocusWatchlist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self.watchlist.focus_handle(cx);
        window.focus(&focus, cx);
    }

    fn on_focus_news(&mut self, _: &FocusNews, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.news.focus_handle(cx);
        window.focus(&focus, cx);
    }

    fn on_focus_book(&mut self, _: &FocusBook, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.book.focus_handle(cx);
        window.focus(&focus, cx);
    }

    fn on_focus_tape(&mut self, _: &FocusTape, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.tape.focus_handle(cx);
        window.focus(&focus, cx);
    }

    fn on_focus_positions(
        &mut self,
        _: &FocusPositions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self.positions.focus_handle(cx);
        window.focus(&focus, cx);
    }

    /// The status as one line, for `DESK_SECONDS`.
    fn status_line(&self, cx: &App) -> String {
        let current = self.bus.read(cx).current();
        format!(
            "SIMULATED · {} updates/s (target {}) · watchlist {} fps · {} · {} UTC",
            thousands(self.status.rate),
            thousands(self.control.rate()),
            self.status.fps,
            current.map_or("—", |id| id.get().ticker),
            self.status.clock,
        )
    }

    fn render_command_line(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .w_full()
            .h(px(30.))
            .px_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .font_family(theme.mono_font_family.clone())
            .text_size(theme.mono_font_size)
            .child(div().flex_none().text_color(theme.primary).child("CMD ›"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.command).appearance(false).small()),
            )
            .when_some(self.message.as_ref(), |row, message| {
                row.child(
                    div()
                        .flex_none()
                        .text_color(if message.error {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .child(message.text.clone()),
                )
            })
            .child(ProductsButton::new())
    }

    fn render_help(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        v_flex()
            .flex_none()
            .w_full()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_size(theme.mono_font_size)
            .text_color(theme.muted_foreground)
            .children(command::HELP.map(SharedString::from))
    }

    fn render_status(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let current = self.bus.read(cx).current().map(InstrumentId::get);
        let theme_label = DeskTheme::current(cx).map_or("—", DeskTheme::label);
        StatusBar::new()
            .font_family(theme.mono_font_family.clone())
            .left(
                div()
                    .px_1()
                    .bg(theme.warning)
                    .text_color(theme.warning_foreground)
                    .child("SIMULATED"),
            )
            .left(SharedString::from(format!(
                "{} upd/s · target {}",
                thousands(self.status.rate),
                thousands(self.control.rate())
            )))
            .left(SharedString::from(format!(
                "watchlist {} fps",
                self.status.fps
            )))
            .right(SharedString::from(match current {
                Some(instrument) => format!("{} · {}", instrument.ticker, instrument.name),
                None => "No instrument".into(),
            }))
            .right(SharedString::from(format!("Theme {theme_label}")))
            .right(
                div()
                    .text_color(theme.foreground)
                    .child(SharedString::from(format!("{} UTC", self.status.clock))),
            )
    }
}

/// `5000` as `5,000`.
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix) % 3 == 0 {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

impl Render for Desk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_toggle_help))
            .on_action(cx.listener(Self::on_amber))
            .on_action(cx.listener(Self::on_depth))
            .on_action(cx.listener(Self::on_heat))
            .on_action(cx.listener(Self::on_next_timeframe))
            .on_action(cx.listener(Self::on_focus_command_line))
            .on_action(cx.listener(Self::on_focus_watchlist))
            .on_action(cx.listener(Self::on_focus_news))
            .on_action(cx.listener(Self::on_focus_book))
            .on_action(cx.listener(Self::on_focus_tape))
            .on_action(cx.listener(Self::on_focus_positions))
            .bg(cx.theme().background)
            .child(self.render_command_line(cx))
            .when(self.help, |this| this.child(self.render_help(cx)))
            .child(div().flex_1().min_h_0().child(self.area.clone()))
            .child(self.render_status(cx))
    }
}

/// gpui-kit, the grid's key bindings (arrows, Page, Home/End, Enter,
/// Escape), the desk's themes with their fonts, the start theme and the
/// desk's keys. Call once, before `open`.
pub fn setup(settings: &Settings, cx: &mut App) {
    gpui_kit::init(cx);
    gpui_kit_grid::init(cx);
    crate::trading::theme::install::<DeskTheme>(cx);
    settings.theme.apply(cx);
    cx.bind_keys(key_bindings());
}

/// The desk in `window`: the simulator started, the keyboard in the
/// watchlist.
pub fn open(settings: &Settings, window: &mut Window, cx: &mut App) -> Entity<Desk> {
    let desk = cx.new(|cx| Desk::new(settings, window, cx));
    let focus = desk.read(cx).watchlist.focus_handle(cx);
    window.focus(&focus, cx);
    desk
}

/// The desk's keys, its own, under its key context.
fn key_bindings() -> [KeyBinding; 11] {
    let context = Some(KEY_CONTEXT);
    [
        KeyBinding::new("f1", ToggleHelp, context),
        KeyBinding::new("f2", AmberTheme, context),
        KeyBinding::new("f3", DepthTheme, context),
        KeyBinding::new("f4", HeatTheme, context),
        KeyBinding::new("f5", NextTimeframe, context),
        KeyBinding::new("f6", FocusCommandLine, context),
        KeyBinding::new("f7", FocusWatchlist, context),
        KeyBinding::new("f8", FocusNews, context),
        KeyBinding::new("f9", FocusBook, context),
        KeyBinding::new("f10", FocusTape, context),
        // Not F11: macOS shows the desktop with it.
        KeyBinding::new("f12", FocusPositions, context),
    ]
}

/// The browser build's side (`crates/grid-web`): the page's settings, and
/// what its scripts read and set.
#[cfg(target_family = "wasm")]
impl Settings {
    /// The page's `rate`, `theme` and `seed`, the defaults otherwise.
    pub fn new(rate: Option<u64>, theme: Option<DeskTheme>, seed: Option<u64>) -> Self {
        Self {
            rate: rate.unwrap_or(market::DEFAULT_RATE),
            theme: theme.unwrap_or(DeskTheme::Amber),
            seed,
            quit_after: None,
        }
    }
}

#[cfg(target_family = "wasm")]
impl Desk {
    /// The simulator's rate and counters.
    pub fn control(&self) -> &Arc<Control> {
        &self.control
    }

    /// The watchlist's grid.
    pub fn watchlist_grid<'a>(
        &self,
        cx: &'a App,
    ) -> &'a Entity<gpui_kit_grid::LiveGridState<panels::Watchlist>> {
        self.watchlist.read(cx).grid()
    }

    /// The current instrument, the theme, the chart's timeframe, the help
    /// strip, the command line's last answer and the panel with the
    /// keyboard, for the page's input checks.
    pub fn page_state(&self, window: &Window, cx: &App) -> serde_json::Value {
        let panels = [
            ("command", self.command.focus_handle(cx)),
            ("watchlist", self.watchlist.focus_handle(cx)),
            ("news", self.news.focus_handle(cx)),
            ("chart", self.chart.focus_handle(cx)),
            ("book", self.book.focus_handle(cx)),
            ("tape", self.tape.focus_handle(cx)),
            ("positions", self.positions.focus_handle(cx)),
        ];
        let focus = panels
            .iter()
            .find(|(_, handle)| handle.contains_focused(window, cx))
            .map(|(name, _)| *name);
        serde_json::json!({
            "current": self.bus.read(cx).current().map(|id| id.get().ticker),
            "theme": DeskTheme::current(cx).map(DeskTheme::label),
            "timeframe": self.chart.read(cx).timeframe().label(),
            "help": self.help,
            "message": self.message.as_ref().map(|message| message.text.as_ref()),
            "message_error": self.message.as_ref().is_some_and(|message| message.error),
            "focus": focus,
        })
    }
}
