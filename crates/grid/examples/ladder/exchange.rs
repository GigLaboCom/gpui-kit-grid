//! The simulated futures exchange: every contract of `CONTRACTS` trades on
//! its own thread, with a ten-level book, a session volume profile, price
//! bars and the trader's working orders, fills and positions. Free of
//! GPUI: it writes the grid stores' writers it is given and publishes its
//! status and the current contract's bars; the window sends it
//! [`Request`]s.
//!
//! **Everything it makes is simulated.** A trade is an aggressor that takes
//! the best ask or hits the best bid; when a level is gone the inside steps
//! a tick, so the price moves the way a book is eaten. The trader's limit
//! orders wait in the queue of their price: one fills once the volume
//! traded there since it was placed exceeds the size that rested ahead of
//! it, or as soon as the price trades through it. A market order, or a
//! limit at or through the market, takes the inside at once.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui_kit_grid::GridWriter;
use parking_lot::Mutex;
use smol::channel::{Receiver, Sender};

use crate::{
    rows::{FillRow, LadderRow, OrderRow, TradeRow},
    trading::{
        chart::{ChartBars, ChartSource, ChartSubject},
        instruments::{CONTRACTS, Contract, ContractId},
        market::{BAR_HISTORY, BAR_SECONDS, utc_time},
        rows::{Bar, Side},
    },
};

/// Trades per second of each contract unless `LADDER_RATE` says otherwise.
pub const DEFAULT_RATE: f64 = 8.;
/// The highest rate `LADDER_RATE` takes.
pub const MAX_RATE: f64 = 2_000.;
/// Levels on each side of a book.
pub const LEVELS: usize = 10;
/// Prices above and below the anchor of the ladder's rows: the band the
/// ladder scrolls through.
pub const HALF_BAND: i64 = 200;
/// The ladder's rows are laid out again around the last price once it is
/// this many ticks from their anchor.
const REANCHOR: i64 = 150;
/// Trades the time and sales keeps, per contract.
pub const TAPE_KEPT: usize = 200;
/// Trades of a history bar: the live rate's, so live bars match.
const HISTORY_TRADES: usize = (DEFAULT_RATE as usize) * BAR_SECONDS as usize;
/// How far from its seed, in ticks, the order flow leans back hard.
const REVERSION_TICKS: f64 = 500.;
/// The step of a depth or profile share: they change in sixteenths, so a
/// new largest level does not rewrite every row.
const SHARE_STEPS: f32 = 16.;

/// What the window asks the exchange for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Show this contract on the ladder, the time and sales and the chart.
    Select(ContractId),
    /// A limit order; at or through the market it fills at once.
    Limit {
        contract: ContractId,
        side: Side,
        tick: i64,
        quantity: i64,
    },
    /// A market order: it takes the inside.
    Market {
        contract: ContractId,
        side: Side,
        quantity: i64,
    },
    /// Cancels the working orders of `side` at `tick`.
    Cancel {
        contract: ContractId,
        side: Side,
        tick: i64,
    },
    /// Cancels every working order, in every contract.
    CancelAll,
}

/// The trader's position in one contract: net contracts (negative is
/// short), their average price in ticks, and the P&L realised so far.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Position {
    pub quantity: i64,
    pub average: f64,
    pub realized: f64,
}

impl Position {
    /// Books a fill of `quantity` contracts at `tick`; returns the P&L it
    /// realised, in US dollars, if it closed any.
    pub fn fill(
        &mut self,
        contract: &Contract,
        side: Side,
        quantity: i64,
        tick: i64,
    ) -> Option<f64> {
        let signed = match side {
            Side::Buy => quantity,
            Side::Sell => -quantity,
        };
        let price = tick as f64;
        if self.quantity == 0 || self.quantity.signum() == signed.signum() {
            let total = self.quantity + signed;
            self.average = (self.average * self.quantity.abs() as f64 + price * quantity as f64)
                / total.abs() as f64;
            self.quantity = total;
            return None;
        }
        let closed = quantity.min(self.quantity.abs());
        let pnl = contract.value((price - self.average) * (closed * self.quantity.signum()) as f64);
        self.realized += pnl;
        let total = self.quantity + signed;
        if total == 0 {
            self.average = 0.;
        } else if total.signum() != self.quantity.signum() {
            // Turned over: the rest opens at the fill's price.
            self.average = price;
        }
        self.quantity = total;
        Some(pnl)
    }

    /// The P&L of the open contracts at `last`, in US dollars.
    pub fn open_pnl(&self, contract: &Contract, last: i64) -> f64 {
        contract.value((last as f64 - self.average) * self.quantity as f64)
    }
}

/// One contract's position and last price.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Holding {
    pub position: Position,
    pub last: i64,
}

/// What the exchange last published for the window.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// The contract on the ladder.
    pub contract: Option<ContractId>,
    pub last: i64,
    pub bid: i64,
    pub ask: i64,
    /// Grows each time the ladder's rows are laid out anew (another
    /// contract, or the price far from their anchor): the ladder centres
    /// the last price then.
    pub band: u64,
    /// Every contract's position, in `CONTRACTS` order.
    pub holdings: Vec<Holding>,
    /// Working orders, in every contract.
    pub working: usize,
    /// Grows with every publication.
    pub version: u64,
}

impl Status {
    /// The open and realised P&L of every contract, in US dollars.
    pub fn total_pnl(&self) -> (f64, f64) {
        self.holdings.iter().zip(ContractId::all()).fold(
            (0., 0.),
            |(open, realized), (holding, id)| {
                (
                    open + holding.position.open_pnl(id.get(), holding.last),
                    realized + holding.position.realized,
                )
            },
        )
    }
}

/// Where the exchange writes.
pub struct Outputs {
    pub ladder: GridWriter<LadderRow>,
    pub tape: GridWriter<TradeRow>,
    pub orders: GridWriter<OrderRow>,
    pub fills: GridWriter<FillRow>,
}

impl Outputs {
    /// True once every store is gone: the exchange stops.
    fn is_closed(&self) -> bool {
        self.ladder.is_closed()
            && self.tape.is_closed()
            && self.orders.is_closed()
            && self.fills.is_closed()
    }
}

/// The window's side of the exchange: requests in, status and bars out.
#[derive(Clone)]
pub struct ExchangeLink {
    requests: Sender<Request>,
    status: Arc<Mutex<Status>>,
    chart: Arc<Mutex<ChartBars>>,
}

impl ExchangeLink {
    pub fn send(&self, request: Request) {
        let _ = self.requests.try_send(request);
    }

    /// The status as last published (every 50 ms).
    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }
}

/// The current contract's bars, published every 100 ms.
impl ChartSource for ExchangeLink {
    fn bars(&self) -> ChartBars {
        self.chart.lock().clone()
    }
}

/// Starts the exchange on its own thread with `first` on the ladder:
/// `rate` trades a second per contract, the same market for the same
/// `seed`. It stops once every store in `outputs` is gone.
pub fn start(seed: Option<u64>, rate: f64, first: ContractId, outputs: Outputs) -> ExchangeLink {
    let (requests, receiver) = smol::channel::unbounded();
    let link = ExchangeLink {
        requests,
        status: Arc::default(),
        chart: Arc::default(),
    };
    let shared = link.clone();
    // The history of every contract is built on the exchange's thread.
    std::thread::spawn(move || {
        let mut exchange = Exchange::new(seed, rate.clamp(0., MAX_RATE));
        exchange.select(first, &outputs);
        run(exchange, outputs, receiver, shared);
    });
    link
}

/// Traded at one price this session.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Traded {
    volume: i64,
    bought: i64,
}

/// One contract's market: the inside, ten levels a side, the volume
/// profile, the bars and the latest trades.
struct Book {
    id: ContractId,
    seed: i64,
    /// The best bid; the best ask is a tick above it.
    bid: i64,
    last: i64,
    /// Resting sizes, best first.
    bids: [i64; LEVELS],
    asks: [i64; LEVELS],
    profile: HashMap<i64, Traded>,
    /// The price traded most, and its volume.
    poc: i64,
    max_volume: i64,
    /// The order flow's lean, -0.15 to 0.15: positive takes the ask more.
    flow: f64,
    bars: VecDeque<Bar>,
    trades: VecDeque<Arc<TradeRow>>,
    /// Trades owed by the rate, carried between batches.
    due: f64,
}

impl Book {
    /// A book at the contract's seed with `BAR_HISTORY` bars of history,
    /// whose trades fill the volume profile.
    fn new(id: ContractId, rng: &mut fastrand::Rng) -> Self {
        let contract = id.get();
        let seed = contract.ticks(contract.seed);
        let mut book = Self {
            id,
            seed,
            bid: seed,
            last: seed,
            bids: [0; LEVELS],
            asks: [0; LEVELS],
            profile: HashMap::new(),
            poc: seed,
            max_volume: 0,
            flow: 0.,
            bars: VecDeque::with_capacity(BAR_HISTORY + 1),
            trades: VecDeque::with_capacity(TAPE_KEPT + 1),
            due: 0.,
        };
        for level in 0..LEVELS {
            book.bids[level] = book.fresh_size(rng, level);
            book.asks[level] = book.fresh_size(rng, level);
        }
        for _ in 0..BAR_HISTORY {
            let mut bar = Bar::new(contract.price(book.last));
            for _ in 0..HISTORY_TRADES {
                let (side, size) = book.next_aggressor(rng);
                let tick = book.aggress(rng, side, size);
                bar.trade(contract.price(tick), size as f64);
            }
            book.bars.push_back(bar);
            book.refresh(rng);
        }
        book
    }

    fn ask(&self) -> i64 {
        self.bid + 1
    }

    /// A new size for the level `level` deep: deeper levels rest larger.
    fn fresh_size(&self, rng: &mut fastrand::Rng, level: usize) -> i64 {
        let mean = self.id.get().depth * (4 + level as i64) / 4;
        rng.i64((mean / 2).max(1)..=(mean * 3 / 2).max(1))
    }

    /// The side and size of the next trade: mostly small, now and then a
    /// block; buyers more often while the flow leans up, and the further
    /// the price from its seed, the more the flow leans back.
    fn next_aggressor(&mut self, rng: &mut fastrand::Rng) -> (Side, i64) {
        self.flow = (self.flow * 0.995 + (rng.f64() - 0.5) * 0.02).clamp(-0.15, 0.15);
        let away = (self.last - self.seed) as f64 / REVERSION_TICKS;
        let buy = (0.5 + self.flow - 0.3 * away).clamp(0.1, 0.9);
        let side = if rng.f64() < buy {
            Side::Buy
        } else {
            Side::Sell
        };
        let unit = (self.id.get().depth / 12).max(1);
        let lots = match rng.u8(..100) {
            0..60 => rng.i64(1..=2),
            60..93 => rng.i64(3..=8),
            _ => rng.i64(10..=30),
        };
        (side, lots * unit)
    }

    /// An aggressor of `side` takes `size` at the inside: the trade prints
    /// at the best ask for a buyer, the best bid for a seller. A level
    /// taken whole moves the inside a tick that way, with fresh size behind
    /// it. Returns the trade's price.
    fn aggress(&mut self, rng: &mut fastrand::Rng, side: Side, size: i64) -> i64 {
        let tick = match side {
            Side::Buy => {
                let tick = self.ask();
                self.asks[0] -= size;
                if self.asks[0] <= 0 {
                    self.asks.rotate_left(1);
                    self.asks[LEVELS - 1] = self.fresh_size(rng, LEVELS - 1);
                    self.bids.rotate_right(1);
                    self.bids[0] = self.fresh_size(rng, 0) / 2 + 1;
                    self.bid += 1;
                }
                tick
            }
            Side::Sell => {
                let tick = self.bid;
                self.bids[0] -= size;
                if self.bids[0] <= 0 {
                    self.bids.rotate_left(1);
                    self.bids[LEVELS - 1] = self.fresh_size(rng, LEVELS - 1);
                    self.asks.rotate_right(1);
                    self.asks[0] = self.fresh_size(rng, 0) / 2 + 1;
                    self.bid -= 1;
                }
                tick
            }
        };
        let traded = self.profile.entry(tick).or_default();
        traded.volume += size;
        if side == Side::Buy {
            traded.bought += size;
        }
        if traded.volume > self.max_volume {
            self.max_volume = traded.volume;
            self.poc = tick;
        }
        self.last = tick;
        tick
    }

    /// Every 50 ms: a few levels get a new size and a few drift, so the
    /// book breathes between trades.
    fn refresh(&mut self, rng: &mut fastrand::Rng) {
        for level in 0..LEVELS {
            for side in [Side::Buy, Side::Sell] {
                let fresh = self.fresh_size(rng, level);
                let slot = match side {
                    Side::Buy => &mut self.bids[level],
                    Side::Sell => &mut self.asks[level],
                };
                match rng.u8(..60) {
                    0 => *slot = fresh,
                    1..4 => *slot = (*slot + rng.i64(-fresh / 4..=fresh / 4)).max(1),
                    _ => {}
                }
            }
        }
    }

    /// The size resting at `tick` on `side`; 0 beyond the book's ten
    /// levels.
    fn resting(&self, side: Side, tick: i64) -> i64 {
        let (levels, depth) = match side {
            Side::Buy => (&self.bids, self.bid - tick),
            Side::Sell => (&self.asks, tick - self.ask()),
        };
        usize::try_from(depth)
            .ok()
            .and_then(|depth| levels.get(depth).copied())
            .unwrap_or(0)
    }

    /// The largest resting level of the book.
    fn largest_level(&self) -> i64 {
        self.bids
            .iter()
            .chain(&self.asks)
            .copied()
            .max()
            .unwrap_or(1)
            .max(1)
    }

    fn subject(&self) -> ChartSubject {
        let contract = self.id.get();
        ChartSubject {
            symbol: contract.symbol,
            name: contract.name,
            reference: contract.seed,
            tick: contract.tick,
        }
    }
}

/// `value` over `whole`, in sixteenths.
fn share(value: i64, whole: i64) -> f32 {
    if whole <= 0 {
        return 0.;
    }
    ((value as f32 / whole as f32) * SHARE_STEPS).round() / SHARE_STEPS
}

/// A working limit order.
struct Order {
    row: Arc<OrderRow>,
    /// The size that rested ahead of it at its price, less what traded
    /// there since.
    ahead: i64,
}

/// The ladder's rows of the current contract: a band of prices around an
/// anchor, highest first in `rows`' reverse.
struct Band {
    anchor: i64,
    /// The row of every tick from `anchor - HALF_BAND`, upwards.
    rows: Vec<Arc<LadderRow>>,
}

impl Band {
    /// Blank rows of `id` from `anchor - HALF_BAND` to `anchor + HALF_BAND`,
    /// each with its price's text.
    fn around(id: ContractId, anchor: i64) -> Self {
        let contract = id.get();
        let rows = (anchor - HALF_BAND..=anchor + HALF_BAND)
            .map(|tick| {
                Arc::new(LadderRow {
                    tick,
                    price: contract.format(tick).into(),
                    bid: 0,
                    ask: 0,
                    buys: 0,
                    sells: 0,
                    volume: 0,
                    bought: 0,
                    depth: 0.,
                    profile: 0.,
                    last: false,
                    best_bid: false,
                    best_ask: false,
                    poc: false,
                })
            })
            .collect();
        Self { anchor, rows }
    }

    fn low(&self) -> i64 {
        self.anchor - HALF_BAND
    }
}

/// What a batch changed, written to the stores at its end.
#[derive(Default)]
struct Pending {
    trades: Vec<Arc<TradeRow>>,
    expired: Vec<u64>,
    placed: Vec<Arc<OrderRow>>,
    gone: Vec<u64>,
    fills: Vec<FillRow>,
}

/// The exchange: every contract's book, the trader's orders and positions.
pub struct Exchange {
    rng: fastrand::Rng,
    rate: f64,
    books: Vec<Book>,
    positions: Vec<Position>,
    orders: Vec<Order>,
    current: Option<ContractId>,
    band: Option<Band>,
    /// Laid-out bands so far, for `Status::band`.
    layouts: u64,
    next_order: u64,
    next_fill: u64,
    next_trade: u64,
    /// When the open bar started, in Unix seconds.
    bar_time: u64,
    /// The time of the trades of this batch, UTC `hh:mm:ss.mmm`.
    time: Arc<str>,
    pending: Pending,
}

impl Exchange {
    /// Every contract opened near its seed with two hours of history; the
    /// same `seed` makes the same market.
    pub fn new(seed: Option<u64>, rate: f64) -> Self {
        let mut rng = seed.map_or_else(fastrand::Rng::new, fastrand::Rng::with_seed);
        let books = ContractId::all()
            .map(|id| Book::new(id, &mut rng))
            .collect();
        Self {
            rng,
            rate,
            books,
            positions: vec![Position::default(); CONTRACTS.len()],
            orders: Vec::new(),
            current: None,
            band: None,
            layouts: 0,
            next_order: 1,
            next_fill: 1,
            next_trade: 0,
            bar_time: bar_start(SystemTime::now()),
            time: utc_time(SystemTime::now(), true),
            pending: Pending::default(),
        }
    }

    fn book(&self, id: ContractId) -> &Book {
        &self.books[id.0 as usize]
    }

    /// Shows `id` on the ladder and the time and sales.
    fn select(&mut self, id: ContractId, outputs: &Outputs) {
        self.current = Some(id);
        self.lay_out(outputs);
        let trades: Vec<Arc<TradeRow>> = self.book(id).trades.iter().cloned().collect();
        outputs.tape.replace_all(trades);
        self.pending.trades.clear();
        self.pending.expired.clear();
    }

    /// Lays the ladder's rows out around the current contract's last price.
    fn lay_out(&mut self, outputs: &Outputs) {
        let Some(id) = self.current else {
            return;
        };
        self.band = Some(Band::around(id, self.book(id).last));
        self.layouts += 1;
        let mut changed = Vec::new();
        self.update_rows(&mut changed);
        let rows = self.band.as_ref().map(|band| band.rows.clone());
        outputs.ladder.replace_all(rows.into_iter().flatten());
    }

    /// Brings every row of the band up to date; pushes the rows that
    /// changed onto `changed`.
    fn update_rows(&mut self, changed: &mut Vec<Arc<LadderRow>>) {
        let Some(id) = self.current else {
            return;
        };
        let book = &self.books[id.0 as usize];
        let Some(band) = &mut self.band else {
            return;
        };
        let largest = book.largest_level();
        let mut working: Vec<(Side, i64, i64)> = Vec::new();
        for order in &self.orders {
            let row = &order.row;
            if row.contract == id {
                working.push((row.side, row.tick, row.quantity));
            }
        }
        let low = band.low();
        for (ix, slot) in band.rows.iter_mut().enumerate() {
            let tick = low + ix as i64;
            let (bid, ask) = (
                book.resting(Side::Buy, tick),
                book.resting(Side::Sell, tick),
            );
            let traded = book.profile.get(&tick).copied().unwrap_or_default();
            let mine = |side: Side| {
                working
                    .iter()
                    .filter(|(own, at, _)| *own == side && *at == tick)
                    .map(|(_, _, quantity)| quantity)
                    .sum::<i64>()
            };
            let row = LadderRow {
                tick,
                price: slot.price.clone(),
                bid,
                ask,
                buys: mine(Side::Buy),
                sells: mine(Side::Sell),
                volume: traded.volume,
                bought: traded.bought,
                depth: share(bid.max(ask), largest),
                profile: share(traded.volume, book.max_volume),
                last: tick == book.last,
                best_bid: tick == book.bid,
                best_ask: tick == book.ask(),
                poc: tick == book.poc && book.max_volume > 0,
            };
            if **slot != row {
                *slot = Arc::new(row);
                changed.push(slot.clone());
            }
        }
    }

    /// Writes the ladder: laid out anew if the price left the band's
    /// middle, else the rows that changed.
    fn write_ladder(&mut self, outputs: &Outputs) {
        let Some(id) = self.current else {
            return;
        };
        let last = self.book(id).last;
        if self
            .band
            .as_ref()
            .is_none_or(|band| (last - band.anchor).abs() > REANCHOR)
        {
            self.lay_out(outputs);
            return;
        }
        let mut changed = Vec::new();
        self.update_rows(&mut changed);
        if !changed.is_empty() {
            outputs.ladder.batch(|rows| {
                for row in changed {
                    rows.upsert(row);
                }
            });
        }
    }

    /// Handles one request of the window.
    pub fn handle(&mut self, request: Request, outputs: &Outputs) {
        match request {
            Request::Select(id) => self.select(id, outputs),
            request => self.execute(request),
        }
    }

    /// Places or cancels orders; the stores learn of it at the batch's
    /// end.
    fn execute(&mut self, request: Request) {
        match request {
            // Writes the stores at once: `handle`.
            Request::Select(_) => {}
            Request::Limit {
                contract,
                side,
                tick,
                quantity,
            } => {
                let book = self.book(contract);
                let marketable = match side {
                    Side::Buy => tick >= book.ask(),
                    Side::Sell => tick <= book.bid,
                };
                if marketable {
                    self.market(contract, side, quantity);
                    return;
                }
                let ahead = book.resting(side, tick);
                let row = Arc::new(OrderRow {
                    id: self.next_order,
                    time: utc_time(SystemTime::now(), false),
                    contract,
                    side,
                    quantity,
                    tick,
                    price: contract.get().format(tick).into(),
                });
                self.next_order += 1;
                self.pending.placed.push(row.clone());
                self.orders.push(Order { row, ahead });
            }
            Request::Market {
                contract,
                side,
                quantity,
            } => self.market(contract, side, quantity),
            Request::Cancel {
                contract,
                side,
                tick,
            } => {
                self.cancel(|row| row.contract == contract && row.side == side && row.tick == tick)
            }
            Request::CancelAll => self.cancel(|_| true),
        }
    }

    fn cancel(&mut self, matches: impl Fn(&OrderRow) -> bool) {
        let gone = &mut self.pending.gone;
        self.orders.retain(|order| {
            let keep = !matches(&order.row);
            if !keep {
                gone.push(order.row.id);
            }
            keep
        });
    }

    /// The trader takes the inside: a trade on the tape, filled at once.
    fn market(&mut self, contract: ContractId, side: Side, quantity: i64) {
        self.time = utc_time(SystemTime::now(), true);
        let tick = self.trade(contract, side, quantity, true);
        self.fill(contract, side, quantity, tick);
    }

    /// One trade of `contract`: the book, the bar, the time and sales, and,
    /// unless the trader's own, the working orders it fills.
    fn trade(&mut self, contract: ContractId, side: Side, size: i64, own: bool) -> i64 {
        let book = &mut self.books[contract.0 as usize];
        let tick = book.aggress(&mut self.rng, side, size);
        let price = contract.get().price(tick);
        if let Some(bar) = book.bars.back_mut() {
            bar.trade(price, size as f64);
        }
        let row = Arc::new(TradeRow {
            seq: self.next_trade,
            time: self.time.clone(),
            price: contract.get().format(tick).into(),
            size,
            side,
            own,
            block: size >= contract.get().depth / 2,
        });
        self.next_trade += 1;
        book.trades.push_back(row.clone());
        let expired = (book.trades.len() > TAPE_KEPT).then(|| book.trades.pop_front());
        if self.current == Some(contract) {
            self.pending.trades.push(row);
            if let Some(Some(old)) = expired {
                self.pending.expired.push(old.seq);
            }
        }
        if !own {
            self.match_orders(contract, side, tick, size);
        }
        tick
    }

    /// Fills the working orders of `contract` that a trade of `size` at
    /// `tick` reaches: those it trades through, and those at its price
    /// whose queue ahead it used up.
    fn match_orders(&mut self, contract: ContractId, aggressor: Side, tick: i64, size: i64) {
        let mut filled = Vec::new();
        self.orders.retain_mut(|order| {
            let row = &order.row;
            if row.contract != contract {
                return true;
            }
            let (through, at_price) = match row.side {
                Side::Buy => (tick < row.tick, tick == row.tick && aggressor == Side::Sell),
                Side::Sell => (tick > row.tick, tick == row.tick && aggressor == Side::Buy),
            };
            if at_price {
                order.ahead -= size;
            }
            let fills = through || (at_price && order.ahead < 0);
            if fills {
                filled.push(row.clone());
            }
            !fills
        });
        for row in filled {
            self.pending.gone.push(row.id);
            self.fill(row.contract, row.side, row.quantity, row.tick);
        }
    }

    fn fill(&mut self, contract: ContractId, side: Side, quantity: i64, tick: i64) {
        let realized =
            self.positions[contract.0 as usize].fill(contract.get(), side, quantity, tick);
        self.pending.fills.push(FillRow {
            id: self.next_fill,
            time: utc_time(SystemTime::now(), false),
            contract,
            side,
            quantity,
            price: contract.get().format(tick).into(),
            realized,
        });
        self.next_fill += 1;
    }

    /// One batch of `seconds`: each contract's trades at the rate, and a
    /// breath of its book every `refresh`th batch.
    fn step(&mut self, seconds: f64, refresh: bool) {
        self.time = utc_time(SystemTime::now(), true);
        for ix in 0..self.books.len() {
            let id = ContractId(ix as u8);
            let book = &mut self.books[ix];
            // Trades arrive unevenly: some batches have none, some several.
            book.due += self.rate * seconds * self.rng.f64() * 2.;
            let count = book.due as u64;
            book.due -= count as f64;
            for _ in 0..count {
                let (side, size) = self.books[ix].next_aggressor(&mut self.rng);
                self.trade(id, side, size, false);
            }
            if refresh {
                self.books[ix].refresh(&mut self.rng);
            }
        }
    }

    /// Opens the bar that starts at `time` (Unix seconds) for every
    /// contract at its last price, and those a stall missed.
    fn open_bars(&mut self, time: u64) {
        let missing = (time.saturating_sub(self.bar_time) / BAR_SECONDS).min(BAR_HISTORY as u64);
        if missing == 0 {
            return;
        }
        self.bar_time = time;
        for book in &mut self.books {
            let price = book.id.get().price(book.last);
            for _ in 0..missing {
                book.bars.push_back(Bar::new(price));
            }
            while book.bars.len() > BAR_HISTORY {
                book.bars.pop_front();
            }
        }
    }

    /// Writes what the batch changed: the time and sales, the orders and
    /// the fills.
    fn flush(&mut self, outputs: &Outputs) {
        let pending = std::mem::take(&mut self.pending);
        if !pending.trades.is_empty() || !pending.expired.is_empty() {
            outputs.tape.batch(|tape| {
                for seq in pending.expired {
                    tape.remove(seq);
                }
                for row in pending.trades {
                    tape.upsert(row);
                }
            });
        }
        if !pending.placed.is_empty() || !pending.gone.is_empty() {
            outputs.orders.batch(|orders| {
                for row in pending.placed {
                    orders.upsert(row);
                }
                for id in pending.gone {
                    orders.remove(id);
                }
            });
        }
        if !pending.fills.is_empty() {
            outputs.fills.batch(|fills| {
                for row in pending.fills {
                    fills.upsert(row);
                }
            });
        }
    }

    fn status(&self, version: u64) -> Status {
        let book = self.current.map(|id| self.book(id));
        Status {
            contract: self.current,
            last: book.map_or(0, |book| book.last),
            bid: book.map_or(0, |book| book.bid),
            ask: book.map_or(0, Book::ask),
            band: self.layouts,
            holdings: self
                .books
                .iter()
                .zip(&self.positions)
                .map(|(book, position)| Holding {
                    position: *position,
                    last: book.last,
                })
                .collect(),
            working: self.orders.len(),
            version,
        }
    }

    fn chart(&self, version: u64) -> ChartBars {
        match self.current {
            Some(id) => {
                let book = self.book(id);
                ChartBars {
                    subject: Some(book.subject()),
                    bars: book.bars.iter().copied().collect(),
                    start: self.bar_time - (book.bars.len() as u64 - 1) * BAR_SECONDS,
                    version,
                }
            }
            None => ChartBars::default(),
        }
    }
}

/// The start of the bar `time` falls in, in Unix seconds.
fn bar_start(time: SystemTime) -> u64 {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    seconds - seconds % BAR_SECONDS
}

/// Runs `exchange` until every store in `outputs` is gone: 10 ms batches;
/// the ladder written every 20 ms, the status every 50 ms and the chart
/// every 100 ms.
fn run(mut exchange: Exchange, outputs: Outputs, requests: Receiver<Request>, link: ExchangeLink) {
    const BATCH: Duration = Duration::from_millis(10);
    let started = Instant::now();
    let mut batch = 0u64;
    let mut version = 0;
    let mut publish = |exchange: &Exchange, chart: bool| {
        version += 1;
        *link.status.lock() = exchange.status(version);
        if chart {
            *link.chart.lock() = exchange.chart(version);
        }
    };
    publish(&exchange, true);
    while !outputs.is_closed() {
        let mut asked = false;
        while let Ok(request) = requests.try_recv() {
            exchange.handle(request, &outputs);
            asked = true;
        }
        exchange.step(BATCH.as_secs_f64(), batch % 5 == 0);
        exchange.open_bars(bar_start(SystemTime::now()));
        if asked || batch % 2 == 0 {
            exchange.write_ladder(&outputs);
        }
        exchange.flush(&outputs);
        batch += 1;
        if asked || batch % 5 == 0 {
            // A new contract's bars at once.
            publish(&exchange, asked || batch % 10 == 0);
        }

        let next = started + BATCH * batch as u32;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > Duration::from_millis(200) {
            // Too far behind (a stalled machine): do not catch up in a burst.
            batch = (started.elapsed().as_millis() / 10) as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn es() -> ContractId {
        ContractId::find("ES").expect("ES")
    }

    #[test]
    fn test_fills_net_into_a_position_and_realise_pnl() {
        let contract = es().get();
        let mut position = Position::default();
        assert_eq!(position.fill(contract, Side::Buy, 2, 100), None);
        assert_eq!(position.fill(contract, Side::Buy, 2, 104), None);
        assert_eq!((position.quantity, position.average), (4, 102.));
        // Four ticks of ES are a point, $50 a contract.
        assert_eq!(position.open_pnl(contract, 106), 4. * 4. * 12.5);
        assert_eq!(position.fill(contract, Side::Sell, 1, 110), Some(100.));
        assert_eq!((position.quantity, position.average), (3, 102.));
        // Through flat: the rest is short at the fill's price.
        assert_eq!(position.fill(contract, Side::Sell, 5, 98), Some(-150.));
        assert_eq!((position.quantity, position.average), (-2, 98.));
        assert_eq!(position.realized, -50.);
        assert_eq!(position.open_pnl(contract, 96), 50.);
    }

    #[test]
    fn test_taking_a_level_moves_the_inside_a_tick() {
        let mut rng = fastrand::Rng::with_seed(7);
        let mut book = Book::new(es(), &mut rng);
        let (bid, ask) = (book.bid, book.ask());
        assert_eq!(ask, bid + 1);
        let size = book.asks[0];
        assert_eq!(book.aggress(&mut rng, Side::Buy, size), ask);
        assert_eq!((book.bid, book.ask(), book.last), (ask, ask + 1, ask));
        let size = book.bids[0];
        assert_eq!(book.aggress(&mut rng, Side::Sell, size), ask);
        assert_eq!(book.bid, bid);
        // The volume profile counted both, one of them bought.
        let traded = book.profile[&ask];
        assert!(traded.volume >= traded.bought && traded.bought > 0);
        assert!(book.max_volume >= traded.volume);
        assert!(book.bids.iter().chain(&book.asks).all(|size| *size > 0));
    }

    #[test]
    fn test_limit_orders_wait_their_turn_or_fill_through() {
        let mut exchange = Exchange::new(Some(7), 0.);
        let id = es();
        exchange.current = Some(id);
        let bid = exchange.book(id).bid;
        let ahead = exchange.book(id).bids[0];
        let buy = |exchange: &mut Exchange, tick| {
            let request = Request::Limit {
                contract: id,
                side: Side::Buy,
                tick,
                quantity: 2,
            };
            exchange.execute(request);
        };
        // At the bid, behind the size resting there; two ticks under it.
        buy(&mut exchange, bid);
        buy(&mut exchange, bid - 2);
        assert_eq!(exchange.orders.len(), 2);
        assert_eq!(exchange.orders[0].ahead, ahead);
        // A seller takes less than the queue: nothing fills.
        exchange.trade(id, Side::Sell, ahead - 1, false);
        assert_eq!(exchange.orders.len(), 2);
        // The rest of the queue and one more: the order at the bid fills.
        exchange.trade(id, Side::Sell, 2, false);
        assert_eq!(exchange.orders.len(), 1);
        assert_eq!(exchange.positions[0].quantity, 2);
        // Sellers trade through the lower order: it fills at its price.
        while exchange.book(id).last >= bid - 2 {
            let size = exchange.book(id).bids[0];
            exchange.trade(id, Side::Sell, size, false);
        }
        assert!(exchange.orders.is_empty());
        assert_eq!(exchange.positions[0].quantity, 4);
        assert_eq!(
            exchange.positions[0].average,
            (2 * bid + 2 * (bid - 2)) as f64 / 4.
        );
        assert_eq!(exchange.pending.fills.len(), 2);
        assert_eq!(exchange.pending.gone, [1, 2]);
    }

    #[test]
    fn test_marketable_limits_and_market_orders_take_the_inside() {
        let mut exchange = Exchange::new(Some(7), 0.);
        let id = es();
        exchange.current = Some(id);
        let ask = exchange.book(id).ask();
        exchange.execute(Request::Limit {
            contract: id,
            side: Side::Buy,
            tick: ask + 5,
            quantity: 1,
        });
        assert!(exchange.orders.is_empty());
        assert_eq!(exchange.positions[0].average, ask as f64);
        let bid = exchange.book(id).bid;
        exchange.execute(Request::Market {
            contract: id,
            side: Side::Sell,
            quantity: 1,
        });
        assert_eq!(exchange.positions[0].quantity, 0);
        assert_eq!(
            exchange.positions[0].realized,
            id.get().value((bid - ask) as f64)
        );
        // Both are on the time and sales, as the trader's own.
        assert_eq!(exchange.pending.trades.len(), 2);
        assert!(exchange.pending.trades.iter().all(|trade| trade.own));
    }

    #[test]
    fn test_cancels_take_orders_off_by_price_and_side_or_all() {
        let mut exchange = Exchange::new(Some(7), 0.);
        let id = es();
        exchange.current = Some(id);
        let (bid, ask) = (exchange.book(id).bid, exchange.book(id).ask());
        for (side, tick) in [
            (Side::Buy, bid - 1),
            (Side::Buy, bid - 3),
            (Side::Sell, ask + 1),
        ] {
            exchange.execute(Request::Limit {
                contract: id,
                side,
                tick,
                quantity: 1,
            });
        }
        // The wrong side at the price cancels nothing.
        exchange.execute(Request::Cancel {
            contract: id,
            side: Side::Sell,
            tick: bid - 1,
        });
        assert_eq!(exchange.orders.len(), 3);
        exchange.execute(Request::Cancel {
            contract: id,
            side: Side::Buy,
            tick: bid - 1,
        });
        assert_eq!(exchange.orders.len(), 2);
        exchange.execute(Request::CancelAll);
        assert!(exchange.orders.is_empty());
        assert_eq!(exchange.pending.gone, [1, 2, 3]);
    }

    #[test]
    fn test_ladder_rows_mark_the_inside_and_the_orders() {
        let mut exchange = Exchange::new(Some(7), 0.);
        let id = es();
        exchange.current = Some(id);
        let bid = exchange.book(id).bid;
        exchange.execute(Request::Limit {
            contract: id,
            side: Side::Buy,
            tick: bid - 2,
            quantity: 3,
        });
        exchange.band = Some(Band::around(id, exchange.book(id).last));
        let mut changed = Vec::new();
        exchange.update_rows(&mut changed);
        let row = |tick: i64| {
            let band = exchange.band.as_ref().expect("a band");
            band.rows[(tick - band.low()) as usize].clone()
        };
        let book = exchange.book(id);
        assert!(row(book.bid).best_bid && row(book.ask()).best_ask);
        assert!(row(book.last).last);
        let rows = &exchange.band.as_ref().expect("a band").rows;
        let marked = |mark: fn(&LadderRow) -> bool| rows.iter().filter(|row| mark(row)).count();
        assert_eq!(marked(|row| row.best_bid) + marked(|row| row.last), 2);
        // The point of control, if in the band, is the profile's top.
        assert!(
            rows.iter()
                .filter(|row| row.poc)
                .all(|row| row.profile == 1.)
        );
        assert_eq!(row(bid - 2).buys, 3);
        assert_eq!(row(bid - 2).bid, book.bids[2]);
        assert_eq!(row(book.ask() + 9).ask, book.asks[9]);
        assert_eq!(row(book.ask() + 10).ask, 0);
        // Nothing changed since: nothing to write.
        changed.clear();
        exchange.update_rows(&mut changed);
        assert!(changed.is_empty());
    }
}
