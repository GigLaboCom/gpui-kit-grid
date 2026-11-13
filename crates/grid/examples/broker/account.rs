//! The simulated account: cash, holdings and orders, and the rows the
//! Positions and Orders grids show. Free of GPUI.
//!
//! Orders fill against the simulated quotes only. A market order fills at
//! once, a buy at the ask and a sell at the bid. A limit order fills at once
//! if the quote already reaches it, else it works until a later quote does
//! (`fill_working`, which `AccountDesk` calls four times a second). The
//! account holds no short positions: a sell takes shares held and not yet
//! claimed by a working sell.

use std::{collections::BTreeMap, sync::Arc};

use gpui_kit_grid::{CellValue, ColumnIx, GridColumns, GridRow};

use crate::trading::{
    instruments::{self, InstrumentId},
    rows::{Quote, Side},
};

/// Implements `GridColumns` for a column enum: its ids, in order (as
/// `trading/rows.rs` does for its rows).
macro_rules! columns {
    ($name:ident { $($column:ident => $id:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($column),+
        }

        impl GridColumns for $name {
            fn all() -> &'static [Self] {
                &[$(Self::$column),+]
            }

            fn id(self) -> &'static str {
                match self {
                    $(Self::$column => $id),+
                }
            }

            fn ix(self) -> ColumnIx {
                ColumnIx::new(self as u16)
            }
        }
    };
}

/// The cash the account starts with.
pub const STARTING_CASH: f64 = 18_450.;

/// The holdings the account starts with, bought before today: ticker,
/// shares, and the average cost as a share of the simulated previous close.
const STARTING_HOLDINGS: [(&str, i64, f64); 6] = [
    ("AAPL", 40, 0.82),
    ("NVDA", 60, 0.74),
    ("AMZN", 25, 0.95),
    ("KO", 80, 0.91),
    ("SPY", 12, 0.88),
    ("TSLA", 10, 1.06),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OrderType {
    #[default]
    Market,
    Limit,
}

impl OrderType {
    pub const ALL: [Self; 2] = [Self::Market, Self::Limit];

    pub fn label(self) -> &'static str {
        match self {
            Self::Market => "Market",
            Self::Limit => "Limit",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }
}

/// What the ticket asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrderRequest {
    pub instrument: InstrumentId,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: i64,
    /// The limit price of a limit order; unused for a market order.
    pub limit: f64,
}

impl OrderRequest {
    /// The price the order is expected to trade at: its limit, or the side
    /// of `quote` a market order takes.
    pub fn price(&self, quote: &Quote) -> f64 {
        match self.order_type {
            OrderType::Limit => self.limit,
            OrderType::Market => match self.side {
                Side::Buy => quote.ask,
                Side::Sell => quote.bid,
            },
        }
    }
}

/// Why an order is not placed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reject {
    /// No shares asked for.
    NoQuantity,
    /// A limit order without a positive price.
    NoPrice,
    /// A buy that costs more than the buying power.
    BuyingPower { available: f64 },
    /// A sell of more shares than are held and free.
    Shares { free: i64 },
}

impl Reject {
    /// The ticket's line for it.
    pub fn message(self) -> String {
        match self {
            Self::NoQuantity => "Enter a quantity of at least one share".into(),
            Self::NoPrice => "Enter a limit price".into(),
            Self::BuyingPower { available } => {
                format!("Not enough buying power ({})", money(available))
            }
            Self::Shares { free: 0 } => "You hold no shares to sell".into(),
            Self::Shares { free } => format!("You can sell at most {free} shares"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OrderStatus {
    Working,
    Filled { price: f64 },
}

columns!(OrderColumn {
    Time => "time",
    Ticker => "ticker",
    Side => "side",
    Type => "type",
    Quantity => "qty",
    Limit => "limit",
    Fill => "fill",
    Status => "status",
    Seq => "seq",
});

/// A simulated order, working or filled. The key is its sequence number.
#[derive(Clone, Debug)]
pub struct Order {
    pub seq: u64,
    /// When it was placed, UTC `hh:mm:ss`.
    pub time: Arc<str>,
    pub instrument: InstrumentId,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: i64,
    /// A limit order's price.
    pub limit: Option<f64>,
    pub status: OrderStatus,
}

impl Order {
    pub fn is_working(&self) -> bool {
        self.status == OrderStatus::Working
    }
}

impl GridRow for Order {
    type Key = u64;
    type Column = OrderColumn;

    fn key(&self) -> &u64 {
        &self.seq
    }

    fn value(&self, column: OrderColumn) -> CellValue<'_> {
        match column {
            OrderColumn::Time => CellValue::Text(&self.time),
            OrderColumn::Ticker => CellValue::Text(self.instrument.get().ticker),
            OrderColumn::Side => CellValue::Text(self.side.label()),
            OrderColumn::Type => CellValue::Text(self.order_type.label()),
            OrderColumn::Quantity => CellValue::Int(self.quantity),
            OrderColumn::Limit => self.limit.map_or(CellValue::Empty, CellValue::Float),
            OrderColumn::Fill => match self.status {
                OrderStatus::Filled { price } => CellValue::Float(price),
                OrderStatus::Working => CellValue::Empty,
            },
            OrderColumn::Status => CellValue::Text(match self.status {
                OrderStatus::Working => "Working",
                OrderStatus::Filled { .. } => "Filled",
            }),
            OrderColumn::Seq => CellValue::Int(self.seq as i64),
        }
    }
}

columns!(HoldingColumn {
    Ticker => "ticker",
    Quantity => "qty",
    Average => "avg",
    Last => "last",
    Value => "value",
    DayPnl => "day_pnl",
    TotalPnl => "pnl",
});

/// A position of the account at the current simulated price.
#[derive(Clone, Debug)]
pub struct Holding {
    pub id: InstrumentId,
    pub ticker: &'static str,
    pub quantity: i64,
    /// The average cost per share.
    pub average: f64,
    pub last: f64,
    /// What the shares were worth at the start of the day: the previous
    /// close for shares held overnight, the fill price for shares bought
    /// today. The day's P&L is against it.
    pub day_base: f64,
}

impl Holding {
    pub fn value(&self) -> f64 {
        self.quantity as f64 * self.last
    }

    pub fn day_pnl(&self) -> f64 {
        self.value() - self.day_base
    }

    pub fn total_pnl(&self) -> f64 {
        (self.last - self.average) * self.quantity as f64
    }
}

impl GridRow for Holding {
    type Key = InstrumentId;
    type Column = HoldingColumn;

    fn key(&self) -> &InstrumentId {
        &self.id
    }

    fn value(&self, column: HoldingColumn) -> CellValue<'_> {
        match column {
            HoldingColumn::Ticker => CellValue::Text(self.ticker),
            HoldingColumn::Quantity => CellValue::Int(self.quantity),
            HoldingColumn::Average => CellValue::Float(self.average),
            HoldingColumn::Last => CellValue::Float(self.last),
            HoldingColumn::Value => CellValue::Float(self.value()),
            HoldingColumn::DayPnl => CellValue::Float(self.day_pnl()),
            HoldingColumn::TotalPnl => CellValue::Float(self.total_pnl()),
        }
    }
}

/// The account at the current prices, for its card.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    /// Cash and the holdings' market value.
    pub total: f64,
    /// The holdings' change since the start of the day, and the profit of
    /// today's sales against it.
    pub day_pnl: f64,
    pub cash: f64,
    pub buying_power: f64,
}

impl Summary {
    /// The day's P&L against the account's value at the start of the day.
    pub fn day_pnl_pct(&self) -> f64 {
        let start = self.total - self.day_pnl;
        if start > 0. {
            self.day_pnl / start * 100.
        } else {
            0.
        }
    }
}

/// The shares of one instrument.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Lot {
    quantity: i64,
    /// What the shares cost in all.
    cost: f64,
    /// What they were worth at the start of the day (`Holding::day_base`).
    day_base: f64,
}

/// The simulated account.
pub struct Account {
    cash: f64,
    lots: BTreeMap<InstrumentId, Lot>,
    orders: Vec<Order>,
    /// Today's sales against the shares' worth at the start of the day.
    realized_today: f64,
}

impl Default for Account {
    fn default() -> Self {
        Self::new()
    }
}

impl Account {
    /// `STARTING_CASH` and `STARTING_HOLDINGS`, bought before today.
    pub fn new() -> Self {
        let lots = STARTING_HOLDINGS
            .iter()
            .filter_map(|&(ticker, quantity, cost_share)| {
                let id = instruments::find(ticker)?;
                let close = id.get().seed;
                let average = round_cents(close * cost_share);
                Some((
                    id,
                    Lot {
                        quantity,
                        cost: average * quantity as f64,
                        day_base: close * quantity as f64,
                    },
                ))
            })
            .collect();
        Self::with(STARTING_CASH, lots)
    }

    fn with(cash: f64, lots: BTreeMap<InstrumentId, Lot>) -> Self {
        Self {
            cash,
            lots,
            orders: Vec::new(),
            realized_today: 0.,
        }
    }

    /// An account with `cash` and nothing else.
    #[cfg(test)]
    fn with_cash(cash: f64) -> Self {
        Self::with(cash, BTreeMap::new())
    }

    pub fn cash(&self) -> f64 {
        self.cash
    }

    pub fn orders(&self) -> &[Order] {
        &self.orders
    }

    pub fn order(&self, seq: u64) -> Option<&Order> {
        self.orders.get(seq as usize)
    }

    /// The shares of `id` held.
    pub fn held(&self, id: InstrumentId) -> i64 {
        self.lots.get(&id).map_or(0, |lot| lot.quantity)
    }

    /// The shares of `id` held and not claimed by a working sell.
    pub fn free(&self, id: InstrumentId) -> i64 {
        let claimed: i64 = self
            .orders
            .iter()
            .filter(|order| {
                order.is_working() && order.instrument == id && order.side == Side::Sell
            })
            .map(|order| order.quantity)
            .sum();
        self.held(id) - claimed
    }

    /// The cash less what the working buys would spend at their limits.
    pub fn buying_power(&self) -> f64 {
        let reserved: f64 = self
            .orders
            .iter()
            .filter(|order| order.is_working() && order.side == Side::Buy)
            .map(|order| order.quantity as f64 * order.limit.unwrap_or(0.))
            .sum();
        self.cash - reserved
    }

    /// Checks `request` against the account and `quote` without placing
    /// it.
    pub fn check(&self, request: &OrderRequest, quote: &Quote) -> Result<(), Reject> {
        if request.quantity <= 0 {
            return Err(Reject::NoQuantity);
        }
        if request.order_type == OrderType::Limit && request.limit <= 0. {
            return Err(Reject::NoPrice);
        }
        match request.side {
            Side::Buy => {
                let cost = request.quantity as f64 * request.price(quote);
                let available = self.buying_power();
                if cost > available {
                    return Err(Reject::BuyingPower { available });
                }
            }
            Side::Sell => {
                let free = self.free(request.instrument);
                if request.quantity > free {
                    return Err(Reject::Shares { free });
                }
            }
        }
        Ok(())
    }

    /// Places `request` at `time`: fills it at once against `quote` if it
    /// is a market order or a limit the quote reaches, else leaves it
    /// working. Returns the order.
    pub fn place(
        &mut self,
        request: &OrderRequest,
        quote: &Quote,
        time: Arc<str>,
    ) -> Result<&Order, Reject> {
        self.check(request, quote)?;
        let seq = self.orders.len() as u64;
        self.orders.push(Order {
            seq,
            time,
            instrument: request.instrument,
            side: request.side,
            order_type: request.order_type,
            quantity: request.quantity,
            limit: (request.order_type == OrderType::Limit).then_some(request.limit),
            status: OrderStatus::Working,
        });
        self.try_fill(seq as usize, quote);
        Ok(&self.orders[seq as usize])
    }

    /// Fills the working orders `quote` reaches, each at its side of the
    /// quote. Returns the sequence numbers of those filled.
    pub fn fill_working<'a>(
        &mut self,
        quote_of: impl Fn(InstrumentId) -> Option<&'a Quote>,
    ) -> Vec<u64> {
        let mut filled = Vec::new();
        for ix in 0..self.orders.len() {
            let order = &self.orders[ix];
            if !order.is_working() {
                continue;
            }
            if let Some(quote) = quote_of(order.instrument)
                && self.try_fill(ix, quote)
            {
                filled.push(ix as u64);
            }
        }
        filled
    }

    /// Fills order `ix` if `quote` reaches it: a market order always, a
    /// buy limit at an ask at or below it, a sell limit at a bid at or above
    /// it. A buy that no longer fits the cash, or a sell of shares no
    /// longer held, keeps working.
    fn try_fill(&mut self, ix: usize, quote: &Quote) -> bool {
        let order = &self.orders[ix];
        let price = match order.side {
            Side::Buy => quote.ask,
            Side::Sell => quote.bid,
        };
        let reached = match (order.limit, order.side) {
            (None, _) => true,
            (Some(limit), Side::Buy) => price <= limit,
            (Some(limit), Side::Sell) => price >= limit,
        };
        if !reached {
            return false;
        }
        let (id, side, quantity) = (order.instrument, order.side, order.quantity);
        let amount = quantity as f64 * price;
        match side {
            Side::Buy => {
                if amount > self.cash {
                    return false;
                }
                self.cash -= amount;
                let lot = self.lots.entry(id).or_default();
                lot.quantity += quantity;
                lot.cost += amount;
                lot.day_base += amount;
            }
            Side::Sell => {
                let Some(lot) = self
                    .lots
                    .get_mut(&id)
                    .filter(|lot| lot.quantity >= quantity)
                else {
                    return false;
                };
                let share = quantity as f64 / lot.quantity as f64;
                let (cost, day_base) = (lot.cost * share, lot.day_base * share);
                self.cash += amount;
                self.realized_today += amount - day_base;
                lot.quantity -= quantity;
                lot.cost -= cost;
                lot.day_base -= day_base;
                if lot.quantity == 0 {
                    self.lots.remove(&id);
                }
            }
        }
        self.orders[ix].status = OrderStatus::Filled { price };
        true
    }

    /// The holdings at the quotes' last prices; one without a quote at its
    /// average cost.
    pub fn holdings<'a>(
        &self,
        quote_of: impl Fn(InstrumentId) -> Option<&'a Quote>,
    ) -> Vec<Holding> {
        self.lots
            .iter()
            .map(|(&id, lot)| {
                let average = lot.cost / lot.quantity as f64;
                Holding {
                    id,
                    ticker: id.get().ticker,
                    quantity: lot.quantity,
                    average,
                    last: quote_of(id).map_or(average, |quote| quote.last),
                    day_base: lot.day_base,
                }
            })
            .collect()
    }

    /// The account at `holdings`' prices.
    pub fn summary(&self, holdings: &[Holding]) -> Summary {
        let (value, day_pnl) = holdings.iter().fold((0., 0.), |(value, pnl), holding| {
            (value + holding.value(), pnl + holding.day_pnl())
        });
        Summary {
            total: self.cash() + value,
            day_pnl: day_pnl + self.realized_today,
            cash: self.cash(),
            buying_power: self.buying_power(),
        }
    }
}

fn round_cents(price: f64) -> f64 {
    (price * 100.).round() / 100.
}

/// `12345.6` as `$12,345.60`; negative as `-$12,345.60`.
pub fn money(value: f64) -> String {
    let sign = if value < 0. { "-" } else { "" };
    format!("{sign}${}", grouped(value.abs(), 2))
}

/// `1234.5` as `+$1,234.50`, `-3` as `-$3.00`.
pub fn signed_money(value: f64) -> String {
    if value >= 0. {
        format!("+{}", money(value))
    } else {
        money(value)
    }
}

/// `value` with `decimals` decimals and thousands separated by commas.
pub fn grouped(value: f64, decimals: usize) -> String {
    let text = format!("{:.*}", decimals, value.abs());
    let (whole, fraction) = text
        .split_once('.')
        .map_or((text.as_str(), ""), |split| split);
    let mut out = String::with_capacity(text.len() + whole.len() / 3 + 1);
    if value < 0.
        && text
            .bytes()
            .any(|byte| byte.is_ascii_digit() && byte != b'0')
    {
        out.push('-');
    }
    for (ix, digit) in whole.chars().enumerate() {
        if ix > 0 && (whole.len() - ix) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(fraction);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(ticker: &str, bid: f64, ask: f64) -> Quote {
        let id = instruments::find(ticker).expect("an instrument");
        let instrument = id.get();
        Quote {
            id,
            ticker: instrument.ticker,
            name: instrument.name,
            sector: instrument.sector,
            last: (bid + ask) / 2.,
            close: instrument.seed,
            bid,
            ask,
            volume: 0,
        }
    }

    fn request(quote: &Quote, side: Side, order_type: OrderType, quantity: i64) -> OrderRequest {
        OrderRequest {
            instrument: quote.id,
            side,
            order_type,
            quantity,
            limit: 0.,
        }
    }

    fn time() -> Arc<str> {
        "10:00:00".into()
    }

    #[test]
    fn test_starting_holdings_are_instruments() {
        let account = Account::new();
        assert_eq!(account.lots.len(), STARTING_HOLDINGS.len());
        assert_eq!(account.cash(), STARTING_CASH);
    }

    #[test]
    fn test_market_orders_fill_at_the_side_of_the_quote() {
        let mut account = Account::with_cash(10_000.);
        let ko = quote("KO", 69.98, 70.02);
        let buy = request(&ko, Side::Buy, OrderType::Market, 100);
        let order = account.place(&buy, &ko, time()).expect("placed");
        assert_eq!(order.status, OrderStatus::Filled { price: 70.02 });
        assert_eq!(account.held(ko.id), 100);
        assert!((account.cash() - (10_000. - 7_002.)).abs() < 1e-9);

        let sell = request(&ko, Side::Sell, OrderType::Market, 40);
        let order = account.place(&sell, &ko, time()).expect("placed");
        assert_eq!(order.status, OrderStatus::Filled { price: 69.98 });
        assert_eq!(account.held(ko.id), 60);
        assert_eq!(account.orders().len(), 2);
    }

    #[test]
    fn test_rejects_what_the_account_cannot_do() {
        let mut account = Account::with_cash(1_000.);
        let ko = quote("KO", 69.98, 70.02);
        let too_dear = request(&ko, Side::Buy, OrderType::Market, 100);
        assert!(matches!(
            account.place(&too_dear, &ko, time()),
            Err(Reject::BuyingPower { .. })
        ));
        let short = request(&ko, Side::Sell, OrderType::Market, 1);
        assert_eq!(
            account.place(&short, &ko, time()).err(),
            Some(Reject::Shares { free: 0 })
        );
        let none = request(&ko, Side::Buy, OrderType::Market, 0);
        assert_eq!(account.check(&none, &ko), Err(Reject::NoQuantity));
        let unpriced = request(&ko, Side::Buy, OrderType::Limit, 1);
        assert_eq!(account.check(&unpriced, &ko), Err(Reject::NoPrice));
        assert!(account.orders().is_empty());
    }

    #[test]
    fn test_a_limit_order_works_until_the_quote_reaches_it() {
        let mut account = Account::with_cash(10_000.);
        let ko = quote("KO", 69.98, 70.02);
        let buy = OrderRequest {
            limit: 69.50,
            ..request(&ko, Side::Buy, OrderType::Limit, 100)
        };
        let seq = account.place(&buy, &ko, time()).expect("placed").seq;
        assert!(account.orders()[0].is_working());
        // The working buy holds its cost back from the buying power.
        assert!((account.buying_power() - (10_000. - 6_950.)).abs() < 1e-9);
        assert!(account.fill_working(|_| Some(&ko)).is_empty());

        let lower = quote("KO", 69.40, 69.44);
        assert_eq!(account.fill_working(|_| Some(&lower)), vec![seq]);
        assert_eq!(
            account.order(seq).map(|order| order.status),
            Some(OrderStatus::Filled { price: 69.44 })
        );
        assert_eq!(account.held(ko.id), 100);
        assert!((account.buying_power() - account.cash()).abs() < 1e-9);
    }

    #[test]
    fn test_a_working_sell_claims_its_shares() {
        let mut account = Account::with_cash(10_000.);
        let ko = quote("KO", 69.98, 70.02);
        account
            .place(&request(&ko, Side::Buy, OrderType::Market, 50), &ko, time())
            .expect("bought");
        let sell = OrderRequest {
            limit: 75.,
            ..request(&ko, Side::Sell, OrderType::Limit, 30)
        };
        account.place(&sell, &ko, time()).expect("working");
        assert_eq!(account.free(ko.id), 20);
        let more = request(&ko, Side::Sell, OrderType::Market, 21);
        assert_eq!(account.check(&more, &ko), Err(Reject::Shares { free: 20 }));
    }

    #[test]
    fn test_day_pnl_is_against_the_start_of_the_day() {
        let ko = quote("KO", 69.98, 70.02);
        let close = ko.close;
        let lots = BTreeMap::from([(
            ko.id,
            Lot {
                quantity: 10,
                cost: 10. * 60.,
                day_base: 10. * close,
            },
        )]);
        let mut account = Account::with(1_000., lots);
        let at = |last: f64| Quote { last, ..ko.clone() };

        let up = at(close + 1.);
        let holdings = account.holdings(|_| Some(&up));
        assert!((holdings[0].day_pnl() - 10.).abs() < 1e-9);
        assert!((holdings[0].total_pnl() - 10. * (close + 1. - 60.)).abs() < 1e-9);

        // Shares bought today count from their fill.
        account
            .place(&request(&ko, Side::Buy, OrderType::Market, 10), &ko, time())
            .expect("bought");
        let holdings = account.holdings(|_| Some(&up));
        let expected = 10. + 10. * (close + 1. - ko.ask);
        let summary = account.summary(&holdings);
        assert!((summary.day_pnl - expected).abs() < 1e-9);
        assert!((summary.total - (account.cash() + 20. * (close + 1.))).abs() < 1e-9);
    }

    #[test]
    fn test_money_groups_thousands() {
        assert_eq!(money(1_234_567.891), "$1,234,567.89");
        assert_eq!(money(-12.5), "-$12.50");
        assert_eq!(money(0.), "$0.00");
        assert_eq!(signed_money(999.999), "+$1,000.00");
        assert_eq!(signed_money(-1_000.), "-$1,000.00");
        assert_eq!(grouped(-1234.0, 0), "-1,234");
        assert_eq!(grouped(-0.001, 2), "0.00");
        assert_eq!(grouped(123.0, 0), "123");
    }
}
