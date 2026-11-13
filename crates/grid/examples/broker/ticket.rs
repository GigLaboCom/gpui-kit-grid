//! The order ticket: Buy or Sell, Market or Limit, the number of shares
//! with a stepper and a limit price; the estimated cost or proceeds and the
//! buying power; Submit places a simulated order (`AccountDesk::place`),
//! which fills against the simulated price. Buy takes the colour of a rise
//! and Sell that of a fall (`PriceColors`).

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonRounded, ButtonVariant, ButtonVariants as _},
    input::{InputEvent, InputState, NumberInput},
    tab::{Tab, TabBar},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use super::{
    account::{OrderRequest, OrderStatus, OrderType, money},
    colors::PriceColors,
    portfolio::AccountDesk,
    ui,
};
use crate::trading::{bus::InstrumentBus, rows::Side};

/// The shares a new ticket starts with.
const DEFAULT_QUANTITY: &str = "10";
/// A limit price's step.
const PRICE_STEP: f64 = 0.05;

/// What the last Submit did: the order's line, or why it was refused.
#[derive(Clone, Debug, PartialEq)]
struct Outcome {
    placed: bool,
    text: SharedString,
}

pub struct Ticket {
    bus: Entity<InstrumentBus>,
    desk: Entity<AccountDesk>,
    side: Side,
    order_type: OrderType,
    quantity: Entity<InputState>,
    /// The limit price field, made the first time it is shown
    /// (`limit_input`). Until its first draw an input lays its text out in
    /// the window's text style of when it was made: outside a draw, GPUI's
    /// default `.SystemUIFont`, which the browser does not have (GPUI's web
    /// text system panics on it). Made up front, the field a Market order
    /// hides took each new instrument's price in that font.
    limit: Option<Entity<InputState>>,
    /// The limit price until the field is made.
    limit_text: SharedString,
    outcome: Option<Outcome>,
    _subscriptions: Vec<Subscription>,
}

impl Ticket {
    pub fn new(
        bus: Entity<InstrumentBus>,
        desk: Entity<AccountDesk>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let quantity = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(DEFAULT_QUANTITY)
                .min(1.)
                .step(1.)
        });
        let subscriptions = vec![
            cx.subscribe_in(&quantity, window, Self::on_input_event),
            // A new instrument: a fresh limit at its price.
            cx.observe_in(&bus, window, |ticket, _, window, cx| {
                ticket.outcome = None;
                ticket.reset_limit(window, cx);
            }),
            // The estimate follows the price.
            cx.observe(&desk, |_, _, cx| cx.notify()),
        ];
        Self {
            bus,
            desk,
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity,
            limit: None,
            limit_text: SharedString::default(),
            outcome: None,
            _subscriptions: subscriptions,
        }
    }

    /// Turns the ticket to `side` and puts the keyboard in the quantity.
    pub fn set_side(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        self.side = side;
        self.outcome = None;
        self.quantity
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub fn set_order_type(
        &mut self,
        order_type: OrderType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.order_type = order_type;
        self.outcome = None;
        if order_type == OrderType::Limit && self.limit_value(cx).trim().is_empty() {
            self.reset_limit(window, cx);
        }
        cx.notify();
    }

    /// Market or Limit, for the page's input checks.
    #[cfg(target_family = "wasm")]
    pub fn order_type(&self) -> OrderType {
        self.order_type
    }

    /// Sets the number of shares, as typing would (the snapshot's orders).
    #[cfg(feature = "test-support")]
    pub fn set_quantity(&mut self, quantity: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.quantity.update(cx, |input, cx| {
            input.set_value(quantity.to_string(), window, cx)
        });
        cx.notify();
    }

    /// Sets the limit price to the open instrument's last price.
    fn reset_limit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_limit_below_market(0., window, cx);
    }

    /// Sets the limit price `share` under the open instrument's last price
    /// (0.03 is 3 % under it), as typing would.
    pub fn set_limit_below_market(
        &mut self,
        share: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let last = self.quote_of_current(cx).map(|quote| quote.last);
        let text = last.map_or(String::new(), |last| format!("{:.2}", last * (1. - share)));
        match &self.limit {
            Some(limit) => limit.update(cx, |input, cx| input.set_value(text, window, cx)),
            None => self.limit_text = text.into(),
        }
        cx.notify();
    }

    /// The limit price as typed.
    fn limit_value(&self, cx: &App) -> SharedString {
        self.limit
            .as_ref()
            .map_or_else(|| self.limit_text.clone(), |limit| limit.read(cx).value())
    }

    /// The limit price field, made on its first draw with the price set so
    /// far: drawing gives it the font of the theme before any text is laid
    /// out (see `Ticket::limit`).
    fn limit_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(limit) = &self.limit {
            return limit.clone();
        }
        let text = self.limit_text.clone();
        let limit = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Price")
                .default_value(text)
                .min(0.01)
                .step(PRICE_STEP)
        });
        self._subscriptions
            .push(cx.subscribe_in(&limit, window, Self::on_input_event));
        self.limit = Some(limit.clone());
        limit
    }

    fn quote_of_current(&self, cx: &App) -> Option<std::sync::Arc<crate::trading::rows::Quote>> {
        let id = self.bus.read(cx).current()?;
        self.desk.read(cx).quote(id)
    }

    fn on_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.outcome = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.submit(window, cx),
            _ => {}
        }
    }

    /// The order the ticket describes, if it names an instrument.
    fn request(&self, cx: &App) -> Option<OrderRequest> {
        let number = |text: SharedString| text.trim().replace(',', "").parse::<f64>().unwrap_or(0.);
        Some(OrderRequest {
            instrument: self.bus.read(cx).current()?,
            side: self.side,
            order_type: self.order_type,
            quantity: number(self.quantity.read(cx).value()).floor() as i64,
            limit: number(self.limit_value(cx)),
        })
    }

    /// Places the order and says what came of it.
    pub fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.request(cx) else {
            return;
        };
        let placed = self.desk.update(cx, |desk, cx| desk.place(&request, cx));
        let ticker = request.instrument.get().ticker;
        self.outcome = Some(match placed {
            Ok(order) => {
                let verb = match (order.side, order.status) {
                    (Side::Buy, OrderStatus::Filled { .. }) => "Bought",
                    (Side::Sell, OrderStatus::Filled { .. }) => "Sold",
                    (Side::Buy, OrderStatus::Working) => "Working: buy",
                    (Side::Sell, OrderStatus::Working) => "Working: sell",
                };
                let price = match order.status {
                    OrderStatus::Filled { price } => price,
                    OrderStatus::Working => order.limit.unwrap_or(0.),
                };
                Outcome {
                    placed: true,
                    text: format!("{verb} {} {ticker} at {price:.2}", order.quantity).into(),
                }
            }
            Err(reject) => Outcome {
                placed: false,
                text: reject.message().into(),
            },
        });
        cx.notify();
    }

    /// Buy and Sell as one segmented control, the selected side filled in
    /// its colour.
    fn render_sides(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let side_button = |side: Side, cx: &mut Context<Self>| {
            let selected = self.side == side;
            Button::new(side.label())
                .label(side.label())
                .flex_1()
                .rounded(ButtonRounded::Large)
                .with_variant(if selected {
                    side_variant(side, cx)
                } else {
                    ButtonVariant::Ghost
                })
                .on_click(
                    cx.listener(move |ticket, _, window, cx| ticket.set_side(side, window, cx)),
                )
        };
        let theme = cx.theme();
        h_flex()
            .w_full()
            .p(px(4.))
            .gap(px(4.))
            .rounded(theme.radius_lg)
            .bg(theme.muted)
            .child(side_button(Side::Buy, cx))
            .child(side_button(Side::Sell, cx))
    }

    fn render_field(label: &'static str, field: NumberInput, cx: &App) -> impl IntoElement + use<> {
        h_flex()
            .w_full()
            .gap(px(12.))
            .child(
                div()
                    .flex_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().w(px(150.)).flex_none().child(field))
    }

    fn render_line(label: &'static str, value: SharedString, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .child(
                div()
                    .flex_1()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child(value),
            )
    }
}

/// A side's button: Buy in the colour of a rise, Sell in that of a fall.
/// gpui-kit's `success` and `danger` buttons are the theme's green and red;
/// a custom button colour is drawn at a fifth of its strength, too pale
/// for a filled button.
fn side_variant(side: Side, cx: &App) -> ButtonVariant {
    let (rise, fall) = PriceColors::current(cx).pick(ButtonVariant::Success, ButtonVariant::Danger);
    match side {
        Side::Buy => rise,
        Side::Sell => fall,
    }
}

impl Render for Ticket {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let limit = (self.order_type == OrderType::Limit).then(|| self.limit_input(window, cx));
        let theme = cx.theme().clone();
        let quote = self.quote_of_current(cx);
        let request = self.request(cx);
        let desk = self.desk.read(cx);
        let account = desk.account();
        let summary = desk.summary();
        let check = request
            .as_ref()
            .zip(quote.as_ref())
            .map(|(request, quote)| account.check(request, quote));
        let estimate = request
            .as_ref()
            .zip(quote.as_ref())
            .map(|(request, quote)| request.quantity.max(0) as f64 * request.price(quote));
        let free = request
            .as_ref()
            .map_or(0, |request| account.free(request.instrument));
        let ticker = quote.as_ref().map_or("—", |quote| quote.ticker);
        let market_price = quote.as_ref().map(|quote| match self.side {
            Side::Buy => format!("{:.2} ask", quote.ask),
            Side::Sell => format!("{:.2} bid", quote.bid),
        });
        let error = match check {
            Some(Err(reject)) => Some(SharedString::from(reject.message())),
            _ => None,
        };
        let can_submit = matches!(check, Some(Ok(())));

        v_flex()
            .size_full()
            .p(ui::CARD_PADDING)
            .gap(px(14.))
            .font_features(ui::tabular_figures())
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::BOLD)
                            .child("Trade"),
                    )
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.primary)
                            .child(ticker),
                    ),
            )
            .child(self.render_sides(cx))
            .child(
                TabBar::new("order-type")
                    .segmented()
                    .small()
                    .selected_index(self.order_type.ix())
                    .children(OrderType::ALL.map(|order_type| Tab::new().label(order_type.label())))
                    .on_click(cx.listener(|ticket, ix: &usize, window, cx| {
                        if let Some(&order_type) = OrderType::ALL.get(*ix) {
                            ticket.set_order_type(order_type, window, cx);
                        }
                    })),
            )
            .child(Self::render_field(
                "Shares",
                NumberInput::new(&self.quantity),
                cx,
            ))
            .map(|ticket| match limit {
                Some(limit) => ticket.child(Self::render_field(
                    "Limit price",
                    NumberInput::new(&limit),
                    cx,
                )),
                None => ticket.child(Self::render_line(
                    "Market price",
                    market_price.map_or("—".into(), SharedString::from),
                    cx,
                )),
            })
            .child(div().h(px(1.)).w_full().bg(theme.border))
            .child(Self::render_line(
                match self.side {
                    Side::Buy => "Estimated cost",
                    Side::Sell => "Estimated proceeds",
                },
                estimate.map_or("—".into(), |estimate| money(estimate).into()),
                cx,
            ))
            .map(|ticket| match self.side {
                Side::Buy => ticket.child(Self::render_line(
                    "Buying power",
                    money(summary.buying_power).into(),
                    cx,
                )),
                Side::Sell => ticket.child(Self::render_line(
                    "Shares available",
                    free.to_string().into(),
                    cx,
                )),
            })
            .child(
                Button::new("submit")
                    .label(SharedString::from(format!("{} {ticker}", self.side.label())))
                    .large()
                    .w_full()
                    .rounded(ButtonRounded::Large)
                    .with_variant(side_variant(self.side, cx))
                    .disabled(!can_submit)
                    .on_click(cx.listener(|ticket, _, window, cx| ticket.submit(window, cx))),
            )
            .when_some(error, |ticket, error| {
                ticket.child(div().text_size(px(13.)).text_color(theme.danger).child(error))
            })
            .when_some(self.outcome.clone(), |ticket, outcome| {
                ticket.child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(if outcome.placed {
                            theme.primary
                        } else {
                            theme.danger
                        })
                        .child(outcome.text),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child("Simulated orders fill against simulated prices. Nothing is sent to a market."),
            )
    }
}
