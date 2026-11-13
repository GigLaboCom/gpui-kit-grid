//! The rows the exchange writes: the price ladder's levels, the time and
//! sales, the working orders and the fills, with their columns. Free of
//! GPUI: only the grid's row traits.
//!
//! The grid shows a float with two decimals, and a contract's prices have
//! the decimals of its tick (`3.850`, `4050.1`), so every price is text
//! made when the row is written, on the exchange's thread, with its tick
//! as a hidden integer column to sort by. `GridRow::value` runs for every
//! visible cell in every frame and must not allocate.

use std::sync::Arc;

use gpui_kit_grid::{CellValue, ColumnIx, GridColumns, GridRow};

use crate::trading::{instruments::ContractId, rows::Side};

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

/// A count, or nothing for zero: an empty cell reads faster on a ladder.
fn count(value: i64) -> CellValue<'static> {
    if value == 0 {
        CellValue::Empty
    } else {
        CellValue::Int(value)
    }
}

columns!(LadderColumn {
    Buys => "buys",
    Bid => "bid",
    Price => "price",
    Ask => "ask",
    Sells => "sells",
    Volume => "volume",
    Tick => "tick",
});

/// One price of the ladder: the resting sizes there, the trader's working
/// orders, the volume traded there this session, and what the price is
/// now (the last trade, the best bid or ask, the volume profile's point of
/// control). The key is the price in ticks; the ladder's rows are a fixed
/// band of prices around the market, so sizes change in place.
#[derive(Clone, Debug, PartialEq)]
pub struct LadderRow {
    pub tick: i64,
    pub price: Arc<str>,
    /// Resting bid and ask sizes; 0 where the book has none.
    pub bid: i64,
    pub ask: i64,
    /// The trader's working buy and sell orders at this price, in
    /// contracts.
    pub buys: i64,
    pub sells: i64,
    /// Contracts traded at this price this session, and of them those a
    /// buyer took: the delta here is `2 * bought - volume`.
    pub volume: i64,
    pub bought: i64,
    /// The resting size over the largest level of the book, 0 to 1.
    pub depth: f32,
    /// The volume over the session's largest at one price, 0 to 1.
    pub profile: f32,
    pub last: bool,
    pub best_bid: bool,
    pub best_ask: bool,
    /// The price traded most this session.
    pub poc: bool,
}

impl LadderRow {
    /// Bought minus sold at this price.
    pub fn delta(&self) -> i64 {
        2 * self.bought - self.volume
    }
}

impl GridRow for LadderRow {
    type Key = i64;
    type Column = LadderColumn;

    fn key(&self) -> &i64 {
        &self.tick
    }

    fn value(&self, column: LadderColumn) -> CellValue<'_> {
        match column {
            LadderColumn::Buys => count(self.buys),
            LadderColumn::Bid => count(self.bid),
            LadderColumn::Price => CellValue::Text(&self.price),
            LadderColumn::Ask => count(self.ask),
            LadderColumn::Sells => count(self.sells),
            LadderColumn::Volume => count(self.volume),
            LadderColumn::Tick => CellValue::Int(self.tick),
        }
    }
}

columns!(TradeColumn {
    Time => "time",
    Price => "price",
    Size => "size",
    Side => "side",
    Seq => "seq",
});

/// A trade of the current contract on the time and sales. The key is a
/// sequence number; the exchange keeps the latest few hundred.
#[derive(Clone, Debug)]
pub struct TradeRow {
    pub seq: u64,
    /// UTC, `hh:mm:ss.mmm`.
    pub time: Arc<str>,
    pub price: Arc<str>,
    pub size: i64,
    /// The aggressor: `Buy` took the ask, `Sell` hit the bid.
    pub side: Side,
    /// The trader's own market order.
    pub own: bool,
    /// Half a typical book level or more.
    pub block: bool,
}

impl GridRow for TradeRow {
    type Key = u64;
    type Column = TradeColumn;

    fn key(&self) -> &u64 {
        &self.seq
    }

    fn value(&self, column: TradeColumn) -> CellValue<'_> {
        match column {
            TradeColumn::Time => CellValue::Text(&self.time),
            TradeColumn::Price => CellValue::Text(&self.price),
            TradeColumn::Size => CellValue::Int(self.size),
            TradeColumn::Side => CellValue::Text(self.side.label()),
            TradeColumn::Seq => CellValue::Int(self.seq as i64),
        }
    }
}

columns!(OrderColumn {
    Id => "id",
    Time => "time",
    Contract => "contract",
    Side => "side",
    Quantity => "qty",
    Price => "price",
});

/// A working limit order of the trader, in any contract. The key is the
/// order's number; a filled or cancelled order leaves the grid.
#[derive(Clone, Debug)]
pub struct OrderRow {
    pub id: u64,
    /// UTC, `hh:mm:ss`.
    pub time: Arc<str>,
    pub contract: ContractId,
    pub side: Side,
    pub quantity: i64,
    pub tick: i64,
    pub price: Arc<str>,
}

impl GridRow for OrderRow {
    type Key = u64;
    type Column = OrderColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: OrderColumn) -> CellValue<'_> {
        match column {
            OrderColumn::Id => CellValue::Int(self.id as i64),
            OrderColumn::Time => CellValue::Text(&self.time),
            OrderColumn::Contract => CellValue::Text(self.contract.get().symbol),
            OrderColumn::Side => CellValue::Text(self.side.label()),
            OrderColumn::Quantity => CellValue::Int(self.quantity),
            OrderColumn::Price => CellValue::Text(&self.price),
        }
    }
}

columns!(FillColumn {
    Id => "id",
    Time => "time",
    Contract => "contract",
    Side => "side",
    Quantity => "qty",
    Price => "price",
    Realized => "realized",
});

/// A fill of one of the trader's orders, newest first by its number. A
/// fill that closes contracts has the P&L it realised, in US dollars.
#[derive(Clone, Debug)]
pub struct FillRow {
    pub id: u64,
    /// UTC, `hh:mm:ss`.
    pub time: Arc<str>,
    pub contract: ContractId,
    pub side: Side,
    pub quantity: i64,
    pub price: Arc<str>,
    pub realized: Option<f64>,
}

impl GridRow for FillRow {
    type Key = u64;
    type Column = FillColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: FillColumn) -> CellValue<'_> {
        match column {
            FillColumn::Id => CellValue::Int(self.id as i64),
            FillColumn::Time => CellValue::Text(&self.time),
            FillColumn::Contract => CellValue::Text(self.contract.get().symbol),
            FillColumn::Side => CellValue::Text(self.side.label()),
            FillColumn::Quantity => CellValue::Int(self.quantity),
            FillColumn::Price => CellValue::Text(&self.price),
            FillColumn::Realized => self.realized.map_or(CellValue::Empty, CellValue::Float),
        }
    }
}
