//! The trades of the blotter and the thread that writes them: the first
//! load, quotes at `RATE` updates per second, `CHURN` trades replaced per
//! second, the Index and Total rows pinned above and below the trades every
//! 250 ms, and the snapshot a reload gets from the "server". Also the
//! grouping levels of `BLOTTER_GROUP` and the aggregates of group captions
//! and the totals.

use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::px;
use gpui_kit_grid::{
    Aggregate, CellValue, ColumnIx, ColumnZone, GridColumn, GridColumns, GridProgress, GridRow,
    GridSchema, GridWriter, GroupBy, PinnedEdge, SortDirection, StoreSnapshot,
};
use smol::channel::{Receiver, Sender};

/// Instruments `INS000`…`INS199`; the Index row averages the first ten.
const INSTRUMENTS: usize = 200;
const INDEX_INSTRUMENTS: u64 = 10;
const TRADERS: usize = 50;
const SPREAD: f64 = 0.05;
/// How often the producer writes the pinned Index and Total rows, in
/// 10 ms batches.
const SUMMARY_BATCHES: u64 = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuoteColumn {
    Id,
    Instrument,
    Side,
    Bid,
    Ask,
    Last,
    Volume,
    Trader,
}

impl GridColumns for QuoteColumn {
    fn all() -> &'static [Self] {
        use QuoteColumn::*;
        &[Id, Instrument, Side, Bid, Ask, Last, Volume, Trader]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Instrument => "instrument",
            Self::Side => "side",
            Self::Bid => "bid",
            Self::Ask => "ask",
            Self::Last => "last",
            Self::Volume => "volume",
            Self::Trader => "trader",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// The columns as in the spike runs, so the numbers compare: Id and
/// Instrument frozen on the left, Trader on the right.
pub fn schema() -> GridSchema<QuoteColumn> {
    GridSchema::new()
        .with_frozen(2)
        .column(GridColumn::new(QuoteColumn::Id, "Id").with_width(px(80.)))
        .column(GridColumn::new(QuoteColumn::Instrument, "Instrument").with_width(px(110.)))
        .column(GridColumn::new(QuoteColumn::Side, "Side").with_width(px(60.)))
        .column(GridColumn::new(QuoteColumn::Bid, "Bid"))
        .column(GridColumn::new(QuoteColumn::Ask, "Ask"))
        .column(GridColumn::new(QuoteColumn::Last, "Last"))
        .column(GridColumn::new(QuoteColumn::Volume, "Volume"))
        .column(
            GridColumn::new(QuoteColumn::Trader, "Trader")
                .with_width(px(140.))
                .with_zone(ColumnZone::FrozenRight),
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    fn label(self) -> &'static str {
        match self {
            Self::Buy => "Buy",
            Self::Sell => "Sell",
        }
    }
}

/// A trader: the login the Trader column shows, and the full name and desk
/// its tooltip shows.
pub struct Trader {
    login: String,
    name: String,
    desk: &'static str,
}

impl Trader {
    fn new(ix: usize) -> Self {
        const FIRST: [&str; 10] = [
            "Alex", "Maria", "Ivan", "Sofia", "Daniel", "Anna", "Mark", "Elena", "Paul", "Olga",
        ];
        const LAST: [&str; 5] = ["Smith", "Kim", "Novak", "Garcia", "Larsen"];
        const DESKS: [&str; 4] = ["Desk A", "Desk B", "Desk C", "Desk D"];
        Self {
            login: format!("trader{ix}"),
            name: format!(
                "{} {}",
                FIRST[ix % FIRST.len()],
                LAST[ix / FIRST.len() % LAST.len()]
            ),
            desk: DESKS[ix % DESKS.len()],
        }
    }

    /// "Anna Kim, Desk B".
    pub fn description(&self) -> String {
        format!("{}, {}", self.name, self.desk)
    }
}

/// The aggregates of group captions and the totals row: the volume traded,
/// the average last price, the lowest bid and the highest ask. The columns
/// stay `Float` and `Int`, as in the spike runs, so the numbers compare.
// #region aggregates
pub fn aggregates() -> [(QuoteColumn, Aggregate<Quote>); 4] {
    [
        (QuoteColumn::Volume, Aggregate::Sum),
        (QuoteColumn::Last, Aggregate::Avg),
        (QuoteColumn::Bid, Aggregate::Min),
        (QuoteColumn::Ask, Aggregate::Max),
    ]
}
// #endregion aggregates

/// The grouping levels of `BLOTTER_GROUP`: column ids separated by commas,
/// the top level first (`instrument,side`); a `-` after an id sorts that
/// level's groups descending (`instrument-`). Returns the levels and the
/// parts that name no column, which the caller reports and skips.
pub fn group_levels(value: &str) -> (Vec<GroupBy<Quote>>, Vec<String>) {
    let mut levels = Vec::new();
    let mut unknown = Vec::new();
    for part in value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (id, direction) = match part.strip_suffix('-') {
            Some(id) => (id.trim_end(), SortDirection::Descending),
            None => (part, SortDirection::Ascending),
        };
        match QuoteColumn::all().iter().find(|column| column.id() == id) {
            Some(column) => levels.push(GroupBy::column(*column).with_direction(direction)),
            None => unknown.push(part.to_owned()),
        }
    }
    (levels, unknown)
}

/// A trade, or a summary row pinned above or below the trades. Summary rows
/// (Index, Total) have no side, bid, ask or trader.
#[derive(Clone)]
pub struct Quote {
    /// The key, shown in the Id column. A summary row's key is of its band's
    /// own key space: the number of trades (Total) or instruments (Index) it
    /// sums up.
    id: u64,
    instrument: Arc<str>,
    side: Option<Side>,
    bid: Option<f64>,
    ask: Option<f64>,
    last: f64,
    volume: i64,
    trader: Option<Arc<Trader>>,
}

impl Quote {
    pub fn is_sell(&self) -> bool {
        self.side == Some(Side::Sell)
    }

    /// The trader of a trade; `None` for a summary row.
    pub fn trader(&self) -> Option<&Trader> {
        self.trader.as_deref()
    }

    /// This trade at another last price (the tests of the toolbar).
    #[cfg(all(test, feature = "test-support"))]
    pub fn with_last(&self, last: f64) -> Self {
        let mut trade = self.clone();
        trade.set_last(last);
        trade
    }

    /// The last price.
    #[cfg(all(test, feature = "test-support"))]
    pub fn last(&self) -> f64 {
        self.last
    }

    fn set_last(&mut self, last: f64) {
        self.last = last;
        self.bid = self.bid.map(|_| last - SPREAD);
        self.ask = self.ask.map(|_| last + SPREAD);
    }

    /// A summary row over `trades`: Volume is their sum, Last the average
    /// last price, by volume if `by_volume`.
    fn summary<'a>(
        id: u64,
        instrument: &str,
        trades: impl Iterator<Item = &'a Quote>,
        by_volume: bool,
    ) -> Self {
        let (mut count, mut volume, mut sum) = (0u64, 0i64, 0.);
        for trade in trades {
            count += 1;
            volume += trade.volume;
            sum += if by_volume {
                trade.last * trade.volume as f64
            } else {
                trade.last
            };
        }
        let weight = if by_volume {
            volume as f64
        } else {
            count as f64
        };
        Self {
            id,
            instrument: instrument.into(),
            side: None,
            bid: None,
            ask: None,
            last: if weight > 0. { sum / weight } else { 0. },
            volume,
            trader: None,
        }
    }
}

impl GridRow for Quote {
    type Key = u64;
    type Column = QuoteColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: QuoteColumn) -> CellValue<'_> {
        match column {
            QuoteColumn::Id => CellValue::Int(self.id as i64),
            QuoteColumn::Instrument => CellValue::Text(&self.instrument),
            QuoteColumn::Side => self
                .side
                .map_or(CellValue::Empty, |side| CellValue::Text(side.label())),
            QuoteColumn::Bid => self.bid.map_or(CellValue::Empty, CellValue::Float),
            QuoteColumn::Ask => self.ask.map_or(CellValue::Empty, CellValue::Float),
            QuoteColumn::Last => CellValue::Float(self.last),
            QuoteColumn::Volume => CellValue::Int(self.volume),
            QuoteColumn::Trader => self
                .trader
                .as_deref()
                .map_or(CellValue::Empty, |trader| CellValue::Text(&trader.login)),
        }
    }
}

/// The rows a reload gets from the "server": the trades of `snapshot` with
/// last prices 1 % lower, as quoted a while ago. The store replays the live
/// writes buffered during the reload over them (`ReplayLive`), so the
/// trades the stream touched meanwhile keep their live prices.
pub fn server_snapshot(snapshot: &StoreSnapshot<Quote>) -> Vec<Arc<Quote>> {
    snapshot
        .iter()
        .map(|(_, trade)| {
            let mut older = Quote::clone(trade);
            older.set_last(trade.last * 0.99);
            Arc::new(older)
        })
        .collect()
}

/// `rows` trades of a fixed seed: what the tests write instead of the
/// producer thread.
#[cfg(all(test, feature = "test-support"))]
pub fn test_trades(rows: u64) -> Vec<Arc<Quote>> {
    let mut market = Market::new(fastrand::Rng::with_seed(8));
    (0..rows).map(|_| Arc::new(market.trade())).collect()
}

/// The producer's rates, from the environment.
#[derive(Clone, Copy)]
pub struct Feed {
    /// Trades at the first load.
    pub rows: u64,
    /// Quote updates per second.
    pub rate: u64,
    /// Trades replaced per second: one removed, one inserted.
    pub churn: u64,
}

/// New trades with fresh keys.
struct Market {
    rng: fastrand::Rng,
    instruments: Vec<Arc<str>>,
    traders: Vec<Arc<Trader>>,
    next_id: u64,
}

impl Market {
    fn new(rng: fastrand::Rng) -> Self {
        Self {
            rng,
            instruments: (0..INSTRUMENTS)
                .map(|ix| Arc::from(format!("INS{ix:03}")))
                .collect(),
            traders: (0..TRADERS).map(|ix| Arc::new(Trader::new(ix))).collect(),
            next_id: 0,
        }
    }

    fn trade(&mut self) -> Quote {
        let id = self.next_id;
        self.next_id += 1;
        let last = self.rng.f64() * 1_000.;
        Quote {
            id,
            instrument: self.instruments[self.rng.usize(..self.instruments.len())].clone(),
            side: Some(if self.rng.bool() {
                Side::Sell
            } else {
                Side::Buy
            }),
            bid: Some(last - SPREAD),
            ask: Some(last + SPREAD),
            last,
            volume: self.rng.i64(0..1_000_000),
            trader: Some(self.traders[self.rng.usize(..self.traders.len())].clone()),
        }
    }
}

/// Writes the trades until the store is gone; runs on its own thread.
///
/// `loaded` is told once the first data set is written: the window then
/// waits until it is on screen (`LiveGridState::flush`) and selects the
/// first row. `cancelled` brings the keys of trades the window removed
/// (Cancel trades): the producer owns the trades, so it stops writing them.
/// Without `total_row` the Total row is not written: the grid's totals row
/// (`BLOTTER_TOTALS`) sums the trades instead.
pub fn produce(
    writer: GridWriter<Quote>,
    feed: Feed,
    total_row: bool,
    loaded: Sender<()>,
    cancelled: Receiver<Vec<u64>>,
) {
    // `BLOTTER_E2E_LOG` fixes the seed: the same trades every run.
    let mut market =
        Market::new(crate::e2e::seed().map_or_else(fastrand::Rng::new, fastrand::Rng::with_seed));

    // The first load: the progress shows over the empty grid, and leaves
    // with the frame that shows the rows.
    writer.set_progress(Some(
        GridProgress::indeterminate().with_label("Loading trades"),
    ));
    let mut model: Vec<Quote> = (0..feed.rows).map(|_| market.trade()).collect();
    writer.replace_all(model.iter().cloned().map(Arc::new));
    writer.set_progress(None);
    write_summaries(&writer, &model, total_row);
    let _ = loaded.try_send(());

    // Batches every 10 ms: `rate` updates and `churn` replaced trades per
    // second in total. Under 100 a second some batches write none; `RATE=0`
    // writes no updates at all.
    let updates_per_batch = feed.rate as f64 / 100.;
    let churn_per_batch = feed.churn as f64 / 100.;
    let (mut updates_due, mut churn_due) = (0., 0.);
    let started = Instant::now();
    let mut batch = 0u64;
    while !writer.is_closed() {
        while let Ok(keys) = cancelled.try_recv() {
            let keys: HashSet<u64> = keys.into_iter().collect();
            model.retain(|trade| !keys.contains(&trade.id));
            // This thread may have written one after the window removed it.
            for key in keys {
                writer.remove(key);
            }
        }
        if !model.is_empty() {
            updates_due += updates_per_batch;
            while updates_due >= 1. {
                updates_due -= 1.;
                let ix = market.rng.usize(..model.len());
                let trade = &mut model[ix];
                trade.set_last(trade.last * (1. + (market.rng.f64() - 0.5) / 200.));
                trade.volume += market.rng.i64(0..100);
                writer.upsert(trade.clone());
            }
            churn_due += churn_per_batch;
            while churn_due >= 1. {
                churn_due -= 1.;
                let ix = market.rng.usize(..model.len());
                writer.remove(model[ix].id);
                model[ix] = market.trade();
                writer.upsert(model[ix].clone());
            }
        }
        batch += 1;
        if batch % SUMMARY_BATCHES == 0 {
            write_summaries(&writer, &model, total_row);
        }
        let next = started + Duration::from_millis(batch * 10);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

/// Pins Index (the average last price of `INS000`…`INS009`) above the
/// trades and, with `total_row`, Total (the trade count, the volume and the
/// volume-weighted average last price) below them.
fn write_summaries(writer: &GridWriter<Quote>, model: &[Quote], total_row: bool) {
    let in_index = |trade: &&Quote| {
        trade
            .instrument
            .strip_prefix("INS")
            .and_then(|number| number.parse::<u64>().ok())
            .is_some_and(|number| number < INDEX_INSTRUMENTS)
    };
    let index = Quote::summary(
        INDEX_INSTRUMENTS,
        "Index",
        model.iter().filter(in_index),
        false,
    );
    writer.set_pinned_rows(PinnedEdge::Top, [Arc::new(index)]);
    if total_row {
        let total = Quote::summary(model.len() as u64, "Total", model.iter(), true);
        writer.set_pinned_rows(PinnedEdge::Bottom, [Arc::new(total)]);
    }
}
