//! A news and analytics workstation over a simulated market: a news
//! monitor in the centre, the open story beside it with live quotes and a
//! chart of the instruments it mentions, and a market overview by sector;
//! a navy and teal theme in Inter, with room to breathe.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: Inter
//! cargo run --release -p gpui-kit-grid --example wire
//! # No window: the wire drawn offscreen to PNG files (`snapshot.rs`)
//! WIRE_SNAPSHOT=target/wire cargo run --release -p gpui-kit-grid --features test-support --example wire
//! ```
//!
//! **Everything is simulated.** The instruments are real US-listed tickers
//! and names, but their prices random-walk from approximate seed levels
//! (`trading/instruments.rs`, `trading/market.rs`). The stories are written
//! by the example's own newsroom (`newsroom.rs`) from fixed, neutral
//! templates over the simulated quotes: moves, ranges, volume, breadth,
//! sector rotation and gauges. They never report a corporate event of a
//! real company, and every one says it is simulated.
//!
//! - The app bar: the tabs (News, Markets, Watchlist) and a search box
//!   that filters the news monitor through the grid's quick filter.
//! - News: the market overview (grouped by sector with the average %Chg
//!   and the summed volume in the captions, sorted by %Chg, the index funds
//!   pinned on top), the news monitor (newest first, unread stories bold,
//!   a new one tinted, category chips), the story pane, and the quotes and
//!   chart of the instruments the open story mentions.
//! - Markets: the overview with every quote column, and the chart.
//! - Watchlist: a simulated watchlist, and the chart.
//!
//! Opening a story (selecting it, with the mouse or the keys) marks it
//! read and makes the first instrument it mentions current; a ticker chip,
//! or a row of any quote grid, makes its instrument current, and the
//! other grids and the chart follow it.
//!
//! The keys are one table (`KEYS`), which the bindings and the status
//! bar's hints are made from: `secondary` is Cmd on macOS and Ctrl
//! elsewhere.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `WIRE_RATE` | 300 | Simulated price updates per second |
//! | `WIRE_SEED` | none | A fixed seed: the same simulated market every run |
//! | `WIRE_SECONDS` | none | Print the status every second and quit after N seconds |
//! | `WIRE_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime},
};

use giglabo_products::ProductsButton;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size,
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    status_bar::StatusBar,
    tab::{Tab, TabBar},
    tag::Tag,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, Keystroke, ParentElement as _, Pixels,
    Render, SharedString, Styled as _, Subscription, Task, Window, WindowBounds, WindowOptions,
    div, px, size,
};
use gpui_kit_grid::{GridStore, LiveGrid, LiveGridState};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

mod chart;
mod markets;
mod news;
mod newsroom;
#[cfg(feature = "test-support")]
mod snapshot;
mod story;
mod theme;
mod ui;

use crate::{
    chart::InstrumentChart,
    markets::Quotes,
    news::{NewsMonitor, StoryChange},
    newsroom::{NewsCounts, Newsroom},
    theme::WireTheme,
    trading::{
        bus::InstrumentBus,
        instruments,
        market::{self, ChartFeed, Control, Feeds, Market},
        rows::QuoteColumn,
        theme::TradingTheme as _,
    },
    ui::{GAP, PAGE_PADDING},
};

gpui_kit::actions!(
    wire,
    [
        /// Moves the focus to the search box.
        FocusSearch,
        /// Opens the next story: the one below the open one.
        NextStory,
        /// Opens the previous story: the one above the open one.
        PreviousStory,
        /// Marks every story read.
        MarkAllRead,
        /// Shows the News tab.
        ShowNews,
        /// Shows the Markets tab.
        ShowMarkets,
        /// Shows the Watchlist tab.
        ShowWatchlist,
    ]
);

/// The key context of the window content.
pub const KEY_CONTEXT: &str = "Wire";
/// The current instrument at start, before a story names one.
const FIRST_TICKER: &str = "SPY";

/// The keys, as data: the bindings and the status bar's hints are both
/// made from this table. `secondary` is Cmd on macOS and Ctrl elsewhere.
/// Alt with the arrows leaves the arrows alone to the grids and the
/// search box.
const KEYS: [(&str, Command); 7] = [
    ("secondary-f", Command::FocusSearch),
    ("alt-down", Command::NextStory),
    ("alt-up", Command::PreviousStory),
    ("shift-secondary-r", Command::MarkAllRead),
    ("secondary-1", Command::Show(Section::News)),
    ("secondary-2", Command::Show(Section::Markets)),
    ("secondary-3", Command::Show(Section::Watchlist)),
];

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    FocusSearch,
    NextStory,
    PreviousStory,
    MarkAllRead,
    Show(Section),
}

impl Command {
    /// The binding of `keystrokes` to this command's action, in the
    /// wire's key context.
    fn binding(self, keystrokes: &str) -> KeyBinding {
        let context = Some(KEY_CONTEXT);
        match self {
            Self::FocusSearch => KeyBinding::new(keystrokes, FocusSearch, context),
            Self::NextStory => KeyBinding::new(keystrokes, NextStory, context),
            Self::PreviousStory => KeyBinding::new(keystrokes, PreviousStory, context),
            Self::MarkAllRead => KeyBinding::new(keystrokes, MarkAllRead, context),
            Self::Show(Section::News) => KeyBinding::new(keystrokes, ShowNews, context),
            Self::Show(Section::Markets) => KeyBinding::new(keystrokes, ShowMarkets, context),
            Self::Show(Section::Watchlist) => KeyBinding::new(keystrokes, ShowWatchlist, context),
        }
    }

    /// The status bar's word for it.
    fn hint(self) -> &'static str {
        match self {
            Self::FocusSearch => "Search",
            Self::NextStory => "Next story",
            Self::PreviousStory => "Previous",
            Self::MarkAllRead => "Mark all read",
            Self::Show(section) => section.label(),
        }
    }
}

/// Every binding of `KEYS`.
fn key_bindings() -> Vec<KeyBinding> {
    KEYS.iter()
        .map(|(keystrokes, command)| command.binding(keystrokes))
        .collect()
}

/// The tabs of the app bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    News,
    Markets,
    Watchlist,
}

impl Section {
    pub const ALL: [Self; 3] = [Self::News, Self::Markets, Self::Watchlist];

    pub fn label(self) -> &'static str {
        match self {
            Self::News => "News",
            Self::Markets => "Markets",
            Self::Watchlist => "Watchlist",
        }
    }

    fn ix(self) -> usize {
        self as usize
    }
}

/// The overview's width beside the news monitor.
const OVERVIEW_WIDTH: Pixels = px(350.);
/// The story column's width beside the news monitor.
const STORY_WIDTH: Pixels = px(460.);
/// The chart's width beside a list on the Markets and Watchlist tabs.
const CHART_WIDTH: Pixels = px(500.);
/// The least a filled Name column takes.
const MIN_NAME_WIDTH: Pixels = px(160.);
/// The mentioned instruments' card under the story.
const MENTIONED_CARD_HEIGHT: Pixels = px(430.);
const APP_BAR_HEIGHT: Pixels = px(56.);
const SEARCH_WIDTH: Pixels = px(400.);

/// What the environment sets.
struct Settings {
    rate: u64,
    seed: Option<u64>,
    quit_after: Option<u64>,
}

impl Settings {
    fn from_env() -> Self {
        let number = |name: &str| {
            std::env::var(name).ok().and_then(|value| {
                value
                    .parse()
                    .inspect_err(|_| err!("{name}={value}: not a number; ignored"))
                    .ok()
            })
        };
        Self {
            rate: number("WIRE_RATE").unwrap_or(market::DEFAULT_RATE),
            seed: number("WIRE_SEED"),
            quit_after: number("WIRE_SECONDS"),
        }
    }
}

/// The status line's counters, refreshed every half second.
#[derive(Default)]
struct Status {
    /// Updates per second the simulator wrote over the last interval.
    rate: u64,
    clock: SharedString,
}

/// The workstation: the app bar, the tab shown and the status bar.
pub struct Wire {
    section: Section,
    bus: Entity<InstrumentBus>,
    control: Arc<Control>,
    counts: Arc<NewsCounts>,
    search: Entity<InputState>,
    news: Entity<NewsMonitor>,
    overview: Entity<LiveGridState<Quotes>>,
    markets: Entity<LiveGridState<Quotes>>,
    mentioned: Entity<LiveGridState<Quotes>>,
    watchlist: Entity<LiveGridState<Quotes>>,
    chart: Entity<InstrumentChart>,
    status: Status,
    _tasks: [Task<()>; 2],
    _subscriptions: Vec<Subscription>,
}

impl Wire {
    fn new(settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let control = Control::new(settings.rate);
        let quotes = GridStore::new(markets::store_options(), cx);
        let overview_quotes = GridStore::new(markets::store_options(), cx);
        let stories = GridStore::new(news::store_options(), cx);
        let chart_feed = ChartFeed::default();
        let counts = Arc::new(NewsCounts::default());
        let (commands, inbox) = mpsc::channel();

        // The chart shows the broad market until a story names an
        // instrument.
        let bus = cx.new(|cx| {
            let mut bus = InstrumentBus::new(control.clone());
            bus.set_current(instruments::find(FIRST_TICKER), cx);
            bus
        });
        let news = cx.new(|cx| NewsMonitor::new(stories.clone(), commands, counts.clone(), cx));
        let overview = markets::overview(overview_quotes.clone(), cx);
        let markets_grid = markets::markets(overview_quotes.clone(), cx);
        let mentioned = markets::mentioned(quotes.clone(), cx);
        let watchlist = markets::watchlist(quotes.clone(), cx);
        let chart = cx.new(|cx| InstrumentChart::new(chart_feed.clone(), cx));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search the news"));

        let mut subscriptions = vec![
            cx.subscribe_in(&search, window, Self::on_search_event),
            cx.subscribe(&news, Self::on_story_change),
            // The chips show the current instrument.
            cx.observe(&bus, |_, _, cx| cx.notify()),
            cx.observe(&news, |_, _, cx| cx.notify()),
            // The news monitor's headline takes the width it is left.
            cx.observe_window_bounds(window, |wire, window, cx| wire.fit_columns(window, cx)),
        ];
        for grid in [&overview, &markets_grid, &mentioned, &watchlist] {
            subscriptions.extend(markets::follow_current(grid, &bus, cx));
        }

        // The simulator, the relay into the overview's store and the
        // newsroom, each on its own thread; each stops once its stores are
        // gone.
        let seed = settings.seed;
        let feeds = Feeds::new()
            .with_quotes(quotes.writer())
            .with_chart(chart_feed);
        let (market_loaded, _) = smol::channel::bounded(1);
        let sim_control = control.clone();
        std::thread::spawn(move || {
            market::run(Market::new(seed), feeds, sim_control, market_loaded)
        });
        let (source, target) = (quotes.writer(), overview_quotes.writer());
        std::thread::spawn(move || markets::relay(source, target));
        let (news_loaded, first_news) = smol::channel::bounded(1);
        let (source, target, news_counts) = (quotes.writer(), stories.writer(), counts.clone());
        std::thread::spawn(move || {
            newsroom::run(
                Newsroom::new(seed),
                source,
                target,
                inbox,
                news_counts,
                news_loaded,
            )
        });

        // Once the first stories are on screen, open the newest.
        let first = cx.spawn(async move |this, cx| {
            if first_news.recv().await.is_err() {
                return;
            }
            let Ok(flushed) = this.update(cx, |wire, cx| {
                let grid = wire.news.read(cx).grid().clone();
                grid.update(cx, |grid, cx| grid.flush(cx))
            }) else {
                return;
            };
            if flushed.await.is_some() {
                let _ = this.update(cx, |wire, cx| {
                    wire.news
                        .update(cx, |monitor, cx| monitor.open_adjacent(0, cx))
                });
            }
        });

        let mut wire = Self {
            section: Section::default(),
            bus,
            control,
            counts,
            search,
            news,
            overview,
            markets: markets_grid,
            mentioned,
            watchlist,
            chart,
            status: Status::default(),
            _tasks: [first, Self::refresh(settings.quit_after, cx)],
            _subscriptions: subscriptions,
        };
        wire.fit_columns(window, cx);
        wire
    }

    /// Every half second: the measured rate and the clock. With
    /// `WIRE_SECONDS=N` prints the status every second and quits after N
    /// seconds.
    fn refresh(quit_after: Option<u64>, cx: &mut Context<Self>) -> Task<()> {
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let (mut last_updates, mut ticks) = (0, 0u64);
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let Ok(done) = this.update(cx, |wire, cx| {
                    let updates = wire.control.updates();
                    wire.status = Status {
                        rate: (updates - last_updates) * 2,
                        clock: market::utc_time(SystemTime::now(), false).as_ref().into(),
                    };
                    last_updates = updates;
                    ticks += 1;
                    if quit_after.is_some() && ticks % 2 == 0 {
                        out!(
                            "{:>5.1}s  {}",
                            started.elapsed().as_secs_f64(),
                            wire.status_line()
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

    /// The status as one line, for `WIRE_SECONDS`.
    fn status_line(&self) -> String {
        format!(
            "SIMULATED · {} updates/s · {} stories, {} unread · {} UTC",
            thousands(self.status.rate),
            self.counts.stories(),
            self.counts.total_unread(),
            self.status.clock,
        )
    }

    /// Gives the news monitor's headline, and the names on the Markets and
    /// Watchlist tabs, the width the window leaves them.
    fn fit_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = window.viewport_size().width - PAGE_PADDING * 2.;
        let news = page - OVERVIEW_WIDTH - STORY_WIDTH - GAP * 2.;
        self.news
            .update(cx, |monitor, cx| monitor.fit_headline(news, cx));
        // A list's card beside the chart: inside its border, the grid's
        // inset at each side.
        let list = page - CHART_WIDTH - GAP - px(2.) - ui::GRID_INSET * 2.;
        for grid in [&self.markets, &self.watchlist] {
            ui::fill_column(grid, QuoteColumn::Name, list, MIN_NAME_WIDTH, cx);
        }
    }

    fn on_search_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let text = input.read(cx).value();
        // The search box searches the news: typing shows them.
        if !text.trim().is_empty() && self.section != Section::News {
            self.show(Section::News, window, cx);
        }
        self.news
            .update(cx, |monitor, cx| monitor.set_search(text, cx));
    }

    /// The open story changed: its instruments' quotes, and the first one
    /// current.
    fn on_story_change(
        &mut self,
        news: Entity<NewsMonitor>,
        _: &StoryChange,
        cx: &mut Context<Self>,
    ) {
        let Some(story) = news.read(cx).story().cloned() else {
            return;
        };
        let view = markets::mentioned_view(story.tickers.clone());
        self.mentioned
            .update(cx, |grid, cx| grid.set_view(view, cx));
        if let Some(&first) = story.tickers.first() {
            self.bus
                .update(cx, |bus, cx| bus.set_current(Some(first), cx));
        }
        cx.notify();
    }

    /// Shows `section` and moves the focus to its list.
    fn show(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.section = section;
        let focus = match section {
            Section::News => self.news.focus_handle(cx),
            Section::Markets => self.markets.focus_handle(cx),
            Section::Watchlist => self.watchlist.focus_handle(cx),
        };
        window.focus(&focus, cx);
        cx.notify();
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_next_story(&mut self, _: &NextStory, window: &mut Window, cx: &mut Context<Self>) {
        self.open_adjacent(1, window, cx);
    }

    fn on_previous_story(
        &mut self,
        _: &PreviousStory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_adjacent(-1, window, cx);
    }

    fn open_adjacent(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.section != Section::News {
            self.show(Section::News, window, cx);
        }
        self.news
            .update(cx, |monitor, cx| monitor.open_adjacent(step, cx));
    }

    fn on_mark_all_read(&mut self, _: &MarkAllRead, _: &mut Window, cx: &mut Context<Self>) {
        self.news
            .update(cx, |monitor, cx| monitor.mark_all_read(cx));
    }

    fn on_show_news(&mut self, _: &ShowNews, window: &mut Window, cx: &mut Context<Self>) {
        self.show(Section::News, window, cx);
    }

    fn on_show_markets(&mut self, _: &ShowMarkets, window: &mut Window, cx: &mut Context<Self>) {
        self.show(Section::Markets, window, cx);
    }

    fn on_show_watchlist(
        &mut self,
        _: &ShowWatchlist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show(Section::Watchlist, window, cx);
    }

    /// The mark, the SIMULATED badge, the tabs, the search box, the clock
    /// and "More from GigLabo".
    fn render_app_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .w_full()
            .h(APP_BAR_HEIGHT)
            .px(PAGE_PADDING)
            .gap(px(28.))
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(10.))
                    .child(div().size(px(12.)).rounded(px(3.)).bg(theme.primary))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("wire"),
                    )
                    .child(Tag::warning().outline().small().child(ui::SIMULATED)),
            )
            .child(
                div().h_full().flex().items_end().child(
                    TabBar::new("sections")
                        .underline()
                        .large()
                        .selected_index(self.section.ix())
                        .children(Section::ALL.map(|section| Tab::new().label(section.label())))
                        .on_click(cx.listener(|wire, ix: &usize, window, cx| {
                            if let Some(&section) = Section::ALL.get(*ix) {
                                wire.show(section, window, cx);
                            }
                        })),
                ),
            )
            .child(div().flex_1())
            .child(
                Input::new(&self.search)
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .aria_label("Search the news")
                    .w(SEARCH_WIDTH),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .font_features(ui::tabular_figures())
                    .child(SharedString::from(format!("{} UTC", self.status.clock))),
            )
            .child(ProductsButton::new())
    }

    /// The overview, the news monitor, and the story over the mentioned
    /// instruments.
    fn render_news(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .size_full()
            .items_start()
            .gap(GAP)
            .child(
                div()
                    .flex_none()
                    .w(OVERVIEW_WIDTH)
                    .h_full()
                    .child(self.render_overview(cx)),
            )
            .child(div().flex_1().min_w_0().h_full().child(self.news.clone()))
            .child(
                v_flex()
                    .flex_none()
                    .w(STORY_WIDTH)
                    .h_full()
                    .gap(GAP)
                    .child(div().flex_1().min_h_0().child(self.render_story(cx)))
                    .child(
                        div()
                            .flex_none()
                            .h(MENTIONED_CARD_HEIGHT)
                            .child(self.render_mentioned(cx)),
                    ),
            )
            .into_any_element()
    }

    fn render_overview(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        ui::card(cx)
            .size_full()
            .child(ui::card_header(
                "Market overview",
                Some("by sector · %Chg".into()),
                cx,
            ))
            .child(
                ui::grid_frame().child(
                    LiveGrid::new(&self.overview)
                        .with_size(Size::Size(markets::ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Market overview"),
                ),
            )
    }

    /// A list of quotes beside the current instrument's chart.
    fn render_quotes(
        &self,
        title: &'static str,
        detail: &'static str,
        grid: &Entity<LiveGridState<Quotes>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .size_full()
            .items_start()
            .gap(GAP)
            .child(
                ui::card(cx)
                    .flex_1()
                    .h_full()
                    .child(ui::card_header(title, Some(detail.into()), cx))
                    .child(
                        ui::grid_frame().child(
                            LiveGrid::new(grid)
                                .with_size(Size::Size(markets::ROW_HEIGHT))
                                .bordered(false)
                                .accessibility_label(title),
                        ),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .w(CHART_WIDTH)
                    .h_full()
                    .child(self.render_chart_card(cx)),
            )
            .into_any_element()
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let hints = KEYS.iter().filter_map(|(keystrokes, command)| {
            let keystroke = Keystroke::parse(keystrokes).ok()?;
            Some(
                h_flex()
                    .gap(px(6.))
                    .child(Kbd::new(keystroke).outline())
                    .child(command.hint()),
            )
        });
        StatusBar::new()
            .font_features(ui::tabular_figures())
            .left(Tag::warning().small().child(ui::SIMULATED))
            .left(SharedString::from(format!(
                "{} updates/s",
                thousands(self.status.rate)
            )))
            .left(SharedString::from(format!(
                "{} stories · {} unread",
                self.counts.stories(),
                self.counts.total_unread()
            )))
            .right(
                h_flex()
                    .gap(px(16.))
                    .text_color(theme.muted_foreground)
                    .children(hints),
            )
    }
}

/// `5000` as `5,000`.
fn thousands(value: u64) -> String {
    newsroom::thousands(value as i64)
}

impl Render for Wire {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, family, text_size) = (
            theme.background,
            theme.foreground,
            theme.font_family.clone(),
            theme.font_size,
        );
        let content = match self.section {
            Section::News => self.render_news(cx),
            Section::Markets => self.render_quotes(
                "Markets",
                "by sector · %Chg · index funds pinned",
                &self.markets.clone(),
                cx,
            ),
            Section::Watchlist => {
                self.render_quotes("Watchlist", "simulated quotes", &self.watchlist.clone(), cx)
            }
        };
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_next_story))
            .on_action(cx.listener(Self::on_previous_story))
            .on_action(cx.listener(Self::on_mark_all_read))
            .on_action(cx.listener(Self::on_show_news))
            .on_action(cx.listener(Self::on_show_markets))
            .on_action(cx.listener(Self::on_show_watchlist))
            .bg(background)
            .text_color(foreground)
            .font_family(family)
            .text_size(text_size)
            .child(self.render_app_bar(cx))
            .child(div().flex_1().min_h_0().p(PAGE_PADDING).child(content))
            .child(self.render_status(cx))
    }
}

fn main() {
    let settings = Settings::from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("WIRE_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        snapshot::run(settings, std::path::Path::new(&dir));
        #[cfg(not(feature = "test-support"))]
        err!(
            "WIRE_SNAPSHOT={}: needs --features test-support",
            dir.to_string_lossy()
        );
        return;
    }

    gpui_kit::application()
        // gpui-kit's icons (search, sort arrows): without them icons are blank.
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            setup(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                    None,
                    size(px(1600.), px(960.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let wire = cx.new(|cx| Wire::new(&settings, window, cx));
                // The keyboard starts in the news.
                let focus = wire.read(cx).news.focus_handle(cx);
                window.focus(&focus, cx);
                // Closing the window ends the simulation and the wire.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                wire
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}

/// gpui-kit, the grid's key bindings (arrows, Page, Home/End, Enter,
/// Escape), then `configure`.
fn setup(cx: &mut App) {
    gpui_kit::init(cx);
    gpui_kit_grid::init(cx);
    configure(cx);
}

/// After `gpui_kit::init` and `gpui_kit_grid::init`: the theme with its
/// font, and the wire's keys.
fn configure(cx: &mut App) {
    trading::theme::install::<WireTheme>(cx);
    WireTheme::Navy.apply(cx);
    cx.bind_keys(key_bindings());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keys_parse_and_name_each_command_once() {
        for (ix, (keystrokes, command)) in KEYS.iter().enumerate() {
            assert!(Keystroke::parse(keystrokes).is_ok(), "{keystrokes}");
            assert!(
                KEYS[ix + 1..]
                    .iter()
                    .all(|(other, again)| other != keystrokes && again != command),
                "{keystrokes}"
            );
        }
        for section in Section::ALL {
            assert!(
                KEYS.iter()
                    .any(|(_, command)| *command == Command::Show(section))
            );
        }
    }
}
