//! The market simulator: random-walk quotes for the 150 instruments of
//! `instruments.rs`, trades and a 20-level order book for the current
//! instrument (trades of every instrument with `TAPE ALL`), positions in
//! three accounts, price bars and neutral headlines. Free of GPUI:
//! it runs on its own thread (`run`), or one batch at a time on an
//! executor's timer where there are no threads (`Pump`, the browser), and
//! writes into the grid stores' writers an example gives it (`Feeds`);
//! what an example leaves out is not made.
//!
//! Everything it makes is simulated. The headlines describe the simulation
//! only (a price against its simulated VWAP, volume against the sector
//! mean), with numbers from the same bars a chart shows; they never report
//! a corporate event of a real company.

use std::{
    collections::VecDeque,
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

// `smol::channel` is `async_channel`; `smol` itself does not build for the
// browser.
use async_channel::Sender;
use gpui_kit_grid::GridWriter;
use parking_lot::Mutex;
// `std::time` on native targets; the browser clock in wasm, where
// `std::time::{Instant, SystemTime}::now` panic.
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use super::{
    instruments::{INSTRUMENTS, InstrumentId, Sector},
    rows::{self, Bar, BookLevel, Headline, Position, Print, Quote, Side},
};

/// Price updates per second unless an example says otherwise
/// (`DESK_RATE`): about two a second per instrument, a calm screen to
/// read. 5000 and up stress the grid.
pub const DEFAULT_RATE: u64 = 300;
/// The highest rate `RATE` accepts.
pub const MAX_RATE: u64 = 200_000;
/// Levels on each side of the book.
pub const BOOK_LEVELS: usize = 20;
/// Trades the tape keeps.
pub const TAPE_LEN: u64 = 300;
/// Headlines the news keeps.
pub const NEWS_KEPT: u64 = 500;
/// Seconds per price bar; bars start on whole multiples of it (UTC).
pub const BAR_SECONDS: u64 = 5;
/// Bars kept per instrument: two hours, 24 bars of five minutes.
pub const BAR_HISTORY: usize = 1_440;
/// Trades a bar stands for, on average. A bar of the history is made of
/// this many; a live update counts as a share of its trade
/// (`volume_weight`), so live bars add up to as many at any rate.
const TRADES_PER_BAR: f64 = 12.;
/// Positions held, spread over `ACCOUNTS`.
const POSITIONS: usize = 24;
/// The simulated accounts that hold the positions.
pub const ACCOUNTS: [&str; 3] = ["CORE", "HEDGE", "TACTICAL"];

/// The price step.
const TICK: f64 = 0.01;
/// Standard deviation of one update's relative move.
const VOLATILITY: f64 = 0.0003;
/// How strongly a price is pulled back towards its seed per update.
const REVERSION: f64 = 0.0004;
/// The quoted spread, in basis points of the price.
const SPREAD_BPS: f64 = 2.;
/// Updates of the current instrument per second on top of the rate, so
/// its tape and book stay lively at any rate.
const CURRENT_BOOST: f64 = 6.;
/// Headlines per second, on average.
const NEWS_RATE: f64 = 0.5;
/// One book level in this many gets a new size on a book update (every
/// 50 ms); one in `SIZE_DRIFT` drifts a little. Most levels keep theirs, so
/// a size's flash says something.
const SIZE_RENEW: u8 = 80;
const SIZE_DRIFT: u8 = 24;

/// Which trades the simulator writes to the tape (`TAPE CUR|ALL`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TapeScope {
    /// The current instrument's: the tape starts afresh with each one.
    #[default]
    Current,
    /// Every instrument's.
    All,
}

/// What the window and the simulator share: the update rate, the current
/// instrument, the tape's scope and a count of updates written.
pub struct Control {
    rate: AtomicU64,
    current: AtomicU32,
    tape_all: AtomicBool,
    updates: AtomicU64,
}

impl Control {
    const NONE: u32 = u32::MAX;

    pub fn new(rate: u64) -> Arc<Self> {
        Arc::new(Self {
            rate: AtomicU64::new(rate.min(MAX_RATE)),
            current: AtomicU32::new(Self::NONE),
            tape_all: AtomicBool::new(false),
            updates: AtomicU64::new(0),
        })
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Sets the rate, at most `MAX_RATE`; returns the rate set.
    pub fn set_rate(&self, rate: u64) -> u64 {
        let rate = rate.min(MAX_RATE);
        self.rate.store(rate, Ordering::Relaxed);
        rate
    }

    pub fn current(&self) -> Option<InstrumentId> {
        match self.current.load(Ordering::Relaxed) {
            Self::NONE => None,
            id => Some(InstrumentId(id as u16)),
        }
    }

    pub fn set_current(&self, id: Option<InstrumentId>) {
        let raw = id.map_or(Self::NONE, |id| u32::from(id.0));
        self.current.store(raw, Ordering::Relaxed);
    }

    pub fn tape_scope(&self) -> TapeScope {
        if self.tape_all.load(Ordering::Relaxed) {
            TapeScope::All
        } else {
            TapeScope::Current
        }
    }

    pub fn set_tape_scope(&self, scope: TapeScope) {
        self.tape_all
            .store(scope == TapeScope::All, Ordering::Relaxed);
    }

    /// Price updates written since the start.
    pub fn updates(&self) -> u64 {
        self.updates.load(Ordering::Relaxed)
    }
}

/// The price bars of the current instrument, as the simulator last
/// published them (every 100 ms). The chart reads them; nothing else does.
#[derive(Clone, Default)]
pub struct ChartFeed(Arc<Mutex<ChartSnapshot>>);

impl ChartFeed {
    pub fn snapshot(&self) -> ChartSnapshot {
        self.0.lock().clone()
    }

    fn publish(&self, instrument: Option<InstrumentId>, bars: Arc<[Bar]>, start: u64) {
        let mut snapshot = self.0.lock();
        snapshot.instrument = instrument;
        snapshot.bars = bars;
        snapshot.start = start;
        snapshot.version += 1;
    }
}

/// The bars of one instrument, oldest first, `BAR_SECONDS` apart; the last
/// bar is still open. `start` is the first bar's start in Unix seconds.
/// `version` grows with every publication.
#[derive(Clone)]
pub struct ChartSnapshot {
    pub instrument: Option<InstrumentId>,
    pub bars: Arc<[Bar]>,
    pub start: u64,
    pub version: u64,
}

impl Default for ChartSnapshot {
    fn default() -> Self {
        Self {
            instrument: None,
            bars: Arc::from(Vec::new()),
            start: 0,
            version: 0,
        }
    }
}

/// Where the simulator writes: the grid stores' writers and the chart feed
/// an example gives it. A feed left out is not made: no order book without
/// `with_book`, no trades without `with_tape`, no headlines without
/// `with_news`. The quotes move either way: the other feeds follow them.
#[derive(Default)]
pub struct Feeds {
    quotes: Option<GridWriter<Quote>>,
    tape: Option<GridWriter<Print>>,
    book: Option<GridWriter<BookLevel>>,
    positions: Option<GridWriter<Position>>,
    news: Option<GridWriter<Headline>>,
    chart: Option<ChartFeed>,
}

impl Feeds {
    /// No feeds yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every instrument's quote, each update.
    pub fn with_quotes(mut self, writer: GridWriter<Quote>) -> Self {
        self.quotes = Some(writer);
        self
    }

    /// The trades of the tape's scope (`Control::set_tape_scope`), the
    /// latest `TAPE_LEN`.
    pub fn with_tape(mut self, writer: GridWriter<Print>) -> Self {
        self.tape = Some(writer);
        self
    }

    /// The current instrument's order book, every 50 ms.
    pub fn with_book(mut self, writer: GridWriter<BookLevel>) -> Self {
        self.book = Some(writer);
        self
    }

    /// The positions at the current prices, every 250 ms.
    pub fn with_positions(mut self, writer: GridWriter<Position>) -> Self {
        self.positions = Some(writer);
        self
    }

    /// The headlines, the latest `NEWS_KEPT`: a backlog of half an hour at
    /// once, then one every two seconds on average.
    pub fn with_news(mut self, writer: GridWriter<Headline>) -> Self {
        self.news = Some(writer);
        self
    }

    /// The current instrument's bars, every 100 ms.
    pub fn with_chart(mut self, chart: ChartFeed) -> Self {
        self.chart = Some(chart);
        self
    }

    /// True once every store written is gone: the simulator stops.
    fn is_closed(&self) -> bool {
        [
            self.quotes.as_ref().map(GridWriter::is_closed),
            self.tape.as_ref().map(GridWriter::is_closed),
            self.book.as_ref().map(GridWriter::is_closed),
            self.positions.as_ref().map(GridWriter::is_closed),
            self.news.as_ref().map(GridWriter::is_closed),
        ]
        .into_iter()
        .flatten()
        .all(|closed| closed)
    }
}

/// The simulated market: quotes, bars, the book sizes of the current
/// instrument and the positions.
pub struct Market {
    rng: fastrand::Rng,
    quotes: Vec<Quote>,
    /// Each instrument's volume, fractional (`volume_weight`); the quote
    /// shows it rounded.
    volume: Vec<f64>,
    bars: Vec<VecDeque<Bar>>,
    /// When the open (newest) bar started, in Unix seconds.
    bar_time: u64,
    /// Sizes of the current instrument's book: asks, then bids, best first.
    book_sizes: [i64; 2 * BOOK_LEVELS],
    positions: Vec<Position>,
    next_print: u64,
    next_headline: u64,
}

impl Market {
    /// A market opened at the seed prices, with `BAR_HISTORY` bars of
    /// simulated history before now. The same `seed` makes the same market.
    pub fn new(seed: Option<u64>) -> Self {
        let mut rng = seed.map_or_else(fastrand::Rng::new, fastrand::Rng::with_seed);
        let mut quotes = Vec::with_capacity(INSTRUMENTS.len());
        let mut bars = Vec::with_capacity(INSTRUMENTS.len());
        for id in InstrumentId::all() {
            let instrument = id.get();
            // The history ends at the opening price, near the previous close.
            let mut price = instrument.seed * (1. + (rng.f64() - 0.5) * 0.04);
            let mut history = VecDeque::with_capacity(BAR_HISTORY + 1);
            for _ in 0..BAR_HISTORY {
                let mut bar = Bar::new(price);
                for _ in 0..TRADES_PER_BAR as usize {
                    price = walk(&mut rng, price, instrument.seed, 3.);
                    bar.trade(price, lot(&mut rng, price) as f64);
                }
                history.push_back(bar);
            }
            let (bid, ask) = quote_sides(price);
            quotes.push(Quote {
                id,
                ticker: instrument.ticker,
                name: instrument.name,
                sector: instrument.sector,
                last: price,
                close: instrument.seed,
                bid,
                ask,
                volume: rng.i64(200_000..8_000_000) * 100 / (100 + instrument.seed as i64),
            });
            bars.push(history);
        }
        let positions = (0..POSITIONS)
            .map(|ix| {
                // Spread over the table, a few short.
                let id = InstrumentId(((ix * 37 + 11) % INSTRUMENTS.len()) as u16);
                let quote = &quotes[id.0 as usize];
                let lots = rng.i64(1..40) * if quote.last < 100. { 100 } else { 10 };
                Position {
                    id,
                    ticker: quote.ticker,
                    account: ACCOUNTS[ix % ACCOUNTS.len()],
                    sector: quote.sector,
                    quantity: if ix % 4 == 3 { -lots } else { lots },
                    average: round_tick(quote.close * (1. + (rng.f64() - 0.5) * 0.08)),
                    last: quote.last,
                }
            })
            .collect();
        Self {
            volume: quotes.iter().map(|quote| quote.volume as f64).collect(),
            rng,
            quotes,
            bars,
            bar_time: bar_start(SystemTime::now()),
            book_sizes: [0; 2 * BOOK_LEVELS],
            positions,
            next_print: 0,
            next_headline: 0,
        }
    }

    pub fn quotes(&self) -> &[Quote] {
        &self.quotes
    }

    pub fn quote(&self, id: InstrumentId) -> &Quote {
        &self.quotes[id.0 as usize]
    }

    /// One price update of `id`: a new last, bid and ask, and the size and
    /// side of the trade that printed it. The volume grows by the size
    /// times `weight` (`volume_weight`).
    pub fn update(&mut self, id: InstrumentId, weight: f64) -> (i64, Side) {
        let ix = id.0 as usize;
        let seed = INSTRUMENTS[ix].seed;
        let rng = &mut self.rng;
        let quote = &mut self.quotes[ix];
        let before = quote.last;
        let last = walk(rng, before, seed, 1.);
        let size = lot(rng, last);
        let (bid, ask) = quote_sides(last);
        let volume = size as f64 * weight;
        self.volume[ix] += volume;
        quote.last = last;
        quote.bid = bid;
        quote.ask = ask;
        quote.volume = self.volume[ix].round() as i64;
        if let Some(bar) = self.bars[ix].back_mut() {
            bar.trade(last, volume);
        }
        let side = if last > before || (last == before && rng.bool()) {
            Side::Buy
        } else {
            Side::Sell
        };
        (size, side)
    }

    /// The tape's line for the last update of `id`.
    pub fn print(&mut self, id: InstrumentId, (size, side): (i64, Side), time: Arc<str>) -> Print {
        let seq = self.next_print;
        self.next_print += 1;
        Print {
            seq,
            time,
            ticker: id.get().ticker,
            price: self.quote(id).last,
            size,
            side,
        }
    }

    /// Opens the bars up to the one that starts at `time` (Unix seconds,
    /// a multiple of `BAR_SECONDS`) for every instrument at its last price;
    /// a stall opens the bars it missed too, so bars stay `BAR_SECONDS`
    /// apart.
    pub fn open_bars(&mut self, time: u64) {
        let missing = (time.saturating_sub(self.bar_time) / BAR_SECONDS).min(BAR_HISTORY as u64);
        if missing == 0 {
            return;
        }
        self.bar_time = time;
        for (bars, quote) in self.bars.iter_mut().zip(&self.quotes) {
            for _ in 0..missing {
                bars.push_back(Bar::new(quote.last));
            }
            while bars.len() > BAR_HISTORY {
                bars.pop_front();
            }
        }
    }

    pub fn bars(&self, id: InstrumentId) -> Arc<[Bar]> {
        self.bars[id.0 as usize].iter().copied().collect()
    }

    /// When the first of the `BAR_HISTORY` bars started, in Unix seconds.
    pub fn bars_start(&self) -> u64 {
        self.bar_time - (BAR_HISTORY as u64 - 1) * BAR_SECONDS
    }

    /// The 20 asks and 20 bids of `id` around its quote, best first, each
    /// with its depth against the largest level, then the spread line. A
    /// few sizes change on every call.
    pub fn book(&mut self, id: InstrumentId) -> Vec<BookLevel> {
        let quote = &self.quotes[id.0 as usize];
        let step = if quote.last < 100. {
            TICK
        } else if quote.last < 1_000. {
            5. * TICK
        } else {
            25. * TICK
        };
        let unit = if quote.last < 100. { 100 } else { 10 };
        let (bid, ask) = (quote.bid, quote.ask);
        let mut levels = Vec::with_capacity(2 * BOOK_LEVELS + 1);
        for (side_ix, best) in [(0, ask), (1, bid)] {
            let mut total = 0;
            for depth in 0..BOOK_LEVELS {
                let slot = &mut self.book_sizes[side_ix * BOOK_LEVELS + depth];
                // Deeper levels rest larger on average.
                let mean = unit * (8 + depth as i64 * 3);
                *slot = if *slot == 0 || self.rng.u8(..SIZE_RENEW) == 0 {
                    self.rng.i64(1..=2 * mean)
                } else if self.rng.u8(..SIZE_DRIFT) == 0 {
                    (*slot + self.rng.i64(-mean / 4..=mean / 4)).clamp(unit, 4 * mean)
                } else {
                    *slot
                };
                let size = *slot;
                total += size;
                let offset = step * depth as f64;
                let (level, price) = if side_ix == 0 {
                    (-(depth as i8 + 1), round_tick(best + offset))
                } else {
                    (depth as i8 + 1, round_tick(best - offset))
                };
                levels.push(BookLevel {
                    level,
                    price,
                    size,
                    total,
                    orders: 1 + size / (unit * 3),
                    depth: 0.,
                    spread: 0.,
                });
            }
        }
        let largest = levels
            .iter()
            .map(|level| level.size)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        for level in &mut levels {
            level.depth = level.size as f32 / largest;
        }
        levels.push(BookLevel::spread_line(
            levels[0].price,
            levels[BOOK_LEVELS].price,
        ));
        levels
    }

    /// The positions at the current prices.
    pub fn positions(&mut self) -> &[Position] {
        for position in &mut self.positions {
            position.last = self.quotes[position.id.0 as usize].last;
        }
        &self.positions
    }

    /// A neutral line about the simulated market: about `about` if given,
    /// else about a random instrument or the whole market.
    pub fn headline(&mut self, about: Option<InstrumentId>, time: Arc<str>) -> Headline {
        let seq = self.next_headline;
        self.next_headline += 1;
        // One line in ten is about the whole market.
        let instrument = match about {
            Some(id) => Some(id),
            None if self.rng.u8(..10) == 0 => None,
            None => Some(self.random_instrument()),
        };
        let mut text = String::with_capacity(80);
        match instrument {
            Some(id) => self.instrument_line(id, &mut text),
            None => self.market_line(&mut text),
        }
        Headline {
            seq,
            time,
            instrument,
            text: text.into(),
        }
    }

    fn instrument_line(&mut self, id: InstrumentId, text: &mut String) {
        let ix = id.0 as usize;
        let quote = &self.quotes[ix];
        let ticker = quote.ticker;
        let _ = match self.rng.u8(..5) {
            // From the bars a chart shows: its VWAP line, its highest high
            // and lowest low.
            0 => {
                let vwap = rows::vwap(&self.bars[ix]).unwrap_or(quote.last);
                let place = if quote.last >= vwap { "above" } else { "below" };
                write!(
                    text,
                    "{ticker} trades {place} its simulated VWAP ({vwap:.2})"
                )
            }
            1 => {
                let peers: Vec<i64> = self
                    .quotes
                    .iter()
                    .filter(|peer| peer.sector == quote.sector)
                    .map(|peer| peer.volume)
                    .collect();
                let mean = peers.iter().sum::<i64>() as f64 / peers.len().max(1) as f64;
                let ratio = quote.volume as f64 / mean.max(1.);
                write!(
                    text,
                    "Simulated volume in {ticker} at {ratio:.1}× the {} mean",
                    quote.sector.label()
                )
            }
            2 => {
                let (low, high) = range(&self.bars[ix]).unwrap_or((quote.last, quote.last));
                let hours = BAR_HISTORY as u64 * BAR_SECONDS / 3_600;
                if quote.last - low < high - quote.last {
                    write!(
                        text,
                        "{ticker} near its simulated {hours}-hour low ({low:.2}), last {:.2}",
                        quote.last
                    )
                } else {
                    write!(
                        text,
                        "{ticker} near its simulated {hours}-hour high ({high:.2}), last {:.2}",
                        quote.last
                    )
                }
            }
            3 => {
                let change = quote.change_pct();
                let direction = if change >= 0. { "up" } else { "down" };
                write!(
                    text,
                    "{ticker} {direction} {:.2}% on the simulated session",
                    change.abs()
                )
            }
            _ => {
                let bps = (quote.ask - quote.bid) / quote.last * 1e4;
                write!(text, "Simulated spread in {ticker} at {bps:.1} bps")
            }
        };
    }

    fn market_line(&mut self, text: &mut String) {
        let _ = if self.rng.bool() {
            let up = self
                .quotes
                .iter()
                .filter(|quote| quote.last >= quote.close)
                .count();
            let down = self.quotes.len() - up;
            write!(text, "Simulated breadth: {up} advancing, {down} declining")
        } else {
            let mut sectors: Vec<(Sector, f64)> = Sector::ALL
                .iter()
                .map(|&sector| {
                    let (sum, count) = self
                        .quotes
                        .iter()
                        .filter(|quote| quote.sector == sector)
                        .fold((0., 0), |(sum, count), quote| {
                            (sum + quote.change_pct(), count + 1)
                        });
                    (sector, sum / f64::from(count.max(1)))
                })
                .collect();
            sectors.sort_by(|a, b| b.1.total_cmp(&a.1));
            let (best, worst) = (sectors[0], sectors[sectors.len() - 1]);
            write!(
                text,
                "{} lead the simulated session ({:+.2}%), {} lag ({:+.2}%)",
                best.0.label(),
                best.1,
                worst.0.label(),
                worst.1
            )
        };
    }

    fn random_instrument(&mut self) -> InstrumentId {
        InstrumentId(self.rng.u16(..INSTRUMENTS.len() as u16))
    }
}

// ---- Hook for `crates/grid-gateway`: quotes without a `GridWriter`. ----
impl Market {
    /// One batch of `count` price updates at random instruments, as
    /// `Pump::step` makes them at `rate` updates a second in total, without
    /// writing anywhere: pushes each updated instrument to `changed`
    /// (repeats included) and opens the bars the clock has reached.
    pub fn step_quotes(&mut self, count: u64, rate: f64, changed: &mut Vec<InstrumentId>) {
        let weight = volume_weight(rate / INSTRUMENTS.len() as f64);
        for _ in 0..count {
            let id = self.random_instrument();
            self.update(id, weight);
            changed.push(id);
        }
        self.open_bars(bar_start(SystemTime::now()));
    }
}
// ---- End of the hook for `crates/grid-gateway`. ----

/// The lowest low and the highest high of `bars`.
fn range<'a>(bars: impl IntoIterator<Item = &'a Bar>) -> Option<(f64, f64)> {
    bars.into_iter().fold(None, |range, bar| {
        let (low, high) = range.unwrap_or((bar.low, bar.high));
        Some((low.min(bar.low), high.max(bar.high)))
    })
}

/// The share of its trade size one update adds to the volume, for an
/// instrument updated `per_second` times a second: a live bar then adds up
/// to `TRADES_PER_BAR` trades on average whatever the rate, as a bar of
/// the history does. Never more than the whole size: a rate too low to
/// fill a bar leaves it short instead of making one trade a spike.
pub fn volume_weight(per_second: f64) -> f64 {
    TRADES_PER_BAR / (per_second * BAR_SECONDS as f64).max(TRADES_PER_BAR)
}

/// One step of a mean-reverting random walk, in whole ticks. `scale`
/// widens the step (history bars stand for several updates).
fn walk(rng: &mut fastrand::Rng, price: f64, seed: f64, scale: f64) -> f64 {
    // Uniform with the standard deviation of `VOLATILITY * scale`.
    let shock = (rng.f64() - 0.5) * 2. * 3f64.sqrt() * VOLATILITY * scale;
    let pull = -REVERSION * scale * (price / seed).ln();
    round_tick((price * (1. + shock + pull)).max(TICK))
}

/// Bid and ask around `last`, `SPREAD_BPS` apart, at least a tick.
fn quote_sides(last: f64) -> (f64, f64) {
    let spread = (last * SPREAD_BPS / 1e4 / TICK).round().max(1.) * TICK;
    let bid = round_tick(((last - spread / 2.) / TICK).floor() * TICK);
    (bid, round_tick(bid + spread))
}

fn round_tick(price: f64) -> f64 {
    (price / TICK).round() * TICK
}

/// A trade size: round lots, fewer shares of dearer instruments.
fn lot(rng: &mut fastrand::Rng, price: f64) -> i64 {
    let unit = if price < 50. {
        500
    } else if price < 300. {
        100
    } else {
        20
    };
    rng.i64(1..=20) * unit
}

/// The start of the bar `time` falls in, in Unix seconds.
fn bar_start(time: SystemTime) -> u64 {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    seconds - seconds % BAR_SECONDS
}

/// `time` as UTC `hh:mm:ss`, or `hh:mm:ss.mmm` with `millis`.
pub fn utc_time(time: SystemTime, millis: bool) -> Arc<str> {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs() % 86_400;
    let (h, m, s) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    if millis {
        format!("{h:02}:{m:02}:{s:02}.{:03}", since.subsec_millis()).into()
    } else {
        format!("{h:02}:{m:02}:{s:02}").into()
    }
}

/// Writes `headline` to the news, dropping the one `NEWS_KEPT` older.
fn publish_headline(news: &GridWriter<Headline>, headline: Headline) {
    let seq = headline.seq;
    news.batch(|rows| {
        if let Some(old) = seq.checked_sub(NEWS_KEPT) {
            rows.remove(old);
        }
        rows.upsert(headline);
    });
}

/// Runs the market on the calling thread until every store in `feeds` is
/// gone: the first load, then 10 ms batches of `Control::rate` updates a
/// second in total. `loaded` is told once the first load is written.
pub fn run(market: Market, feeds: Feeds, control: Arc<Control>, loaded: Sender<()>) {
    let mut pump = Pump::start(market, feeds, control);
    let _ = loaded.try_send(());
    while !pump.is_closed() {
        let wait = pump.step();
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

/// The market between its 10 ms batches, for an executor without threads
/// (the browser): `start` writes the first load, each `step` one batch, and
/// the caller waits as long as `step` says, on its executor's timer.
pub struct Pump {
    market: Market,
    feeds: Feeds,
    control: Arc<Control>,
    started: Instant,
    batch: u64,
    due: f64,
    current: Option<InstrumentId>,
    tape_scope: TapeScope,
    prints: Vec<Print>,
}

impl Pump {
    /// Writes the first load and starts the batch clock.
    pub fn start(mut market: Market, feeds: Feeds, control: Arc<Control>) -> Self {
        Self::first_load(&mut market, &feeds);
        Self {
            started: Instant::now(),
            batch: 0,
            due: 0.,
            current: None,
            tape_scope: control.tape_scope(),
            prints: Vec::with_capacity(64),
            market,
            feeds,
            control,
        }
    }

    /// True once every store written is gone: stop stepping.
    pub fn is_closed(&self) -> bool {
        self.feeds.is_closed()
    }

    fn first_load(market: &mut Market, feeds: &Feeds) {
        if let Some(quotes) = &feeds.quotes {
            quotes.replace_all(market.quotes().iter().cloned().map(Arc::new));
        }
        if let Some(positions) = &feeds.positions {
            positions.replace_all(market.positions().iter().cloned().map(Arc::new));
        }
        if let Some(news) = &feeds.news {
            // A backlog of headlines over the last half hour, without a flash.
            let now = SystemTime::now();
            let backlog: Vec<Arc<Headline>> = (1..=30)
                .rev()
                .map(|minutes| {
                    let time = utc_time(now - Duration::from_secs(minutes * 60), false);
                    Arc::new(market.headline(None, time))
                })
                .collect();
            news.replace_all(backlog);
        }
    }

    /// Writes one 10 ms batch; returns how long to wait before the next.
    pub fn step(&mut self) -> Duration {
        let (started, mut batch, mut due, mut current, mut tape_scope) = (
            self.started,
            self.batch,
            self.due,
            self.current,
            self.tape_scope,
        );
        let Self {
            market,
            feeds,
            control,
            prints,
            ..
        } = self;
        let rate = control.rate() as f64;
        let selected = control.current();
        let scope = control.tape_scope();
        // The tape of the current instrument starts afresh with each one;
        // the whole market's goes on.
        if let Some(tape) = &feeds.tape
            && (scope != tape_scope || (selected != current && scope == TapeScope::Current))
        {
            tape.clear();
        }
        tape_scope = scope;
        if selected != current {
            current = selected;
            market.book_sizes = [0; 2 * BOOK_LEVELS];
            match current {
                Some(id) => {
                    if let Some(book) = &feeds.book {
                        book.replace_all(market.book(id).into_iter().map(Arc::new));
                    }
                    if let Some(chart) = &feeds.chart {
                        chart.publish(current, market.bars(id), market.bars_start());
                    }
                    // Something about it to read at once.
                    if let Some(news) = &feeds.news {
                        let time = utc_time(SystemTime::now(), false);
                        publish_headline(news, market.headline(Some(id), time));
                    }
                }
                None => {
                    if let Some(book) = &feeds.book {
                        book.clear();
                    }
                    if let Some(chart) = &feeds.chart {
                        chart.publish(None, Arc::from(Vec::new()), 0);
                    }
                }
            }
        }

        due += rate / 100.;
        let boost = current.filter(|_| market.rng.f64() < CURRENT_BOOST / 100.);
        let count = due as u64;
        due -= count as f64;
        let total = count + u64::from(boost.is_some());
        // An instrument's updates a second: its share of the rate, and the
        // boost for the current one.
        let per_instrument = rate / INSTRUMENTS.len() as f64;
        let (weight, current_weight) = (
            volume_weight(per_instrument),
            volume_weight(per_instrument + CURRENT_BOOST),
        );
        let now = SystemTime::now();
        let time = utc_time(now, true);
        let writes_tape = feeds.tape.is_some();
        let mut update = |market: &mut Market, n: u64| {
            let id = match boost {
                Some(id) if n == count => id,
                _ => market.random_instrument(),
            };
            let weight = if Some(id) == current {
                current_weight
            } else {
                weight
            };
            let trade = market.update(id, weight);
            if writes_tape && (tape_scope == TapeScope::All || Some(id) == current) {
                prints.push(market.print(id, trade, time.clone()));
            }
            id
        };
        match &feeds.quotes {
            Some(writer) => writer.batch(|quotes| {
                for n in 0..total {
                    let id = update(market, n);
                    quotes.upsert(market.quote(id).clone());
                }
            }),
            None => {
                for n in 0..total {
                    update(market, n);
                }
            }
        }
        control.updates.fetch_add(total, Ordering::Relaxed);
        if let Some(tape) = &feeds.tape
            && !prints.is_empty()
        {
            tape.batch(|tape| {
                for print in prints.drain(..) {
                    if let Some(old) = print.seq.checked_sub(TAPE_LEN) {
                        tape.remove(old);
                    }
                    tape.upsert(print);
                }
            });
        }

        batch += 1;
        if let Some(id) = current {
            if let Some(book) = &feeds.book
                && batch % 5 == 0
            {
                let levels = market.book(id);
                book.batch(|book| {
                    for level in levels {
                        book.upsert(level);
                    }
                });
            }
            if let Some(chart) = &feeds.chart
                && batch % 10 == 0
            {
                chart.publish(current, market.bars(id), market.bars_start());
            }
        }
        if let Some(writer) = &feeds.positions
            && batch % 25 == 0
        {
            let positions = market.positions().to_vec();
            writer.batch(|rows| {
                for position in positions {
                    rows.upsert(position);
                }
            });
        }
        if let Some(news) = &feeds.news
            && market.rng.f64() < NEWS_RATE / 100.
        {
            // Half the lines are about the current instrument.
            let about = current.filter(|_| market.rng.bool());
            let headline = market.headline(about, utc_time(now, false));
            publish_headline(news, headline);
        }
        market.open_bars(bar_start(now));

        let next = started + Duration::from_millis(batch * 10);
        let now = Instant::now();
        let mut wait = Duration::ZERO;
        if next > now {
            wait = next - now;
        } else if now - next > Duration::from_millis(200) {
            // Too far behind (a stalled machine): do not catch up in a burst.
            batch = (started.elapsed().as_millis() / 10) as u64;
        }
        (self.batch, self.due, self.current, self.tape_scope) = (batch, due, current, tape_scope);
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quotes_stay_ordered_and_positive() {
        let mut market = Market::new(Some(7));
        let id = InstrumentId(0);
        for _ in 0..500 {
            let (size, _) = market.update(id, 1.);
            let quote = market.quote(id);
            assert!(quote.last > 0. && size > 0);
            assert!(quote.bid < quote.ask, "{quote:?}");
        }
    }

    #[test]
    fn test_book_has_twenty_levels_a_side_best_first() {
        let mut market = Market::new(Some(7));
        let id = InstrumentId(3);
        let levels = market.book(id);
        assert_eq!(levels.len(), 2 * BOOK_LEVELS + 1);
        let (asks, rest) = levels.split_at(BOOK_LEVELS);
        let (bids, spread) = rest.split_at(BOOK_LEVELS);
        assert!(asks.windows(2).all(|pair| pair[0].price < pair[1].price));
        assert!(bids.windows(2).all(|pair| pair[0].price > pair[1].price));
        assert!(bids[0].price < asks[0].price);
        assert!(asks.windows(2).all(|pair| pair[0].total < pair[1].total));
        // The spread line sorts between the best bid and the best ask.
        let spread = &spread[0];
        assert!(spread.is_spread());
        assert!(bids[0].price < spread.price && spread.price < asks[0].price);
        assert!((spread.spread - (asks[0].price - bids[0].price)).abs() < 1e-9);
        // The largest level is fully shaded, the others less.
        let depths: Vec<f32> = levels[..2 * BOOK_LEVELS].iter().map(|l| l.depth).collect();
        assert!(depths.iter().all(|depth| (0. ..=1.).contains(depth)));
        assert!(depths.contains(&1.));
    }

    #[test]
    fn test_positions_spread_over_the_accounts() {
        let mut market = Market::new(Some(7));
        let positions = market.positions();
        for account in ACCOUNTS {
            let held = positions.iter().filter(|p| p.account == account).count();
            assert_eq!(held, POSITIONS / ACCOUNTS.len(), "{account}");
        }
        assert!(positions.iter().any(|position| position.quantity < 0));
    }

    #[test]
    fn test_headlines_name_the_instrument_and_say_simulated() {
        let mut market = Market::new(Some(7));
        let id = InstrumentId(2);
        for _ in 0..20 {
            let headline = market.headline(Some(id), Arc::from("00:00:00"));
            assert_eq!(headline.instrument, Some(id));
            assert!(headline.text.contains(id.get().ticker), "{}", headline.text);
            assert!(headline.text.contains("simulated") || headline.text.contains("Simulated"));
        }
    }

    #[test]
    fn test_headline_numbers_come_from_the_bars() {
        let mut market = Market::new(Some(7));
        let id = InstrumentId(2);
        for _ in 0..200 {
            market.update(id, 1.);
        }
        let bars = market.bars(id);
        let vwap = format!("({:.2})", rows::vwap(bars.iter()).expect("volume"));
        let (low, high) = range(bars.iter()).expect("bars");
        let (low, high) = (format!("({low:.2})"), format!("({high:.2})"));
        let mut seen = [false; 3];
        for _ in 0..60 {
            let headline = market.headline(Some(id), Arc::from("00:00:00"));
            let text = &*headline.text;
            if text.contains("VWAP") {
                assert!(text.contains(&vwap), "{text}, not {vwap}");
                seen[0] = true;
            } else if text.contains("-hour high") {
                assert!(text.contains(&high), "{text}, not {high}");
                seen[1] = true;
            } else if text.contains("-hour low") {
                assert!(text.contains(&low), "{text}, not {low}");
                seen[2] = true;
            }
        }
        assert!(seen[0] && (seen[1] || seen[2]), "{seen:?}");
        // Each headline has the next number.
        let next = market.headline(None, Arc::from("00:00:00")).seq;
        assert_eq!(market.headline(None, Arc::from("00:00:00")).seq, next + 1);
    }

    #[test]
    fn test_bar_volume_does_not_grow_with_the_rate() {
        // Above a trade a bar, the updates of a bar add up to the trades of
        // a history bar, whatever the rate.
        for per_second in [3., 30., 300., 3_000.] {
            let per_bar = per_second * BAR_SECONDS as f64;
            assert!((per_bar * volume_weight(per_second) - TRADES_PER_BAR).abs() < 1e-9);
        }
        // Below, an update counts its whole size, never more.
        assert_eq!(volume_weight(0.5), 1.);
        assert_eq!(volume_weight(0.), 1.);

        let mut market = Market::new(Some(7));
        let id = InstrumentId(4);
        market.open_bars(market.bar_time + BAR_SECONDS);
        let volume = market.quote(id).volume;
        let (size, _) = market.update(id, 0.25);
        let bar = market.bars(id)[BAR_HISTORY - 1];
        assert!((bar.volume - size as f64 * 0.25).abs() < 1e-9);
        assert_eq!(
            market.quote(id).volume,
            (volume as f64 + size as f64 * 0.25).round() as i64
        );
    }
}
