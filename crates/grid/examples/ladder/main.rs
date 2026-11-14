//! A futures price ladder: one-click simulated orders on a vertical
//! depth-of-market grid, with the time and sales, a chart, the working
//! orders, the fills and the position, in a dark grey engineering theme.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: the theme's fonts
//! cargo run --release -p gpui-kit-grid --example ladder
//! LADDER_CONTRACT=GC LADDER_RATE=40 cargo run --release -p gpui-kit-grid --example ladder
//! # No window: the ladder drawn offscreen to PNG files (`snapshot.rs`)
//! LADDER_SNAPSHOT=target/ladder cargo run --release -p gpui-kit-grid --features test-support --example ladder
//! ```
//!
//! **Everything is simulated.** The contracts are real index and commodity
//! futures with their own tick sizes and tick values
//! (`trading/instruments.rs`), but their prices start from approximate
//! seed levels typed in from memory; the book, the trades, the fills and
//! the P&L are made up by the simulated exchange (`exchange.rs`). The top
//! bar and the status bar say SIMULATED.
//!
//! The ladder in the middle has one row per price, highest on top: the
//! trader's working buys, the bid sizes, the price, the ask sizes, the
//! working sells and the volume traded at the price this session, shaded
//! by its share of the most traded price (the point of control, in
//! yellow). The rows stay where they are while sizes change; the last
//! trade's price is inverted, the best bid and ask solid. A left click in
//! Buy or Sell places a limit order there for the selected quantity, a
//! right click there cancels it. Orders fill when the simulated market
//! trades through their price, or at it once the size that rested ahead of
//! them has traded. The ladder recentres by itself when the last price
//! leaves it, unless it was scrolled away on purpose.
//!
//! Keys (`commands.rs`): Shift+B buy at the market, Shift+S sell at the
//! market, Shift+X cancel every working order, `[` and `]` the order
//! quantity, C recentre. The bar on top picks the contract.
//!
//! The theme is a gpui-kit theme file (`themes/dark.json`); its fonts
//! (Inter, JetBrains Mono) are downloaded by `script/fetch-example-fonts`
//! into `examples/fonts/` (not committed). Without them the ladder runs
//! with system fonts and says so once on stderr.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `LADDER_CONTRACT` | `ES` | The contract at start, by root or symbol |
//! | `LADDER_RATE` | 8 | Simulated trades per second of each contract |
//! | `LADDER_SEED` | none | A fixed seed: the same simulated market every run |
//! | `LADDER_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use giglabo_products::ProductsButton;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, Size,
    button::{Button, ButtonGroup, ButtonVariants as _},
    kbd::Kbd,
    status_bar::StatusBar,
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, Focusable as _, FontFeatures, Hsla,
    InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, Pixels, Render,
    SharedString, Styled as _, Subscription, Task, Window, WindowBounds, WindowOptions, div,
    prelude::FluentBuilder as _, px, size,
};
use gpui_kit_grid::{
    GridStore, GridTarget, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState,
};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

mod commands;
mod dom;
mod exchange;
mod rows;
#[cfg(feature = "test-support")]
mod snapshot;
mod tables;
mod theme;

use crate::{
    commands::Command,
    dom::Ladder,
    exchange::{ExchangeLink, Outputs, Request, Status},
    rows::LadderColumn,
    tables::{Fills, Orders, Tape},
    theme::LadderTheme,
    trading::{
        chart::ChartPanel,
        instruments::{ContractId, tick_decimals},
        market::utc_time,
        rows::Side,
        theme::TradingTheme as _,
    },
};

/// The key context of the window content.
const KEY_CONTEXT: &str = "Ladder";
/// The order quantities the selector offers, in contracts.
const QUANTITIES: [i64; 6] = [1, 2, 5, 10, 25, 50];
/// The row height of the grids: a dense ladder.
const ROW_HEIGHT: Pixels = px(20.);
/// How often the window reads the exchange's status.
const POLL: Duration = Duration::from_millis(100);
/// The chart's EMA, in candles.
const EMA_PERIOD: usize = 20;
/// The widths of the side columns: the time and sales on the left, the
/// ladder in the middle.
const TAPE_WIDTH: Pixels = px(310.);
const LADDER_WIDTH: Pixels = px(456.);
/// The height of the working orders and the fills under the chart.
const BLOTTER_HEIGHT: Pixels = px(280.);

/// What the environment sets.
struct Settings {
    rate: f64,
    seed: Option<u64>,
    contract: ContractId,
}

impl Settings {
    fn from_env() -> Self {
        let number = |name: &str| {
            std::env::var(name).ok().and_then(|value| {
                value
                    .parse::<f64>()
                    .inspect_err(|_| err!("{name}={value}: not a number; ignored"))
                    .ok()
            })
        };
        let first = ContractId::find("ES").expect("ES is a contract");
        let contract = std::env::var("LADDER_CONTRACT")
            .ok()
            .map_or(first, |value| {
                ContractId::find(&value).unwrap_or_else(|| {
                    err!("LADDER_CONTRACT={value}: not a contract; ES is used");
                    first
                })
            });
        Self {
            rate: number("LADDER_RATE").unwrap_or(exchange::DEFAULT_RATE),
            seed: number("LADDER_SEED").map(|seed| seed as u64),
            contract,
        }
    }
}

/// The window: the contract bar on top; the time and sales, the ladder
/// with its order entry, and the chart over the working orders and the
/// fills; the status bar below.
struct Screen {
    link: ExchangeLink,
    rate: f64,
    /// The contract on the ladder, as asked for.
    contract: ContractId,
    quantity: i64,
    ladder: Entity<LiveGridState<Ladder>>,
    tape: Entity<LiveGridState<Tape>>,
    orders: Entity<LiveGridState<Orders>>,
    fills: Entity<LiveGridState<Fills>>,
    chart: Entity<ChartPanel>,
    status: Status,
    /// The ladder centres the last price as soon as it can: after a new
    /// contract or band of rows, or `C`.
    center_pending: bool,
    /// The last price was in view at the last look, and at which tick: it
    /// leaving the view recentres the ladder, the ladder scrolled away
    /// from it does not.
    last_in_view: bool,
    followed: i64,
    clock: SharedString,
    _poll: Task<()>,
    _subscriptions: [Subscription; 1],
}

impl Screen {
    fn new(settings: &Settings, cx: &mut Context<Self>) -> Self {
        let ladder = GridStore::new(dom::store_options(), cx);
        let tape = GridStore::new(tables::tape_options(), cx);
        let orders = GridStore::new(tables::order_options(), cx);
        let fills = GridStore::new(tables::fill_options(), cx);
        let link = exchange::start(
            settings.seed,
            settings.rate,
            settings.contract,
            Outputs {
                ladder: ladder.writer(),
                tape: tape.writer(),
                orders: orders.writer(),
                fills: fills.writer(),
            },
        );
        let ladder = cx.new(|cx| dom::grid(ladder, cx));
        let tape = cx.new(|cx| tables::tape(tape, cx));
        let orders = cx.new(|cx| tables::orders(orders, cx));
        let fills = cx.new(|cx| tables::fills(fills, cx));
        let chart = cx.new(|cx| ChartPanel::new(link.clone(), cx).with_ema(EMA_PERIOD));
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |screen, cx| screen.pull(cx)).is_err() {
                    break;
                }
            }
        });
        let subscriptions = [cx.subscribe(&ladder, Self::on_ladder_event)];
        Self {
            link,
            rate: settings.rate,
            contract: settings.contract,
            quantity: QUANTITIES[0],
            ladder,
            tape,
            orders,
            fills,
            chart,
            status: Status::default(),
            center_pending: true,
            last_in_view: false,
            followed: 0,
            clock: SharedString::default(),
            _poll: poll,
            _subscriptions: subscriptions,
        }
    }

    /// A click in the ladder's order columns places or cancels; the first
    /// rows of a new band centre the last price.
    fn on_ladder_event(
        &mut self,
        _: Entity<LiveGridState<Ladder>>,
        event: &LiveGridEvent<i64, LadderColumn>,
        cx: &mut Context<Self>,
    ) {
        match event {
            LiveGridEvent::Click {
                target: GridTarget::Cell { key, column },
                button,
                ..
            } => {
                let request =
                    dom::click_request(self.contract, *key, *column, *button, self.quantity);
                if let Some(request) = request {
                    self.link.send(request);
                }
            }
            LiveGridEvent::ProjectionChange { .. } if self.center_pending => self.try_center(cx),
            _ => {}
        }
    }

    /// Reads the exchange's status: a new band of rows centres the last
    /// price, the last price leaving the view recentres it.
    fn pull(&mut self, cx: &mut Context<Self>) {
        let status = self.link.status();
        let clock = SharedString::from(utc_time(SystemTime::now(), false).to_string());
        if status.version == self.status.version && clock == self.clock {
            return;
        }
        if status.band != self.status.band {
            self.center_pending = true;
        }
        self.status = status;
        self.clock = clock;
        if self.center_pending {
            self.try_center(cx);
        } else {
            self.follow(cx);
        }
        cx.notify();
    }

    fn try_center(&mut self, cx: &mut Context<Self>) {
        if self.status.contract != Some(self.contract) {
            return;
        }
        let last = self.status.last;
        if self
            .ladder
            .update(cx, |grid, cx| dom::center(grid, last, cx))
        {
            self.center_pending = false;
            self.last_in_view = true;
            self.followed = last;
        }
    }

    /// Recentres when the last price moved out of view since the last
    /// look; leaves a ladder scrolled away from it alone.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let last = self.status.last;
        let moved = last != self.followed;
        self.followed = last;
        match dom::in_view(self.ladder.read(cx), last) {
            Some(true) => self.last_in_view = true,
            Some(false) if self.last_in_view && moved => {
                self.center_pending = true;
                self.try_center(cx);
            }
            Some(false) => self.last_in_view = false,
            None => {}
        }
    }

    /// Shows `id` on the ladder, the time and sales and the chart.
    fn select(&mut self, id: ContractId, cx: &mut Context<Self>) {
        if id == self.contract {
            return;
        }
        self.contract = id;
        self.center_pending = true;
        self.link.send(Request::Select(id));
        cx.notify();
    }

    /// Runs `command`, from its key or its button.
    fn run(&mut self, command: Command, cx: &mut Context<Self>) {
        let market = |side| Request::Market {
            contract: self.contract,
            side,
            quantity: self.quantity,
        };
        match command {
            Command::Recentre => {
                self.center_pending = true;
                self.try_center(cx);
            }
            Command::QuantityUp => {
                self.quantity = QUANTITIES
                    .into_iter()
                    .find(|quantity| *quantity > self.quantity)
                    .unwrap_or(self.quantity);
            }
            Command::QuantityDown => {
                self.quantity = QUANTITIES
                    .into_iter()
                    .rev()
                    .find(|quantity| *quantity < self.quantity)
                    .unwrap_or(self.quantity);
            }
            Command::BuyMarket => self.link.send(market(Side::Buy)),
            Command::SellMarket => self.link.send(market(Side::Sell)),
            Command::CancelAll => self.link.send(Request::CancelAll),
        }
        cx.notify();
    }

    fn render_top_bar(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let contract = self.contract.get();
        let quote = (self.status.contract == Some(self.contract)).then(|| {
            let decimals = tick_decimals(contract.tick);
            let price = |tick: i64| format!("{:.*}", decimals, contract.price(tick));
            (
                price(self.status.last),
                price(self.status.bid),
                price(self.status.ask),
            )
        });
        h_flex()
            .flex_none()
            .w_full()
            .h(px(38.))
            .px_2()
            .gap_3()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(simulated(theme.warning, theme.warning_foreground))
            .child(
                ButtonGroup::new("contracts")
                    .small()
                    .outline()
                    .children(ContractId::all().map(|id| {
                        let symbol = id.get().symbol;
                        Button::new(symbol)
                            .label(symbol)
                            .selected(id == self.contract)
                    }))
                    .on_click(cx.listener(|screen, clicked: &Vec<usize>, _, cx| {
                        if let Some(ix) = clicked.first() {
                            screen.select(ContractId(*ix as u8), cx);
                        }
                    })),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(format!(
                        "{} · {} · tick {} = ${:.2}",
                        contract.name,
                        contract.month,
                        format_args!("{:.*}", tick_decimals(contract.tick), contract.tick),
                        contract.tick_value,
                    ))),
            )
            .child(div().flex_1())
            .when_some(quote, |bar, (last, bid, ask)| {
                bar.child(
                    h_flex()
                        .flex_none()
                        .gap_3()
                        .font_family(theme.mono_font_family.clone())
                        .child(labelled("Bid", bid, theme.success, theme.muted_foreground))
                        .child(labelled(
                            "Last",
                            last,
                            theme.foreground,
                            theme.muted_foreground,
                        ))
                        .child(labelled("Ask", ask, theme.danger, theme.muted_foreground)),
                )
            })
            .child(ProductsButton::new())
    }

    /// The button of `command`.
    fn command_button(command: Command, cx: &Context<Self>) -> Button {
        Button::new(command.label())
            .label(command.label())
            .on_click(cx.listener(move |screen, _, _, cx| screen.run(command, cx)))
    }

    /// The order entry over the ladder: the quantity and the market
    /// orders, then the position in the contract.
    fn render_order_entry(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let button =
            |command: Command, cx: &Context<Self>| Self::command_button(command, cx).small();
        let contract = self.contract.get();
        let holding = self
            .status
            .holdings
            .get(self.contract.0 as usize)
            .copied()
            .unwrap_or_default();
        let position = holding.position;
        let open = position.open_pnl(contract, holding.last);
        let average = if position.quantity == 0 {
            "flat".to_owned()
        } else {
            format!(
                "@ {:.*}",
                tick_decimals(contract.tick) + 1,
                position.average * contract.tick
            )
        };
        let signed = |value: f64| {
            if value >= 0. {
                theme.chart_bullish
            } else {
                theme.chart_bearish
            }
        };
        let quantity_color = match position.quantity {
            0 => theme.muted_foreground,
            quantity if quantity > 0 => theme.blue,
            _ => theme.magenta,
        };
        v_flex()
            .flex_none()
            .w_full()
            .gap_1()
            .p_2()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_color(theme.muted_foreground).child("Qty"))
                    .child(
                        ButtonGroup::new("quantity")
                            .small()
                            .outline()
                            .children(QUANTITIES.map(|quantity| {
                                Button::new(("quantity", quantity as usize))
                                    .label(SharedString::from(quantity.to_string()))
                                    .selected(quantity == self.quantity)
                            }))
                            .on_click(cx.listener(|screen, clicked: &Vec<usize>, _, cx| {
                                if let Some(ix) = clicked.first() {
                                    screen.quantity = QUANTITIES[*ix];
                                    cx.notify();
                                }
                            })),
                    )
                    .child(div().flex_1())
                    .child(button(Command::BuyMarket, cx).success())
                    .child(button(Command::SellMarket, cx).danger()),
            )
            .child(
                h_flex()
                    .gap_3()
                    .font_family(theme.mono_font_family.clone())
                    .child(
                        div()
                            .text_color(quantity_color)
                            .child(SharedString::from(format!("Pos {:+}", position.quantity))),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(average)),
                    )
                    .child(labelled(
                        "Open",
                        money(open),
                        signed(open),
                        theme.muted_foreground,
                    ))
                    .child(labelled(
                        "Real",
                        money(position.realized),
                        signed(position.realized),
                        theme.muted_foreground,
                    )),
            )
    }

    fn render_status(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (open, realized) = self.status.total_pnl();
        let total = open + realized;
        StatusBar::new()
            .font_family(theme.mono_font_family.clone())
            .left(simulated(theme.warning, theme.warning_foreground))
            .left(SharedString::from(format!(
                "{} trades/s a contract",
                self.rate
            )))
            .left(h_flex().gap_3().children(Command::ALL.map(|command| {
                h_flex()
                    .gap_1()
                    .when_some(Keystroke::parse(command.key()).ok(), |legend, key| {
                        legend.child(Kbd::new(key))
                    })
                    .child(command.label())
            })))
            .right(SharedString::from(format!(
                "{} working",
                self.status.working
            )))
            .right(labelled(
                "P&L",
                money(total),
                if total >= 0. {
                    theme.chart_bullish
                } else {
                    theme.chart_bearish
                },
                theme.muted_foreground,
            ))
            .right(
                div()
                    .text_color(theme.foreground)
                    .child(SharedString::from(format!("{} UTC", self.clock))),
            )
    }
}

/// The SIMULATED badge.
fn simulated(background: Hsla, foreground: Hsla) -> impl IntoElement {
    div()
        .flex_none()
        .px_1()
        .bg(background)
        .text_color(foreground)
        .child("SIMULATED")
}

/// `label value`, the label quiet.
fn labelled(
    label: &'static str,
    value: impl Into<SharedString>,
    color: Hsla,
    muted: Hsla,
) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .child(div().text_color(muted).child(label))
        .child(div().text_color(color).child(value.into()))
}

/// `1234.5` as `+$1,234.50`, `-12` as `-$12.00`.
fn money(value: f64) -> String {
    let cents = (value.abs() * 100.).round() as u64;
    let sign = match cents {
        0 => "",
        _ if value < 0. => "-",
        _ => "+",
    };
    let digits = (cents / 100).to_string();
    let mut dollars = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix) % 3 == 0 {
            dollars.push(',');
        }
        dollars.push(digit);
    }
    format!("{sign}${dollars}.{:02}", cents % 100)
}

/// A titled pane: a quiet title strip, with `tools` at its right end, over
/// `body`.
fn pane(
    title: impl Into<SharedString>,
    tools: Option<gpui_kit::AnyElement>,
    body: impl IntoElement,
    cx: &App,
) -> gpui_kit::Div {
    let theme = cx.theme();
    v_flex()
        .min_w_0()
        .min_h_0()
        .child(
            h_flex()
                .flex_none()
                .h(px(26.))
                .pl_2()
                .gap_1()
                .bg(theme.tab_bar)
                .border_b_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_1().min_w_0().child(title.into()))
                .children(tools),
        )
        .child(div().flex_1().min_h_0().child(body))
}

/// A grid in the theme's monospace family and size, with a slashed zero
/// (OpenType `zero`; the grid adds tabular figures), dense rows and no
/// frame.
fn grid<D: LiveGridDelegate>(
    state: &Entity<LiveGridState<D>>,
    label: &'static str,
    cx: &App,
) -> gpui_kit::Div {
    let theme = cx.theme();
    div()
        .size_full()
        .font_family(theme.mono_font_family.clone())
        .text_size(theme.mono_font_size)
        .font_features(FontFeatures(Arc::new(vec![("zero".into(), 1)])))
        .child(
            LiveGrid::new(state)
                .with_size(Size::Size(ROW_HEIGHT))
                .bordered(false)
                .accessibility_label(label),
        )
}

impl Render for Screen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let symbol = self.contract.get().symbol;
        let tape = grid(&self.tape, "Time and sales", cx);
        let ladder = grid(&self.ladder, "Price ladder", cx);
        let orders = grid(&self.orders, "Working orders", cx);
        let fills = grid(&self.fills, "Fills", cx);
        // Over the ladder, the commands that act on it.
        let ladder_tools = h_flex()
            .child(
                Self::command_button(Command::CancelAll, cx)
                    .xsmall()
                    .ghost(),
            )
            .child(Self::command_button(Command::Recentre, cx).xsmall().ghost())
            .into_any_element();
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|screen, _: &commands::Recentre, _, cx| {
                screen.run(Command::Recentre, cx)
            }))
            .on_action(cx.listener(|screen, _: &commands::QuantityUp, _, cx| {
                screen.run(Command::QuantityUp, cx)
            }))
            .on_action(cx.listener(|screen, _: &commands::QuantityDown, _, cx| {
                screen.run(Command::QuantityDown, cx)
            }))
            .on_action(cx.listener(|screen, _: &commands::BuyMarket, _, cx| {
                screen.run(Command::BuyMarket, cx)
            }))
            .on_action(cx.listener(|screen, _: &commands::SellMarket, _, cx| {
                screen.run(Command::SellMarket, cx)
            }))
            .on_action(cx.listener(|screen, _: &commands::CancelAll, _, cx| {
                screen.run(Command::CancelAll, cx)
            }))
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_sm()
            .child(self.render_top_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        pane(format!("Time & Sales · {symbol}"), None, tape, cx)
                            .flex_none()
                            .w(TAPE_WIDTH)
                            .h_full()
                            .border_r_1()
                            .border_color(theme.border),
                    )
                    .child(
                        v_flex()
                            .flex_none()
                            .w(LADDER_WIDTH)
                            .h_full()
                            .border_r_1()
                            .border_color(theme.border)
                            .child(self.render_order_entry(cx))
                            .child(
                                pane(
                                    format!("Price ladder · {symbol}"),
                                    Some(ladder_tools),
                                    ladder,
                                    cx,
                                )
                                .flex_1()
                                .w_full(),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(div().flex_1().min_h_0().w_full().child(self.chart.clone()))
                            .child(
                                h_flex()
                                    .flex_none()
                                    .h(BLOTTER_HEIGHT)
                                    .w_full()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .child(
                                        pane("Working orders", None, orders, cx)
                                            .flex_1()
                                            .h_full()
                                            .border_r_1()
                                            .border_color(theme.border),
                                    )
                                    .child(pane("Fills", None, fills, cx).flex_1().h_full()),
                            ),
                    ),
            )
            .child(self.render_status(cx))
    }
}

fn main() {
    let settings = Settings::from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("LADDER_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        snapshot::run(settings, std::path::Path::new(&dir));
        #[cfg(not(feature = "test-support"))]
        err!(
            "LADDER_SNAPSHOT={}: needs --features test-support",
            dir.to_string_lossy()
        );
        return;
    }

    gpui_kit::application()
        // gpui-kit's icons (sort arrows, menu checks): without them icons are blank.
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            // The grid's key bindings: arrows, Page, Home/End, Enter, Escape.
            gpui_kit_grid::init(cx);
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
                let screen = cx.new(|cx| Screen::new(&settings, cx));
                // The keyboard starts on the ladder.
                let focus = screen.read(cx).ladder.focus_handle(cx);
                window.focus(&focus, cx);
                // Closing the window ends the simulation and the ladder.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                screen
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}

/// The theme with its fonts, and the keys: the window's and the
/// snapshot's alike.
fn setup(cx: &mut App) {
    trading::theme::install::<LadderTheme>(cx);
    LadderTheme::Dark.apply(cx);
    // Keys are the example's own, under its key context.
    cx.bind_keys(commands::bindings(KEY_CONTEXT));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_money_has_a_sign_and_thousands() {
        assert_eq!(money(1234.5), "+$1,234.50");
        assert_eq!(money(-12.), "-$12.00");
        assert_eq!(money(0.001), "$0.00");
        assert_eq!(money(-1_000_000.), "-$1,000,000.00");
    }
}
