//! The portfolio of the example and the thread that writes it: two funds of
//! four desks of eight books, `ROWS` positions spread over the books, price
//! updates at `RATE` a second, `CHURN` positions opened and closed a second,
//! `PORTFOLIO_MOVES` positions moved to another book a second, and the
//! sums of the books, desks and funds every 100 ms.
//!
//! The grid sums nothing on the tree rows (its totals add up the leaves):
//! the producer owns the rows and writes the sums of a parent as its own
//! values, as a position server would.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::px;
use gpui_kit_grid::{
    CellValue, ColumnIx, ColumnZone, GridColumn, GridColumns, GridRow, GridSchema, GridWriter,
};

const FUNDS: u64 = 2;
const DESKS: u64 = 4;
const BOOKS: u64 = 8;
/// The tree rows above the positions: the funds, their desks and books.
pub const BRANCHES: u64 = FUNDS + FUNDS * DESKS + FUNDS * DESKS * BOOKS;
/// How often the producer writes the sums of the tree rows, in 10 ms
/// batches.
const SUM_BATCHES: u64 = 10;
/// The instruments of the positions.
const INSTRUMENTS: [&str; 12] = [
    "AAPL", "MSFT", "NVDA", "TSLA", "AMZN", "GOOG", "META", "NFLX", "ORCL", "INTC", "AMD", "IBM",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeColumn {
    Name,
    Instrument,
    Quantity,
    Price,
    Pnl,
    Delta,
}

impl GridColumns for NodeColumn {
    fn all() -> &'static [Self] {
        use NodeColumn::*;
        &[Name, Instrument, Quantity, Price, Pnl, Delta]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Instrument => "instrument",
            Self::Quantity => "quantity",
            Self::Price => "price",
            Self::Pnl => "pnl",
            Self::Delta => "delta",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// Name, the tree's column, frozen on the left: the hierarchy stays in view
/// when the other columns scroll.
pub fn schema() -> GridSchema<NodeColumn> {
    GridSchema::new()
        .column(
            GridColumn::new(NodeColumn::Name, "Name")
                .with_width(px(220.))
                .with_zone(ColumnZone::FrozenLeft),
        )
        .column(GridColumn::new(NodeColumn::Instrument, "Instrument").with_width(px(100.)))
        .column(GridColumn::new(NodeColumn::Quantity, "Quantity").with_width(px(100.)))
        .column(GridColumn::new(NodeColumn::Price, "Price").with_width(px(100.)))
        .column(GridColumn::new(NodeColumn::Pnl, "P&L").with_width(px(120.)))
        .column(GridColumn::new(NodeColumn::Delta, "Delta").with_width(px(120.)))
}

/// Where a row sits in the portfolio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Fund,
    Desk,
    Book,
    Position,
}

/// A row of the portfolio: a fund, a desk or a book with the sums of its
/// positions, or a position.
#[derive(Clone, Debug)]
pub struct Node {
    id: u64,
    parent: Option<u64>,
    level: Level,
    name: Arc<str>,
    /// A position's instrument; `None` for a tree row above the positions.
    instrument: Option<Arc<str>>,
    quantity: i64,
    price: f64,
    /// What a position was bought at.
    cost: f64,
    pnl: f64,
    delta: f64,
}

impl Node {
    pub fn level(&self) -> Level {
        self.level
    }

    pub fn pnl(&self) -> f64 {
        self.pnl
    }

    /// A tree row above the positions.
    fn branch(id: u64, parent: Option<u64>, level: Level, name: String) -> Self {
        Self {
            id,
            parent,
            level,
            name: name.into(),
            instrument: None,
            quantity: 0,
            price: 0.,
            cost: 0.,
            pnl: 0.,
            delta: 0.,
        }
    }

    /// A new price: the P&L and the delta follow.
    fn set_price(&mut self, price: f64) {
        self.price = price;
        self.pnl = (price - self.cost) * self.quantity as f64;
        self.delta = self.quantity as f64 * price / 100.;
    }
}

impl GridRow for Node {
    type Key = u64;
    type Column = NodeColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: NodeColumn) -> CellValue<'_> {
        let position = self.level == Level::Position;
        match column {
            NodeColumn::Name => CellValue::Text(&self.name),
            NodeColumn::Instrument => self
                .instrument
                .as_deref()
                .map_or(CellValue::Empty, CellValue::Text),
            NodeColumn::Quantity if position => CellValue::Int(self.quantity),
            NodeColumn::Price if position => CellValue::Float(self.price),
            NodeColumn::Quantity | NodeColumn::Price => CellValue::Empty,
            NodeColumn::Pnl => CellValue::Float(self.pnl),
            NodeColumn::Delta => CellValue::Float(self.delta),
        }
    }

    // #region parent-key
    fn parent_key(&self) -> Option<&u64> {
        self.parent.as_ref()
    }
    // #endregion parent-key
}

/// The producer's rates, from the environment.
#[derive(Clone, Copy)]
pub struct Feed {
    /// Positions at the first load.
    pub rows: u64,
    /// Price updates per second.
    pub rate: u64,
    /// Positions opened and closed per second.
    pub churn: u64,
    /// Positions moved to another book per second.
    pub moves: f64,
}

/// The tree rows above the positions: funds, desks and books, by key.
struct Branches {
    rows: Vec<Node>,
    /// The keys of the books, the parents of the positions.
    books: Vec<u64>,
}

impl Branches {
    fn new() -> Self {
        let mut rows = Vec::new();
        let mut books = Vec::new();
        for fund in 0..FUNDS {
            let fund_id = 1 + fund;
            rows.push(Node::branch(
                fund_id,
                None,
                Level::Fund,
                format!("Fund {}", fund + 1),
            ));
            for desk in 0..DESKS {
                let desk_ix = fund * DESKS + desk;
                let desk_id = 10 + desk_ix;
                let desk_name = char::from(b'A' + desk as u8);
                rows.push(Node::branch(
                    desk_id,
                    Some(fund_id),
                    Level::Desk,
                    format!("Desk {}{desk_name}", fund + 1),
                ));
                for book in 0..BOOKS {
                    let book_id = 100 + desk_ix * BOOKS + book;
                    rows.push(Node::branch(
                        book_id,
                        Some(desk_id),
                        Level::Book,
                        format!("Book {}{desk_name}-{}", fund + 1, book + 1),
                    ));
                    books.push(book_id);
                }
            }
        }
        Self { rows, books }
    }

    /// Adds every position's P&L and delta up its tree, and returns the
    /// tree rows whose sums changed, updated.
    fn sum(&mut self, positions: &[Node]) -> Vec<Node> {
        let mut sums: rustc_hash::FxHashMap<u64, (f64, f64)> = rustc_hash::FxHashMap::default();
        let parents: rustc_hash::FxHashMap<u64, Option<u64>> =
            self.rows.iter().map(|row| (row.id, row.parent)).collect();
        for position in positions {
            let mut parent = position.parent;
            while let Some(id) = parent {
                let sum = sums.entry(id).or_default();
                sum.0 += position.pnl;
                sum.1 += position.delta;
                parent = parents.get(&id).copied().flatten();
            }
        }
        let mut changed = Vec::new();
        for row in &mut self.rows {
            let (pnl, delta) = sums.get(&row.id).copied().unwrap_or_default();
            // Cents: a sum that moved less is not worth a write.
            if (row.pnl - pnl).abs() >= 0.005 || (row.delta - delta).abs() >= 0.005 {
                row.pnl = pnl;
                row.delta = delta;
                changed.push(row.clone());
            }
        }
        changed
    }
}

/// New positions with fresh keys.
struct Market {
    rng: fastrand::Rng,
    next_id: u64,
}

impl Market {
    fn position(&mut self, book: u64) -> Node {
        let id = self.next_id;
        self.next_id += 1;
        let instrument = INSTRUMENTS[self.rng.usize(..INSTRUMENTS.len())];
        let cost = 50. + self.rng.f64() * 450.;
        let quantity = self.rng.i64(-500..=1_000);
        let mut position = Node {
            id,
            parent: Some(book),
            level: Level::Position,
            name: format!("{instrument} #{id}").into(),
            instrument: Some(instrument.into()),
            quantity,
            price: cost,
            cost,
            pnl: 0.,
            delta: 0.,
        };
        position.set_price(cost * (1. + (self.rng.f64() - 0.5) / 20.));
        position
    }
}

/// Writes the portfolio until the store is gone; runs on its own thread.
pub fn produce(writer: GridWriter<Node>, feed: Feed) {
    let mut market = Market {
        // `PORTFOLIO_E2E_LOG` fixes the seed: the same positions every run.
        rng: crate::e2e::seed().map_or_else(fastrand::Rng::new, fastrand::Rng::with_seed),
        next_id: 10_000,
    };
    let mut branches = Branches::new();
    let books = branches.books.clone();
    let mut positions: Vec<Node> = (0..feed.rows)
        .map(|ix| market.position(books[ix as usize % books.len()]))
        .collect();
    branches.sum(&positions);
    writer.replace_all(
        branches
            .rows
            .iter()
            .chain(&positions)
            .cloned()
            .map(Arc::new),
    );

    // Batches every 10 ms: `rate` updates, `churn` positions opened and
    // closed and `moves` moved a second in total.
    let per_batch = |rate: f64| rate / 100.;
    let (mut updates_due, mut churn_due, mut moves_due) = (0., 0., 0.);
    let started = Instant::now();
    let mut batch = 0u64;
    while !writer.is_closed() {
        updates_due += per_batch(feed.rate as f64);
        while updates_due >= 1. && !positions.is_empty() {
            updates_due -= 1.;
            let ix = market.rng.usize(..positions.len());
            let position = &mut positions[ix];
            position.set_price(position.price * (1. + (market.rng.f64() - 0.5) / 200.));
            writer.upsert(position.clone());
        }
        churn_due += per_batch(feed.churn as f64);
        while churn_due >= 1. && !positions.is_empty() {
            churn_due -= 1.;
            let ix = market.rng.usize(..positions.len());
            writer.remove(positions[ix].id);
            let book = books[market.rng.usize(..books.len())];
            positions[ix] = market.position(book);
            writer.upsert(positions[ix].clone());
        }
        // A move changes the parent alone: the grid sees it as a parent
        // change and rebuilds the tree.
        moves_due += per_batch(feed.moves);
        while moves_due >= 1. && !positions.is_empty() {
            moves_due -= 1.;
            let ix = market.rng.usize(..positions.len());
            let book = books[market.rng.usize(..books.len())];
            let position = &mut positions[ix];
            position.parent = Some(book);
            writer.upsert(position.clone());
        }
        batch += 1;
        if batch % SUM_BATCHES == 0 {
            for row in branches.sum(&positions) {
                writer.upsert(row);
            }
        }
        let next = started + Duration::from_millis(batch * 10);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}
