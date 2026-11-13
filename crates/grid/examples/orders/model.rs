//! The orders of a trader: the row, its columns and schema, the start
//! data, and the rules the trader's edits and the "server" follow.
//!
//! No GPUI here but the schema's widths: `tests/orders_flow.rs` uses this
//! module as it is (`#[path]`).

use std::sync::Arc;

use chrono::{DateTime, Duration as Days, TimeZone as _, Utc};
use gpui_kit::component::IconName;
use gpui_kit::px;
use gpui_kit_grid::{
    CellContent, CellValue, ColumnAlign, ColumnIx, ColumnZone, GridColumn, GridColumns, GridRow,
    GridSchema, IconValue, OwnedValue,
};

/// The columns of an order, one of every kind of value and content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderColumn {
    Id,
    Account,
    Instrument,
    Side,
    Quantity,
    Limit,
    Last,
    Filled,
    Status,
    Tif,
    Expiry,
    Active,
    Note,
    Placed,
    Cancel,
}

const COLUMNS: [OrderColumn; 15] = [
    OrderColumn::Id,
    OrderColumn::Account,
    OrderColumn::Instrument,
    OrderColumn::Side,
    OrderColumn::Quantity,
    OrderColumn::Limit,
    OrderColumn::Last,
    OrderColumn::Filled,
    OrderColumn::Status,
    OrderColumn::Tif,
    OrderColumn::Expiry,
    OrderColumn::Active,
    OrderColumn::Note,
    OrderColumn::Placed,
    OrderColumn::Cancel,
];

// By hand, as in every example: they teach the trait, and build without the
// `derive` feature.
impl GridColumns for OrderColumn {
    fn all() -> &'static [Self] {
        &COLUMNS
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Account => "account",
            Self::Instrument => "instrument",
            Self::Side => "side",
            Self::Quantity => "quantity",
            Self::Limit => "limit",
            Self::Last => "last",
            Self::Filled => "filled",
            Self::Status => "status",
            Self::Tif => "tif",
            Self::Expiry => "expiry",
            Self::Active => "active",
            Self::Note => "note",
            Self::Placed => "placed",
            Self::Cancel => "cancel",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

impl OrderColumn {
    /// The column with this id.
    pub fn from_id(id: &str) -> Option<Self> {
        COLUMNS.into_iter().find(|column| column.id() == id)
    }
}

/// Where an order is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    New,
    Working,
    PartFilled,
    Filled,
    Cancelled,
    Rejected,
}

impl Status {
    /// The icon of gpui-kit's default catalog that shows it, and its text.
    pub fn icon(self) -> IconValue<'static> {
        match self {
            Self::New => IconValue::new("icons/square.svg", "New"),
            Self::Working => IconValue::new("icons/loader-circle.svg", "Working"),
            Self::PartFilled => IconValue::new("icons/chart-pie.svg", "Part filled"),
            Self::Filled => IconValue::new("icons/circle-check.svg", "Filled"),
            Self::Cancelled => IconValue::new("icons/ban.svg", "Cancelled"),
            Self::Rejected => IconValue::new("icons/circle-x.svg", "Rejected"),
        }
    }

    /// Returns true if the order is done: no edit, no fill.
    pub fn is_final(self) -> bool {
        matches!(self, Self::Filled | Self::Cancelled | Self::Rejected)
    }
}

pub const ACCOUNTS: [&str; 5] = ["ACC-1", "ACC-2", "ACC-3", "ACC-4", "ACC-5"];
pub const INSTRUMENTS: [(&str, f64); 8] = [
    ("AAPL", 190.),
    ("MSFT", 410.),
    ("NVDA", 120.),
    ("SBER", 280.),
    ("GAZP", 160.),
    ("TSLA", 240.),
    ("AMZN", 180.),
    ("META", 500.),
];
pub const SIDES: [&str; 2] = ["Buy", "Sell"];
pub const TIFS: [&str; 3] = ["Day", "GTC", "IOC"];

/// One order. Cloning is cheap: the note is shared.
#[derive(Clone, Debug)]
pub struct Order {
    pub id: u64,
    pub account: &'static str,
    pub instrument: &'static str,
    pub side: &'static str,
    pub quantity: i64,
    pub limit: f64,
    pub last: f64,
    /// The part filled, 0 to 1.
    pub filled: f64,
    pub status: Status,
    pub tif: &'static str,
    pub expiry: DateTime<Utc>,
    pub active: bool,
    pub note: Arc<str>,
    pub placed: DateTime<Utc>,
}

impl GridRow for Order {
    type Key = u64;
    type Column = OrderColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    // #region order-value
    fn value(&self, column: OrderColumn) -> CellValue<'_> {
        match column {
            OrderColumn::Id => CellValue::Int(self.id as i64),
            OrderColumn::Account => CellValue::Text(self.account),
            OrderColumn::Instrument => CellValue::Text(self.instrument),
            OrderColumn::Side => CellValue::Text(self.side),
            OrderColumn::Quantity => CellValue::Int(self.quantity),
            OrderColumn::Limit => CellValue::Float(self.limit),
            OrderColumn::Last => CellValue::Float(self.last),
            OrderColumn::Filled => CellValue::Float(self.filled),
            OrderColumn::Status => CellValue::Icon(self.status.icon()),
            OrderColumn::Tif => CellValue::Text(self.tif),
            OrderColumn::Expiry => CellValue::Timestamp(self.expiry),
            OrderColumn::Active => CellValue::Bool(self.active),
            OrderColumn::Note => CellValue::Text(&self.note),
            OrderColumn::Placed => CellValue::Timestamp(self.placed),
            OrderColumn::Cancel => CellValue::Empty,
        }
    }
    // #endregion order-value
}

impl Order {
    /// This order with `value` in `column`, if the column takes it: the
    /// edits the "server" applies, and the moves to another account.
    pub fn with(&self, column: OrderColumn, value: &OwnedValue) -> Option<Self> {
        let mut order = self.clone();
        match (column, value) {
            (OrderColumn::Account, OwnedValue::Text(text)) => {
                order.account = ACCOUNTS.into_iter().find(|each| **each == **text)?;
            }
            (OrderColumn::Side, OwnedValue::Text(text)) => {
                order.side = SIDES.into_iter().find(|each| **each == **text)?;
            }
            (OrderColumn::Tif, OwnedValue::Text(text)) => {
                order.tif = TIFS.into_iter().find(|each| **each == **text)?;
            }
            (OrderColumn::Quantity, OwnedValue::Int(quantity)) => order.quantity = *quantity,
            (OrderColumn::Limit, OwnedValue::Float(limit)) => order.limit = *limit,
            (OrderColumn::Expiry, OwnedValue::Timestamp(expiry)) => order.expiry = *expiry,
            (OrderColumn::Active, OwnedValue::Bool(active)) => order.active = *active,
            (OrderColumn::Note, OwnedValue::Text(note)) => order.note = note.clone(),
            (OrderColumn::Note, OwnedValue::Empty) => order.note = "".into(),
            _ => return None,
        }
        Some(order)
    }
}

/// Every column with its title and width, in the schema's order: 1180 px
/// together, so a 1500 px window shows them all beside the basket.
pub const COLUMN_WIDTHS: [(OrderColumn, &str, f32); 15] = [
    (OrderColumn::Id, "Id", 56.),
    (OrderColumn::Account, "Account", 72.),
    (OrderColumn::Instrument, "Instrument", 84.),
    (OrderColumn::Side, "Side", 56.),
    (OrderColumn::Quantity, "Quantity", 72.),
    (OrderColumn::Limit, "Limit", 72.),
    (OrderColumn::Last, "Last", 72.),
    (OrderColumn::Filled, "Filled", 72.),
    (OrderColumn::Status, "Status", 40.),
    (OrderColumn::Tif, "TIF", 48.),
    (OrderColumn::Expiry, "Expiry", 150.),
    (OrderColumn::Active, "Active", 56.),
    (OrderColumn::Note, "Note", 110.),
    (OrderColumn::Placed, "Placed", 150.),
    (OrderColumn::Cancel, "Cancel", 70.),
];

// #region orders-schema
/// The columns, their widths and contents: Id frozen left; Side a tag,
/// Filled a progress bar (elements); Active a checkbox and Cancel a button
/// (controls), Cancel frozen right; Status an icon, its header the icon
/// alone.
pub fn schema() -> GridSchema<OrderColumn> {
    COLUMN_WIDTHS
        .into_iter()
        .fold(GridSchema::new(), |schema, (column, title, width)| {
            let grid_column = GridColumn::new(column, title).with_width(px(width));
            schema.column(match column {
                OrderColumn::Id => grid_column.with_zone(ColumnZone::FrozenLeft),
                OrderColumn::Side | OrderColumn::Filled => {
                    grid_column.with_content(CellContent::Element)
                }
                OrderColumn::Active => grid_column.with_content(CellContent::Control),
                // An action column: frozen right, always on screen.
                OrderColumn::Cancel => grid_column
                    .with_content(CellContent::Control)
                    .with_zone(ColumnZone::FrozenRight),
                OrderColumn::Status => grid_column
                    .with_icon_only(IconName::Info)
                    .with_align(ColumnAlign::Auto),
                _ => grid_column,
            })
        })
}
// #endregion orders-schema

/// The start of the trading day the orders were placed on, 2026-11-04 UTC:
/// fixed, so every run with a seed has the same orders.
pub fn trading_day() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 4, 0, 0, 0)
        .single()
        .expect("a valid day")
}

/// `count` orders from `seed`: ids from 1, accounts, instruments, sides
/// and statuses spread, limits near the last price, a few filled in part.
pub fn orders(count: u64, seed: u64) -> Vec<Order> {
    let mut rng = fastrand::Rng::with_seed(seed);
    let day = trading_day();
    (1..=count)
        .map(|id| {
            let (instrument, price) = INSTRUMENTS[rng.usize(..INSTRUMENTS.len())];
            let last = round_to(price * (1. + (rng.f64() - 0.5) / 20.), 0.01);
            let status = match rng.u8(..20) {
                0 => Status::New,
                1 => Status::PartFilled,
                2 => Status::Filled,
                3 => Status::Cancelled,
                _ => Status::Working,
            };
            let filled = match status {
                Status::PartFilled => round_to(rng.f64() * 0.8 + 0.1, 0.01),
                Status::Filled => 1.,
                _ => 0.,
            };
            Order {
                id,
                account: ACCOUNTS[rng.usize(..ACCOUNTS.len())],
                instrument,
                side: SIDES[rng.usize(..2)],
                quantity: 100 * rng.i64(1..=50),
                limit: round_to(last * (1. + (rng.f64() - 0.5) / 25.), STEP),
                last,
                filled,
                status,
                tif: TIFS[rng.usize(..TIFS.len())],
                expiry: day + Days::days(rng.i64(30..=90)),
                active: rng.bool(),
                note: if rng.u8(..4) == 0 {
                    "Client callback".into()
                } else {
                    "".into()
                },
                placed: day + Days::seconds(rng.i64(9 * 3600..17 * 3600)),
            }
        })
        .collect()
}

/// The price step of the exchange: limits are rounded to it.
pub const STEP: f64 = 0.05;

/// `value` rounded to a multiple of `step`.
pub fn round_to(value: f64, step: f64) -> f64 {
    ((value / step).round() * step * 100.).round() / 100.
}

/// What the trader may enter (`validate_edit`): a quantity in whole lots of
/// 100, an expiry not before today. `None` takes the value.
pub fn check_edit(column: OrderColumn, value: &OwnedValue, today: DateTime<Utc>) -> Option<String> {
    match (column, value) {
        (OrderColumn::Quantity, OwnedValue::Int(quantity)) if quantity % 100 != 0 => {
            Some("Quantity must be a multiple of 100".into())
        }
        (OrderColumn::Expiry, OwnedValue::Timestamp(expiry))
            if expiry.date_naive() < today.date_naive() =>
        {
            Some("Expiry cannot be in the past".into())
        }
        _ => None,
    }
}

/// How far a limit may be from the last price: ±10%.
pub const BAND: f64 = 0.10;

/// What the "server" makes of an edit of `order`: the value it applies
/// (a limit rounded to the price step) or why it refuses. A limit out of
/// the band around the last price is always refused.
pub fn server_rule(
    order: &Order,
    column: OrderColumn,
    value: &OwnedValue,
) -> Result<OwnedValue, &'static str> {
    if order.status.is_final() {
        return Err("Order is closed");
    }
    match (column, value) {
        (OrderColumn::Limit, OwnedValue::Float(limit)) => {
            if (limit - order.last).abs() > order.last * BAND {
                Err("Price out of band")
            } else {
                Ok(OwnedValue::Float(round_to(*limit, STEP)))
            }
        }
        _ => Ok(value.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orders_model() {
        let rows = orders(40, 7);
        assert_eq!(rows.len(), 40);
        assert_eq!(rows[0].id, 1);
        assert!(rows.iter().all(|order| order.quantity % 100 == 0));
        assert_eq!(round_to(101.53, STEP), 101.55);
        let order = Order {
            last: 100.,
            ..rows[0].clone()
        };
        let order = Order {
            status: Status::Working,
            ..order
        };
        assert_eq!(
            server_rule(&order, OrderColumn::Limit, &OwnedValue::Float(101.53)),
            Ok(OwnedValue::Float(101.55))
        );
        assert_eq!(
            server_rule(&order, OrderColumn::Limit, &OwnedValue::Float(999.)),
            Err("Price out of band")
        );
        let today = trading_day();
        assert!(check_edit(OrderColumn::Quantity, &OwnedValue::Int(150), today).is_some());
        assert!(check_edit(OrderColumn::Quantity, &OwnedValue::Int(200), today).is_none());
        assert_eq!(OrderColumn::from_id("limit"), Some(OrderColumn::Limit));
        let moved = order
            .with(OrderColumn::Account, &OwnedValue::Text("ACC-3".into()))
            .expect("an account");
        assert_eq!(moved.account, "ACC-3");
    }
}
