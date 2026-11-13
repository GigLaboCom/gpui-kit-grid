//! The rows the simulator writes: quotes, trades, the order book of the
//! current instrument, positions and headlines, with their columns; and
//! the price bars of a chart. Free of GPUI: only the grid's row traits.
//! An example shows the columns it wants; a column one example needs is
//! one more variant here.
//!
//! Every text a cell shows is made when the row is written, on the
//! simulator's thread: `GridRow::value` runs for every visible cell in
//! every frame and must not allocate.

use std::sync::Arc;

use gpui_kit_grid::{CellValue, ColumnIx, GridColumns, GridRow};

use super::instruments::{InstrumentId, Sector};

/// Implements `GridColumns` for a column enum: its ids, in order.
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

columns!(QuoteColumn {
    Ticker => "ticker",
    Name => "name",
    Sector => "sector",
    Last => "last",
    Change => "chg",
    ChangePct => "pct",
    Bid => "bid",
    Ask => "ask",
    Volume => "volume",
});

/// A watchlist line: one instrument's simulated quote.
#[derive(Clone, Debug)]
pub struct Quote {
    pub id: InstrumentId,
    pub ticker: &'static str,
    pub name: &'static str,
    pub sector: Sector,
    pub last: f64,
    /// The simulated previous close: Chg and %Chg are against it.
    pub close: f64,
    pub bid: f64,
    pub ask: f64,
    pub volume: i64,
}

impl Quote {
    pub fn change(&self) -> f64 {
        self.last - self.close
    }

    pub fn change_pct(&self) -> f64 {
        (self.last / self.close - 1.) * 100.
    }
}

impl GridRow for Quote {
    type Key = InstrumentId;
    type Column = QuoteColumn;

    fn key(&self) -> &InstrumentId {
        &self.id
    }

    fn value(&self, column: QuoteColumn) -> CellValue<'_> {
        match column {
            QuoteColumn::Ticker => CellValue::Text(self.ticker),
            QuoteColumn::Name => CellValue::Text(self.name),
            QuoteColumn::Sector => CellValue::Text(self.sector.label()),
            QuoteColumn::Last => CellValue::Float(self.last),
            QuoteColumn::Change => CellValue::Float(self.change()),
            QuoteColumn::ChangePct => CellValue::Float(self.change_pct()),
            QuoteColumn::Bid => CellValue::Float(self.bid),
            QuoteColumn::Ask => CellValue::Float(self.ask),
            QuoteColumn::Volume => CellValue::Int(self.volume),
        }
    }
}

/// Which side of the book, or which side a trade hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Self::Buy => "Buy",
            Self::Sell => "Sell",
        }
    }
}

columns!(PrintColumn {
    Time => "time",
    Ticker => "ticker",
    Price => "price",
    Size => "size",
    Side => "side",
    Seq => "seq",
});

/// A simulated trade on the tape: of the current instrument, or of every
/// instrument with `TAPE ALL`. The key is a sequence number; the tape keeps
/// the latest `TAPE_LEN` trades.
#[derive(Clone, Debug)]
pub struct Print {
    pub seq: u64,
    /// UTC, `hh:mm:ss.mmm`.
    pub time: Arc<str>,
    pub ticker: &'static str,
    pub price: f64,
    pub size: i64,
    /// `Buy` on an uptick or at the ask, `Sell` on a downtick or at the bid.
    pub side: Side,
}

impl GridRow for Print {
    type Key = u64;
    type Column = PrintColumn;

    fn key(&self) -> &u64 {
        &self.seq
    }

    fn value(&self, column: PrintColumn) -> CellValue<'_> {
        match column {
            PrintColumn::Time => CellValue::Text(&self.time),
            PrintColumn::Ticker => CellValue::Text(self.ticker),
            PrintColumn::Price => CellValue::Float(self.price),
            PrintColumn::Size => CellValue::Int(self.size),
            PrintColumn::Side => CellValue::Text(self.side.label()),
            PrintColumn::Seq => CellValue::Int(self.seq as i64),
        }
    }
}

columns!(BookColumn {
    Side => "side",
    Price => "price",
    Size => "size",
    Total => "total",
    Orders => "orders",
});

/// One level of the current instrument's simulated order book, or the
/// spread line between its asks and bids.
///
/// The key is the level: `-1` is the best ask, `-20` the 20th; `1` the best
/// bid, `20` the 20th; `0` (`SPREAD_LEVEL`) the spread line, priced at the
/// mid. Sorting by Price descending shows the asks above the bids with the
/// spread line between them.
#[derive(Clone, Debug)]
pub struct BookLevel {
    pub level: i8,
    pub price: f64,
    pub size: i64,
    /// The size of this level and every better one on its side.
    pub total: i64,
    pub orders: i64,
    /// `size` over the largest size of the book when it was written, from
    /// 0 to 1: how strongly the level is shaded.
    pub depth: f32,
    /// The spread line only: best ask minus best bid; 0 on a level.
    pub spread: f64,
}

/// The key of the spread line.
pub const SPREAD_LEVEL: i8 = 0;

impl BookLevel {
    /// The spread line between `best_ask` and `best_bid`, at the mid.
    pub fn spread_line(best_ask: f64, best_bid: f64) -> Self {
        Self {
            level: SPREAD_LEVEL,
            price: (best_ask + best_bid) / 2.,
            size: 0,
            total: 0,
            orders: 0,
            depth: 0.,
            spread: best_ask - best_bid,
        }
    }

    /// `Buy` for a bid, `Sell` for an ask (and the spread line).
    pub fn side(&self) -> Side {
        if self.level > 0 {
            Side::Buy
        } else {
            Side::Sell
        }
    }

    pub fn is_spread(&self) -> bool {
        self.level == SPREAD_LEVEL
    }

    /// The best bid or the best ask.
    pub fn is_best(&self) -> bool {
        self.level.abs() == 1
    }
}

impl GridRow for BookLevel {
    type Key = i8;
    type Column = BookColumn;

    fn key(&self) -> &i8 {
        &self.level
    }

    fn value(&self, column: BookColumn) -> CellValue<'_> {
        if self.is_spread() {
            // The mid under Price, the spread under Cum, labelled between.
            return match column {
                BookColumn::Side => CellValue::Text("Mid"),
                BookColumn::Price => CellValue::Float(self.price),
                BookColumn::Size => CellValue::Text("spread"),
                BookColumn::Total => CellValue::Float(self.spread),
                BookColumn::Orders => CellValue::Empty,
            };
        }
        match column {
            BookColumn::Side => CellValue::Text(match self.side() {
                Side::Buy => "Bid",
                Side::Sell => "Ask",
            }),
            BookColumn::Price => CellValue::Float(self.price),
            BookColumn::Size => CellValue::Int(self.size),
            BookColumn::Total => CellValue::Int(self.total),
            BookColumn::Orders => CellValue::Int(self.orders),
        }
    }
}

columns!(PositionColumn {
    Ticker => "ticker",
    Quantity => "qty",
    Average => "avg",
    Last => "last",
    Value => "value",
    Pnl => "pnl",
    PnlPct => "pnl_pct",
});

/// A simulated position in one instrument, held in one account; negative
/// quantities are short.
#[derive(Clone, Debug)]
pub struct Position {
    pub id: InstrumentId,
    pub ticker: &'static str,
    /// The simulated account that holds it.
    pub account: &'static str,
    pub sector: Sector,
    pub quantity: i64,
    pub average: f64,
    pub last: f64,
}

impl Position {
    pub fn value(&self) -> f64 {
        self.quantity as f64 * self.last
    }

    pub fn pnl(&self) -> f64 {
        (self.last - self.average) * self.quantity as f64
    }

    pub fn pnl_pct(&self) -> f64 {
        (self.last / self.average - 1.) * 100. * (self.quantity.signum() as f64)
    }
}

impl GridRow for Position {
    type Key = InstrumentId;
    type Column = PositionColumn;

    fn key(&self) -> &InstrumentId {
        &self.id
    }

    fn value(&self, column: PositionColumn) -> CellValue<'_> {
        match column {
            PositionColumn::Ticker => CellValue::Text(self.ticker),
            PositionColumn::Quantity => CellValue::Int(self.quantity),
            PositionColumn::Average => CellValue::Float(self.average),
            PositionColumn::Last => CellValue::Float(self.last),
            PositionColumn::Value => CellValue::Float(self.value()),
            PositionColumn::Pnl => CellValue::Float(self.pnl()),
            PositionColumn::PnlPct => CellValue::Float(self.pnl_pct()),
        }
    }
}

columns!(HeadlineColumn {
    Time => "time",
    Tag => "tag",
    Ticker => "ticker",
    Text => "headline",
    Seq => "seq",
});

/// What every headline's Tag says: it is simulated.
pub const SIMULATED_TAG: &str = "SIM";

/// A simulated headline: a neutral line about the simulated market, never
/// a corporate event. `instrument` is `None` for a market-wide line. The
/// key is a sequence number; the simulator keeps the latest `NEWS_KEPT`.
#[derive(Clone, Debug)]
pub struct Headline {
    pub seq: u64,
    /// UTC, `hh:mm:ss`.
    pub time: Arc<str>,
    pub instrument: Option<InstrumentId>,
    pub text: Arc<str>,
}

impl GridRow for Headline {
    type Key = u64;
    type Column = HeadlineColumn;

    fn key(&self) -> &u64 {
        &self.seq
    }

    fn value(&self, column: HeadlineColumn) -> CellValue<'_> {
        match column {
            HeadlineColumn::Time => CellValue::Text(&self.time),
            HeadlineColumn::Tag => CellValue::Text(SIMULATED_TAG),
            HeadlineColumn::Ticker => self
                .instrument
                .map_or(CellValue::Empty, |id| CellValue::Text(id.get().ticker)),
            HeadlineColumn::Text => CellValue::Text(&self.text),
            HeadlineColumn::Seq => CellValue::Int(self.seq as i64),
        }
    }
}

/// One bar of a simulated price series: open, high, low, close and volume
/// over `BAR_SECONDS` of simulated time. The volume does not grow with the
/// update rate (`market.rs`, `TRADES_PER_BAR`), so it is fractional.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl Bar {
    pub fn new(price: f64) -> Self {
        Self {
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 0.,
        }
    }

    pub fn trade(&mut self, price: f64, volume: f64) {
        self.high = self.high.max(price);
        self.low = self.low.min(price);
        self.close = price;
        self.volume += volume;
    }

    /// The typical price, `(high + low + close) / 3`: what a VWAP weighs by
    /// volume, on a chart and in a headline alike.
    pub fn typical(&self) -> f64 {
        (self.high + self.low + self.close) / 3.
    }
}

/// The volume-weighted average of `bars`' typical prices, or `None` without
/// volume.
pub fn vwap<'a>(bars: impl IntoIterator<Item = &'a Bar>) -> Option<f64> {
    let (turnover, volume) = bars.into_iter().fold((0., 0.), |(turnover, volume), bar| {
        (turnover + bar.typical() * bar.volume, volume + bar.volume)
    });
    (volume > 0.).then(|| turnover / volume)
}
