//! The broker app itself: the top bar, the watchlist, the instrument, the
//! account and the ticket, the positions, orders and movers, and the status
//! bar, over the simulator. The native `main` (`main.rs`) opens it in a
//! window; the browser build (`crates/grid-web`) in the page's canvas. Both
//! call `setup` once, then `open`.

use std::{sync::Arc, time::Duration};

use giglabo_products::ProductsButton;
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
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, Keystroke, ParentElement as _, Pixels,
    Render, SharedString, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{GridStore, LiveGrid, LiveGridEvent, LiveGridState, SortDirection};
// `std::time` natively; the browser's clock in wasm, where
// `std::time::{Instant, SystemTime}::now` panic.
use web_time::{Instant, SystemTime};

mod account;
mod colors;
mod instrument;
mod portfolio;
mod quotes;
#[cfg(feature = "test-support")]
pub mod snapshot;
mod theme;
mod ticket;
mod ui;

pub use self::theme::BrokerTheme;
use self::{
    account::{OrderStatus, money, signed_money},
    colors::PriceColors,
    instrument::InstrumentView,
    portfolio::{AccountDesk, Orders, Positions},
    quotes::QuoteList,
    ticket::Ticket,
    ui::{GAP, PAGE_PADDING},
};
use crate::trading::{
    bus::InstrumentBus,
    instruments::{self, InstrumentId},
    market::{self, ChartFeed, Control, Feeds},
    rows::{QuoteColumn, Side},
    theme::TradingTheme as _,
    worker,
};

gpui_kit::actions!(
    broker,
    [
        /// Moves the focus to the search box.
        FocusSearch,
        /// Moves the focus back to the watchlist.
        FocusWatchlist,
        /// Turns the ticket to Buy and puts the keyboard in it.
        Buy,
        /// Turns the ticket to Sell and puts the keyboard in it.
        Sell,
        /// Submits the ticket.
        Submit,
        /// Switches between the light and the dark theme.
        ToggleTheme,
        /// Switches the price colour convention.
        TogglePriceColors,
        /// Shows the Positions tab.
        ShowPositions,
        /// Shows the Orders tab.
        ShowOrders,
        /// Shows the Movers tab.
        ShowMovers,
    ]
);

/// The key context of the window content.
pub const KEY_CONTEXT: &str = "Broker";
/// The instrument open at start.
const FIRST_TICKER: &str = "AAPL";

/// The keys, as data: the bindings and the status bar's hints are both
/// made from this table. `secondary` is Cmd on macOS and Ctrl elsewhere.
/// Alt keeps the letters free for the search box and the ticket's fields,
/// and Enter for the grids.
const KEYS: [(&str, Command); 10] = [
    ("secondary-f", Command::FocusSearch),
    ("alt-w", Command::FocusWatchlist),
    ("alt-b", Command::Trade(Side::Buy)),
    ("alt-s", Command::Trade(Side::Sell)),
    ("alt-enter", Command::Submit),
    ("alt-t", Command::ToggleTheme),
    ("alt-c", Command::TogglePriceColors),
    ("secondary-1", Command::Show(BottomTab::Positions)),
    ("secondary-2", Command::Show(BottomTab::Orders)),
    ("secondary-3", Command::Show(BottomTab::Movers)),
];

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    FocusSearch,
    FocusWatchlist,
    Trade(Side),
    Submit,
    ToggleTheme,
    TogglePriceColors,
    Show(BottomTab),
}

impl Command {
    /// The binding of `keystrokes` to this command's action, in the
    /// broker's key context.
    fn binding(self, keystrokes: &str) -> KeyBinding {
        let context = Some(KEY_CONTEXT);
        match self {
            Self::FocusSearch => KeyBinding::new(keystrokes, FocusSearch, context),
            Self::FocusWatchlist => KeyBinding::new(keystrokes, FocusWatchlist, context),
            Self::Trade(Side::Buy) => KeyBinding::new(keystrokes, Buy, context),
            Self::Trade(Side::Sell) => KeyBinding::new(keystrokes, Sell, context),
            Self::Submit => KeyBinding::new(keystrokes, Submit, context),
            Self::ToggleTheme => KeyBinding::new(keystrokes, ToggleTheme, context),
            Self::TogglePriceColors => KeyBinding::new(keystrokes, TogglePriceColors, context),
            Self::Show(BottomTab::Positions) => KeyBinding::new(keystrokes, ShowPositions, context),
            Self::Show(BottomTab::Orders) => KeyBinding::new(keystrokes, ShowOrders, context),
            Self::Show(BottomTab::Movers) => KeyBinding::new(keystrokes, ShowMovers, context),
        }
    }

    /// The status bar's word for it.
    fn hint(self) -> &'static str {
        match self {
            Self::FocusSearch => "Search",
            Self::FocusWatchlist => "Watchlist",
            Self::Trade(side) => side.label(),
            Self::Submit => "Submit",
            Self::ToggleTheme => "Theme",
            Self::TogglePriceColors => "Colours",
            Self::Show(tab) => tab.label(),
        }
    }
}

/// The Option keys of `KEYS` as a browser on macOS delivers them. GPUI's web
/// platform takes a key from the character it types (`KeyboardEvent.key`),
/// and Option types one: Option-B arrives as `alt-∫` (U.S. layout), which
/// `alt-b` does not match. Natively the key is the letter, so these never
/// match there; elsewhere a browser delivers `alt-b` itself.
const BROWSER_OPTION_KEYS: [(&str, Command); 5] = [
    ("alt-∑", Command::FocusWatchlist),
    ("alt-∫", Command::Trade(Side::Buy)),
    ("alt-ß", Command::Trade(Side::Sell)),
    ("alt-†", Command::ToggleTheme),
    ("alt-ç", Command::TogglePriceColors),
];

/// Every binding of `KEYS` and `BROWSER_OPTION_KEYS`.
fn key_bindings() -> Vec<KeyBinding> {
    KEYS.iter()
        .chain(&BROWSER_OPTION_KEYS)
        .map(|(keystrokes, command)| command.binding(keystrokes))
        .collect()
}

/// The tabs of the bottom card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BottomTab {
    #[default]
    Positions,
    Orders,
    Movers,
}

impl BottomTab {
    pub const ALL: [Self; 3] = [Self::Positions, Self::Orders, Self::Movers];

    pub fn label(self) -> &'static str {
        match self {
            Self::Positions => "Positions",
            Self::Orders => "Orders",
            Self::Movers => "Movers",
        }
    }

    fn ix(self) -> usize {
        self as usize
    }
}

/// The watchlist's width.
const WATCHLIST_WIDTH: Pixels = px(380.);
/// The account and ticket column's width.
const SIDE_WIDTH: Pixels = px(340.);
/// The bottom card's height.
const BOTTOM_HEIGHT: Pixels = px(330.);
const TOP_BAR_HEIGHT: Pixels = px(64.);
const SEARCH_WIDTH: Pixels = px(300.);
/// The bottom grids' rows: roomy, a little shorter than the watchlist's.
const BOTTOM_ROW_HEIGHT: Pixels = px(36.);

/// Price updates per second unless `BROKER_RATE` says otherwise: half the
/// trading examples' default, about one a second per instrument, a calm
/// screen for a retail app.
const DEFAULT_RATE: u64 = market::DEFAULT_RATE / 2;

/// What the app starts with: natively from the environment (`from_env`),
/// in the browser from the page's address.
pub struct Settings {
    rate: u64,
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
        Self {
            rate: number("BROKER_RATE").unwrap_or(DEFAULT_RATE),
            seed: number("BROKER_SEED"),
            quit_after: number("BROKER_SECONDS"),
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

/// The app: the top bar, the cards and the status bar.
pub struct Broker {
    tab: BottomTab,
    bus: Entity<InstrumentBus>,
    control: Arc<Control>,
    search: Entity<InputState>,
    watchlist: Entity<LiveGridState<QuoteList>>,
    gainers: Entity<LiveGridState<QuoteList>>,
    losers: Entity<LiveGridState<QuoteList>>,
    positions: Entity<LiveGridState<Positions>>,
    orders: Entity<LiveGridState<Orders>>,
    instrument: Entity<InstrumentView<ChartFeed>>,
    desk: Entity<AccountDesk>,
    ticket: Entity<Ticket>,
    /// The watchlist's rows shown, once its first order is: what the
    /// search leaves.
    watch_rows: Option<usize>,
    status: Status,
    _tasks: [Task<()>; 4],
    _subscriptions: Vec<Subscription>,
}

impl Broker {
    fn new(settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let control = Control::new(settings.rate);
        let quotes = GridStore::new(quotes::store_options(), cx);
        let gainer_quotes = GridStore::new(quotes::store_options(), cx);
        let loser_quotes = GridStore::new(quotes::store_options(), cx);
        let holdings = GridStore::new(portfolio::store_options(), cx);
        let order_rows = GridStore::new(portfolio::store_options(), cx);
        let chart_feed = ChartFeed::default();

        let bus = cx.new(|cx| {
            let mut bus = InstrumentBus::new(control.clone());
            bus.set_current(instruments::find(FIRST_TICKER), cx);
            bus
        });
        let watchlist = quotes::watchlist(quotes.clone(), cx);
        let gainers = quotes::movers(gainer_quotes.clone(), SortDirection::Descending, cx);
        let losers = quotes::movers(loser_quotes.clone(), SortDirection::Ascending, cx);
        let positions = portfolio::positions(holdings.clone(), cx);
        let orders = portfolio::orders(order_rows.clone(), cx);
        let desk = cx
            .new(|cx| AccountDesk::new(quotes.clone(), holdings.writer(), order_rows.writer(), cx));
        let instrument =
            cx.new(|cx| InstrumentView::new(bus.clone(), quotes.clone(), chart_feed.clone(), cx));
        let ticket = cx.new(|cx| Ticket::new(bus.clone(), desk.clone(), window, cx));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search symbols"));

        let mut subscriptions = vec![
            cx.subscribe_in(&search, window, Self::on_search_event),
            cx.subscribe(&watchlist, Self::on_watchlist_event),
            // The account card and the tab counts follow the desk.
            cx.observe(&desk, |_, _, cx| cx.notify()),
        ];
        subscriptions.extend(quotes::follow_current(&watchlist, &bus, cx));
        subscriptions.extend(quotes::follow_current(&gainers, &bus, cx));
        subscriptions.extend(quotes::follow_current(&losers, &bus, cx));
        subscriptions.extend(quotes::follow_current(&positions, &bus, cx));

        // The simulator and the movers' relay, each a worker (natively a
        // thread of its own); each stops once its stores are gone.
        let feeds = Feeds::new()
            .with_quotes(quotes.writer())
            .with_chart(chart_feed);
        let (first_quotes, simulator) =
            worker::start_market(settings.seed, feeds, control.clone(), cx);
        let mut relay = quotes::MoversRelay::new(
            quotes.writer(),
            gainer_quotes.writer(),
            loser_quotes.writer(),
        );
        let movers = worker::repeat(quotes::MOVERS_INTERVAL, move || relay.step(), cx);

        // Once the first quotes are on screen, select the open instrument.
        let first = cx.spawn(async move |this, cx| {
            if first_quotes.recv().await.is_err() {
                return;
            }
            let Ok(flushed) = this.update(cx, |broker, cx| {
                broker.watchlist.update(cx, |grid, cx| grid.flush(cx))
            }) else {
                return;
            };
            if flushed.await.is_some() {
                let _ = this.update(cx, |broker, cx| {
                    let current = broker.bus.read(cx).current();
                    broker
                        .watchlist
                        .update(cx, |grid, cx| grid.select_key(current, cx));
                });
            }
        });

        Self {
            tab: BottomTab::default(),
            bus,
            control,
            search,
            watchlist,
            gainers,
            losers,
            positions,
            orders,
            instrument,
            desk,
            ticket,
            watch_rows: None,
            status: Status::default(),
            _tasks: [
                simulator,
                movers,
                first,
                Self::refresh(settings.quit_after, cx),
            ],
            _subscriptions: subscriptions,
        }
    }

    /// Every half second: the measured rate and the clock. With
    /// `BROKER_SECONDS=N` prints the status every second and quits after
    /// N seconds.
    fn refresh(quit_after: Option<u64>, cx: &mut Context<Self>) -> Task<()> {
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let (mut last_updates, mut ticks) = (0, 0u64);
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let Ok(done) = this.update(cx, |broker, cx| {
                    let updates = broker.control.updates();
                    broker.status = Status {
                        rate: (updates - last_updates) * 2,
                        clock: market::utc_time(SystemTime::now(), false).as_ref().into(),
                    };
                    last_updates = updates;
                    ticks += 1;
                    if quit_after.is_some() && ticks % 2 == 0 {
                        out!(
                            "{:>5.1}s  {}",
                            started.elapsed().as_secs_f64(),
                            broker.status_line(cx)
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

    /// The status as one line, for `BROKER_SECONDS`.
    fn status_line(&self, cx: &App) -> String {
        let summary = self.desk.read(cx).summary();
        format!(
            "SIMULATED · {} updates/s · total {} · day {} · {} UTC",
            account::grouped(self.status.rate as f64, 0),
            money(summary.total),
            signed_money(summary.day_pnl),
            self.status.clock,
        )
    }

    fn on_search_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let text = input.read(cx).value();
        self.watchlist
            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
    }

    fn on_watchlist_event(
        &mut self,
        _: Entity<LiveGridState<QuoteList>>,
        event: &LiveGridEvent<InstrumentId, QuoteColumn>,
        cx: &mut Context<Self>,
    ) {
        if let LiveGridEvent::ProjectionChange { row_count, .. } = event {
            self.watch_rows = Some(*row_count);
            cx.notify();
        }
    }

    /// Shows `tab` and moves the focus to its first grid.
    fn show(&mut self, tab: BottomTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        let focus = match tab {
            BottomTab::Positions => self.positions.focus_handle(cx),
            BottomTab::Orders => self.orders.focus_handle(cx),
            BottomTab::Movers => self.gainers.focus_handle(cx),
        };
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Makes `colors` the price colour convention and repaints: the grids
    /// take a change paint in it, the rest reads it when drawn.
    fn set_price_colors(&mut self, colors: PriceColors, cx: &mut Context<Self>) {
        colors.install(cx);
        for grid in [&self.watchlist, &self.gainers, &self.losers] {
            grid.update(cx, |grid, cx| {
                grid.set_change_paint(colors.change_paint(), cx)
            });
        }
        self.positions.update(cx, |grid, cx| {
            grid.set_change_paint(colors.change_paint(), cx)
        });
        cx.refresh_windows();
    }

    fn toggle_theme(cx: &mut App) {
        BrokerTheme::current(cx)
            .unwrap_or(BrokerTheme::Light)
            .toggled()
            .apply(cx);
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
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

    fn on_buy(&mut self, _: &Buy, window: &mut Window, cx: &mut Context<Self>) {
        self.ticket
            .update(cx, |ticket, cx| ticket.set_side(Side::Buy, window, cx));
    }

    fn on_sell(&mut self, _: &Sell, window: &mut Window, cx: &mut Context<Self>) {
        self.ticket
            .update(cx, |ticket, cx| ticket.set_side(Side::Sell, window, cx));
    }

    fn on_submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        self.ticket
            .update(cx, |ticket, cx| ticket.submit(window, cx));
    }

    fn on_toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        Self::toggle_theme(cx);
    }

    fn on_toggle_price_colors(
        &mut self,
        _: &TogglePriceColors,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_price_colors(PriceColors::current(cx).toggled(), cx);
    }

    fn on_show_positions(
        &mut self,
        _: &ShowPositions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show(BottomTab::Positions, window, cx);
    }

    fn on_show_orders(&mut self, _: &ShowOrders, window: &mut Window, cx: &mut Context<Self>) {
        self.show(BottomTab::Orders, window, cx);
    }

    fn on_show_movers(&mut self, _: &ShowMovers, window: &mut Window, cx: &mut Context<Self>) {
        self.show(BottomTab::Movers, window, cx);
    }

    /// The mark, the account's name, the search box, the price colours,
    /// the theme and "More from GigLabo".
    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let dark = theme.is_dark();
        h_flex()
            .flex_none()
            .w_full()
            .h(TOP_BAR_HEIGHT)
            .px(PAGE_PADDING)
            .gap(px(20.))
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(10.))
                    .child(
                        div()
                            .size(px(28.))
                            .rounded(px(9.))
                            .bg(theme.primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.primary_foreground)
                            .font_weight(FontWeight::BOLD)
                            .child("b"),
                    )
                    .child(
                        div()
                            .text_size(px(19.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("broker"),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(6.))
                    .rounded_full()
                    .bg(theme.muted)
                    .child(
                        Icon::new(IconName::CircleUser)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child(SharedString::from(format!(
                                "Individual · {}",
                                ui::SIMULATED
                            ))),
                    ),
            )
            .child(
                Input::new(&self.search)
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .aria_label("Search the watchlist")
                    .w(SEARCH_WIDTH),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.muted_foreground)
                            .child("Price colours"),
                    )
                    .child(
                        TabBar::new("price-colors")
                            .segmented()
                            .small()
                            .selected_index(PriceColors::current(cx).ix())
                            .children(
                                PriceColors::ALL.map(|colors| Tab::new().label(colors.label())),
                            )
                            .on_click(cx.listener(|broker, ix: &usize, _, cx| {
                                if let Some(&colors) = PriceColors::ALL.get(*ix) {
                                    broker.set_price_colors(colors, cx);
                                }
                            })),
                    ),
            )
            .child(
                Button::new("theme")
                    .ghost()
                    .icon(if dark { IconName::Sun } else { IconName::Moon })
                    .tooltip(if dark { "Light theme" } else { "Dark theme" })
                    .on_click(|_, _, cx| Self::toggle_theme(cx)),
            )
            .child(ProductsButton::new())
    }

    fn render_watchlist(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let query = self.watchlist.read(cx).quick_filter().clone();
        let shown = self.watch_rows.unwrap_or(quotes::WATCHLIST.len());
        let detail = if query.is_empty() {
            format!("{} symbols", quotes::WATCHLIST.len())
        } else {
            format!("{shown} of {}", quotes::WATCHLIST.len())
        };
        ui::card(cx)
            .size_full()
            .child(ui::card_header("Watchlist", Some(detail.into()), None, cx))
            .child(
                ui::grid_frame()
                    .child(
                        LiveGrid::new(&self.watchlist)
                            .large()
                            .bordered(false)
                            .accessibility_label("Watchlist"),
                    )
                    .when(self.watch_rows == Some(0), |frame| {
                        frame.child(ui::empty_state(
                            "No symbols match".into(),
                            format!("Nothing in the watchlist matches \"{query}\"").into(),
                            cx,
                        ))
                    }),
            )
    }

    fn render_instrument(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        ui::card(cx).size_full().child(self.instrument.clone())
    }

    /// The account's total value, the day's P&L, cash and buying power.
    fn render_account(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = PriceColors::current(cx);
        let summary = self.desk.read(cx).summary();
        let day_color = colors.of(summary.day_pnl, theme);
        ui::card(cx)
            .flex_none()
            .w_full()
            .p(ui::CARD_PADDING)
            .gap(px(12.))
            .font_features(ui::tabular_figures())
            .child(
                v_flex()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.muted_foreground)
                            .child("Total value"),
                    )
                    .child(
                        div()
                            .text_size(px(30.))
                            .line_height(px(36.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child(SharedString::from(money(summary.total))),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .text_size(px(14.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(day_color)
                            .child(SharedString::from(format!(
                                "{} {}",
                                ui::arrow(summary.day_pnl),
                                signed_money(summary.day_pnl)
                            )))
                            .child(SharedString::from(format!(
                                "({})",
                                ui::signed_pct(summary.day_pnl_pct())
                            )))
                            .child(
                                div()
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(theme.muted_foreground)
                                    .child("today"),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(div().flex_1().child(ui::stat(
                        "Cash",
                        money(summary.cash).into(),
                        None,
                        cx,
                    )))
                    .child(div().flex_1().child(ui::stat(
                        "Buying power",
                        money(summary.buying_power).into(),
                        None,
                        cx,
                    ))),
            )
    }

    /// The bottom card: Positions, Orders and Movers.
    fn render_bottom(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let desk = self.desk.read(cx);
        let holdings = self.positions.read(cx).projection().row_count();
        let orders = desk.account().orders();
        let working = orders.iter().filter(|order| order.is_working()).count();
        let filled = orders
            .iter()
            .filter(|order| matches!(order.status, OrderStatus::Filled { .. }))
            .count();
        let detail: SharedString = match self.tab {
            BottomTab::Positions => format!("{holdings} holdings · simulated prices").into(),
            BottomTab::Orders => format!("{working} working · {filled} filled").into(),
            BottomTab::Movers => "by Chg % in the simulated session".into(),
        };
        let no_orders = orders.is_empty();
        let tabs = TabBar::new("bottom-tabs")
            .underline()
            .selected_index(self.tab.ix())
            .children(BottomTab::ALL.map(|tab| Tab::new().label(tab.label())))
            .on_click(cx.listener(|broker, ix: &usize, window, cx| {
                if let Some(&tab) = BottomTab::ALL.get(*ix) {
                    broker.show(tab, window, cx);
                }
            }));
        let content: AnyElement = match self.tab {
            BottomTab::Positions => ui::grid_frame()
                .child(
                    LiveGrid::new(&self.positions)
                        .with_size(Size::Size(BOTTOM_ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Positions"),
                )
                .into_any_element(),
            BottomTab::Orders => ui::grid_frame()
                .child(
                    LiveGrid::new(&self.orders)
                        .with_size(Size::Size(BOTTOM_ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Orders"),
                )
                .when(no_orders, |frame| {
                    frame.child(ui::empty_state(
                        "No orders yet".into(),
                        "Orders you place with the ticket show here".into(),
                        cx,
                    ))
                })
                .into_any_element(),
            BottomTab::Movers => h_flex()
                .flex_1()
                .min_h_0()
                .gap(GAP)
                .px(ui::GRID_INSET)
                .pb(ui::GRID_INSET)
                .child(self.render_movers("Top gainers", &self.gainers.clone(), cx))
                .child(self.render_movers("Top losers", &self.losers.clone(), cx))
                .into_any_element(),
        };
        ui::card(cx)
            .size_full()
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .h(ui::HEADER_HEIGHT)
                    .px(ui::CARD_PADDING)
                    .gap(px(16.))
                    .child(div().h_full().flex().items_end().child(tabs))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    ),
            )
            .child(content)
    }

    fn render_movers(
        &self,
        title: &'static str,
        grid: &Entity<LiveGridState<QuoteList>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .flex_none()
                    .px(px(6.))
                    .py(px(6.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            )
            .child(
                div().flex_1().min_h_0().child(
                    LiveGrid::new(grid)
                        .with_size(Size::Size(BOTTOM_ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label(title),
                ),
            )
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
            .left(Tag::secondary().small().rounded_full().child(ui::SIMULATED))
            .left(SharedString::from(format!(
                "Simulated prices · {} updates/s · {} UTC",
                account::grouped(self.status.rate as f64, 0),
                self.status.clock
            )))
            .right(
                h_flex()
                    .gap(px(14.))
                    .text_color(theme.muted_foreground)
                    .children(hints),
            )
    }
}

impl Render for Broker {
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
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_focus_watchlist))
            .on_action(cx.listener(Self::on_buy))
            .on_action(cx.listener(Self::on_sell))
            .on_action(cx.listener(Self::on_submit))
            .on_action(cx.listener(Self::on_toggle_theme))
            .on_action(cx.listener(Self::on_toggle_price_colors))
            .on_action(cx.listener(Self::on_show_positions))
            .on_action(cx.listener(Self::on_show_orders))
            .on_action(cx.listener(Self::on_show_movers))
            .bg(background)
            .text_color(foreground)
            .font_family(family)
            .text_size(text_size)
            .child(self.render_top_bar(cx))
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
                            .flex_none()
                            .w(WATCHLIST_WIDTH)
                            .h_full()
                            .child(self.render_watchlist(cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .gap(GAP)
                            .child(div().flex_1().min_h_0().child(self.render_instrument(cx)))
                            .child(
                                div()
                                    .flex_none()
                                    .h(BOTTOM_HEIGHT)
                                    .child(self.render_bottom(cx)),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_none()
                            .w(SIDE_WIDTH)
                            .h_full()
                            .gap(GAP)
                            .child(self.render_account(cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .child(ui::card(cx).size_full().child(self.ticket.clone())),
                            ),
                    ),
            )
            .child(self.render_status(cx))
    }
}

/// gpui-kit, the grid's key bindings (arrows, Page, Home/End, Enter,
/// Escape), then `configure`. Call once, before `open`.
pub fn setup(cx: &mut App) {
    gpui_kit::init(cx);
    gpui_kit_grid::init(cx);
    configure(cx);
}

/// After `gpui_kit::init` and `gpui_kit_grid::init`: the themes with their
/// font, green up, and the broker's keys.
fn configure(cx: &mut App) {
    crate::trading::theme::install::<BrokerTheme>(cx);
    BrokerTheme::Light.apply(cx);
    PriceColors::GreenUp.install(cx);
    cx.bind_keys(key_bindings());
}

/// The broker in `window`: the simulator started, the keyboard in the
/// watchlist.
pub fn open(settings: &Settings, window: &mut Window, cx: &mut App) -> Entity<Broker> {
    let broker = cx.new(|cx| Broker::new(settings, window, cx));
    let focus = broker.read(cx).watchlist.focus_handle(cx);
    window.focus(&focus, cx);
    broker
}

/// The browser build's side (`crates/grid-web`): the page's settings, and
/// what its scripts read and set.
#[cfg(target_family = "wasm")]
impl Settings {
    /// The page's `rate` and `seed`, the defaults otherwise.
    pub fn new(rate: Option<u64>, seed: Option<u64>) -> Self {
        Self {
            rate: rate.unwrap_or(DEFAULT_RATE),
            seed,
            quit_after: None,
        }
    }
}

#[cfg(target_family = "wasm")]
impl Broker {
    /// The simulator's rate and counters.
    pub fn control(&self) -> &Arc<Control> {
        &self.control
    }

    /// The watchlist's grid.
    pub fn watchlist_grid(&self) -> &Entity<LiveGridState<QuoteList>> {
        &self.watchlist
    }

    /// The open instrument, the theme, the price colours, the bottom tab,
    /// the search and the rows it leaves, the orders (oldest first), the
    /// ticket's order type and the grid or box with the keyboard, for the
    /// page's input checks.
    pub fn page_state(&self, window: &Window, cx: &App) -> serde_json::Value {
        let grids = [
            ("search", self.search.focus_handle(cx)),
            ("watchlist", self.watchlist.focus_handle(cx)),
            ("positions", self.positions.focus_handle(cx)),
            ("orders", self.orders.focus_handle(cx)),
            ("gainers", self.gainers.focus_handle(cx)),
            ("losers", self.losers.focus_handle(cx)),
        ];
        let focus = grids
            .iter()
            .find(|(_, handle)| handle.contains_focused(window, cx))
            .map(|(name, _)| *name);
        let orders: Vec<serde_json::Value> = self
            .desk
            .read(cx)
            .account()
            .orders()
            .iter()
            .map(|order| {
                serde_json::json!({
                    "ticker": order.instrument.get().ticker,
                    "side": order.side.label(),
                    "quantity": order.quantity,
                    "working": order.is_working(),
                })
            })
            .collect();
        serde_json::json!({
            "current": self.bus.read(cx).current().map(|id| id.get().ticker),
            "theme": BrokerTheme::current(cx).map(BrokerTheme::label),
            "colors": PriceColors::current(cx).label(),
            "tab": self.tab.label(),
            "search": self.search.read(cx).value().as_ref(),
            "shown": self.watch_rows,
            "orders": orders,
            "order_type": self.ticket.read(cx).order_type().label(),
            "focus": focus,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_option_keys_parse_and_repeat_alt_keys() {
        for (keystrokes, command) in BROWSER_OPTION_KEYS {
            assert!(Keystroke::parse(keystrokes).is_ok(), "{keystrokes}");
            assert!(
                KEYS.iter()
                    .any(|(native, again)| *again == command && native.starts_with("alt-")),
                "{keystrokes}"
            );
        }
    }

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
        for tab in BottomTab::ALL {
            assert!(
                KEYS.iter()
                    .any(|(_, command)| *command == Command::Show(tab))
            );
        }
        for side in [Side::Buy, Side::Sell] {
            assert!(
                KEYS.iter()
                    .any(|(_, command)| *command == Command::Trade(side))
            );
        }
    }
}
