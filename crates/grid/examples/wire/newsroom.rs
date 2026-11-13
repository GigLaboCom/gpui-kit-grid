//! The simulated newsroom: neutral stories about the simulated market, made
//! from the quotes the simulator writes, on a thread of its own that owns
//! the news rows. Free of GPUI: only the grid's row traits and its writer.
//!
//! **Every story is simulated market colour.** The lines come from fixed
//! templates over the simulator's numbers: moves, ranges, volume, breadth,
//! sector rotation and gauges computed from the simulated quotes. They never
//! report a corporate event (no earnings, deals, guidance, people, courts or
//! regulators) of the real companies whose tickers the simulation borrows,
//! and each body ends with a line that says so.
//!
//! The window never writes the news store: it asks the newsroom to mark a
//! story read (`NewsCommand`), and the newsroom writes the new row, so the
//! rows have one owner and a story about to leave the store is never
//! written back.

use std::{
    collections::VecDeque,
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant, SystemTime},
};

use gpui_kit_grid::{CellValue, ColumnIx, GridColumns, GridRow, GridWriter, StoreSnapshot};
use smol::channel::Sender;

use crate::trading::{
    instruments::{InstrumentId, Sector},
    market::utc_time,
    rows::Quote,
};

/// Stories the news keeps; the oldest leaves the store with each new one.
pub const STORIES_KEPT: usize = 400;
/// The backlog written at start: this many stories over `BACKLOG_SPAN`.
const BACKLOG: u32 = 36;
const BACKLOG_SPAN: Duration = Duration::from_secs(45 * 60);
/// Backlog stories older than this start read, as if read earlier in the
/// session; the newer ones start unread.
const BACKLOG_READ_AFTER: Duration = Duration::from_secs(12 * 60);
/// Seconds between two live stories: uniform between these.
const INTERVAL: (f64, f64) = (1.5, 5.5);
/// How often the newsroom reads the quotes (session ranges) when no
/// command wakes it.
const POLL: Duration = Duration::from_millis(100);
/// Headlines a new story should not repeat: the last this many.
const RECENT: usize = 40;
/// Drafts tried for a headline not among the `RECENT`, before one is
/// repeated anyway.
const FRESH_TRIES: u32 = 8;
/// Ends every story's body.
pub const DISCLAIMER: &str = "Simulated market colour: every figure comes from the example's \
    price simulator. It is not market data and describes no real trading, company or event.";

/// What a story is about. Every category is simulated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// The whole simulated market: breadth, the index funds, wide moves.
    Markets,
    /// Sector averages, rankings and rotation.
    Sectors,
    /// One or two names: moves, ranges, spreads, peers.
    Equities,
    /// Share volume and turnover.
    Volume,
    /// Gauges computed from the simulated quotes.
    MacroSim,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Markets,
        Self::Sectors,
        Self::Equities,
        Self::Volume,
        Self::MacroSim,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Markets => "Markets",
            Self::Sectors => "Sectors",
            Self::Equities => "Equities",
            Self::Volume => "Volume",
            Self::MacroSim => "Macro-sim",
        }
    }

    /// The place in `ALL`.
    pub fn ix(self) -> usize {
        self as usize
    }

    /// A random category: single names most often.
    fn pick(rng: &mut fastrand::Rng) -> Self {
        match rng.u8(..20) {
            0..=3 => Self::Markets,
            4..=7 => Self::Sectors,
            8..=14 => Self::Equities,
            15..=17 => Self::Volume,
            _ => Self::MacroSim,
        }
    }
}

/// The columns of the news monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StoryColumn {
    /// A dot while the story is unread.
    Unread,
    Time,
    Category,
    Tickers,
    Headline,
    /// The order of publication: newest first sorts by it.
    Seq,
}

impl GridColumns for StoryColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Unread,
            Self::Time,
            Self::Category,
            Self::Tickers,
            Self::Headline,
            Self::Seq,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Time => "time",
            Self::Category => "category",
            Self::Tickers => "tickers",
            Self::Headline => "headline",
            Self::Seq => "seq",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// The mark of an unread story.
pub const UNREAD_MARK: &str = "•";

/// A simulated story: a neutral headline and body about the simulated
/// market, the instruments it mentions and whether it was read. The key is
/// the publication number. Every text a cell shows is made when the story
/// is written.
#[derive(Clone, Debug)]
pub struct Story {
    pub seq: u64,
    /// UTC, `hh:mm:ss`.
    pub time: Arc<str>,
    pub category: Category,
    /// The instruments the story mentions, in the order it names them.
    pub tickers: Arc<[InstrumentId]>,
    /// The tickers as the Tickers column shows them.
    pub tickers_text: Arc<str>,
    pub headline: Arc<str>,
    /// Paragraphs separated by blank lines; the last is `DISCLAIMER`.
    pub body: Arc<str>,
    pub read: bool,
}

impl Story {
    /// The body's paragraphs.
    pub fn paragraphs(&self) -> impl Iterator<Item = &str> {
        self.body.split("\n\n")
    }
}

impl GridRow for Story {
    type Key = u64;
    type Column = StoryColumn;

    fn key(&self) -> &u64 {
        &self.seq
    }

    fn value(&self, column: StoryColumn) -> CellValue<'_> {
        match column {
            StoryColumn::Unread if self.read => CellValue::Empty,
            StoryColumn::Unread => CellValue::Text(UNREAD_MARK),
            StoryColumn::Time => CellValue::Text(&self.time),
            StoryColumn::Category => CellValue::Text(self.category.label()),
            StoryColumn::Tickers => CellValue::Text(&self.tickers_text),
            StoryColumn::Headline => CellValue::Text(&self.headline),
            StoryColumn::Seq => CellValue::Int(self.seq as i64),
        }
    }
}

/// What the window asks of the newsroom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewsCommand {
    /// The story was opened.
    Read(u64),
    /// Every story kept is read.
    ReadAll,
}

/// What the window shows of the news without reading the store: the
/// stories kept and the unread ones per category. Written by the newsroom
/// after each change.
#[derive(Debug, Default)]
pub struct NewsCounts {
    stories: AtomicU32,
    unread: [AtomicU32; Category::ALL.len()],
}

impl NewsCounts {
    pub fn stories(&self) -> u32 {
        self.stories.load(Ordering::Relaxed)
    }

    pub fn unread(&self, category: Category) -> u32 {
        self.unread[category.ix()].load(Ordering::Relaxed)
    }

    pub fn total_unread(&self) -> u32 {
        Category::ALL
            .iter()
            .map(|&category| self.unread(category))
            .sum()
    }
}

/// The quotes of every instrument, by `InstrumentId`.
pub type Board = [Arc<Quote>];

/// Every instrument's quote from `snapshot`, or `None` before the first
/// load.
pub fn board(snapshot: &StoreSnapshot<Quote>) -> Option<Vec<Arc<Quote>>> {
    InstrumentId::all()
        .map(|id| snapshot.get(&id).cloned())
        .collect()
}

/// A story before it has a number and a time.
struct Draft {
    category: Category,
    tickers: Vec<InstrumentId>,
    headline: String,
    body: String,
}

impl Draft {
    fn new(category: Category) -> Self {
        Self {
            category,
            tickers: Vec::with_capacity(4),
            headline: String::with_capacity(80),
            body: String::with_capacity(400),
        }
    }

    /// Mentions `id`, once.
    fn mention(&mut self, id: InstrumentId) {
        if !self.tickers.contains(&id) {
            self.tickers.push(id);
        }
    }

    /// Starts the next paragraph of the body.
    fn paragraph(&mut self) {
        if !self.body.is_empty() {
            self.body.push_str("\n\n");
        }
    }
}

/// The simulated newsroom: writes stories from the quotes and keeps the
/// ones in the store, to mark them read.
pub struct Newsroom {
    rng: fastrand::Rng,
    next_seq: u64,
    /// Each instrument's simulated session low and high since the newsroom
    /// opened.
    ranges: Vec<(f64, f64)>,
    /// The sector ranking of the last sector story, best first.
    ranking: Vec<Sector>,
    /// The dispersion gauge of the last gauge story.
    dispersion: Option<f64>,
    /// The stories in the store, oldest first.
    stories: VecDeque<Arc<Story>>,
}

impl Newsroom {
    /// The same `seed` writes the same stories from the same quotes.
    pub fn new(seed: Option<u64>) -> Self {
        Self {
            rng: seed.map_or_else(fastrand::Rng::new, fastrand::Rng::with_seed),
            next_seq: 0,
            ranges: Vec::new(),
            ranking: Vec::new(),
            dispersion: None,
            stories: VecDeque::with_capacity(STORIES_KEPT + 1),
        }
    }

    /// Follows the quotes: the session lows and highs.
    pub fn observe(&mut self, board: &Board) {
        if self.ranges.len() != board.len() {
            self.ranges = board.iter().map(|quote| (quote.last, quote.last)).collect();
            return;
        }
        for (range, quote) in self.ranges.iter_mut().zip(board) {
            range.0 = range.0.min(quote.last);
            range.1 = range.1.max(quote.last);
        }
    }

    /// The backlog: `BACKLOG` stories over the `BACKLOG_SPAN` before `now`,
    /// oldest first; the older ones read.
    pub fn backlog(&mut self, board: &Board, now: SystemTime) -> Vec<Arc<Story>> {
        let step = BACKLOG_SPAN / BACKLOG;
        (1..=BACKLOG)
            .rev()
            .map(|n| {
                // A little jitter, so the times do not tick evenly.
                let ago = step * n - Duration::from_secs(self.rng.u64(..step.as_secs() / 2));
                let time = utc_time(now - ago, false);
                let story = self.write(board, time, ago > BACKLOG_READ_AFTER);
                self.keep(story.clone());
                story
            })
            .collect()
    }

    /// A new story about the simulated market, unread, kept; with the
    /// number of the story that leaves the store for it, if one does.
    pub fn publish(&mut self, board: &Board, time: Arc<str>) -> (Arc<Story>, Option<u64>) {
        let story = self.write(board, time, false);
        let evicted = self.keep(story.clone());
        (story, evicted)
    }

    /// Seconds to the next live story.
    pub fn interval(&mut self) -> Duration {
        let (low, high) = INTERVAL;
        Duration::from_secs_f64(low + self.rng.f64() * (high - low))
    }

    /// Applies `command`; returns the stories it changed.
    pub fn apply(&mut self, command: NewsCommand) -> Vec<Arc<Story>> {
        let mut changed = Vec::new();
        for story in &mut self.stories {
            let hit = match command {
                NewsCommand::Read(seq) => story.seq == seq,
                NewsCommand::ReadAll => true,
            };
            if hit && !story.read {
                let mut read = Story::clone(story);
                read.read = true;
                *story = Arc::new(read);
                changed.push(story.clone());
            }
        }
        changed
    }

    /// Writes the counts of the stories kept.
    pub fn count(&self, counts: &NewsCounts) {
        let mut unread = [0u32; Category::ALL.len()];
        for story in self.stories.iter().filter(|story| !story.read) {
            unread[story.category.ix()] += 1;
        }
        for (slot, count) in counts.unread.iter().zip(unread) {
            slot.store(count, Ordering::Relaxed);
        }
        counts
            .stories
            .store(self.stories.len() as u32, Ordering::Relaxed);
    }

    fn keep(&mut self, story: Arc<Story>) -> Option<u64> {
        self.stories.push_back(story);
        (self.stories.len() > STORIES_KEPT)
            .then(|| self.stories.pop_front().map(|story| story.seq))
            .flatten()
    }

    fn write(&mut self, board: &Board, time: Arc<str>, read: bool) -> Arc<Story> {
        if self.ranges.len() != board.len() {
            self.observe(board);
        }
        // A headline the monitor showed lately is drawn again: the
        // quotes move slowly, and the backlog is written from one board.
        let mut draft = self.draft(board);
        for _ in 0..FRESH_TRIES {
            if !self.is_recent(&draft.headline) {
                break;
            }
            draft = self.draft(board);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let tickers_text = draft
            .tickers
            .iter()
            .map(|id| id.get().ticker)
            .collect::<Vec<_>>()
            .join("  ");
        Arc::new(Story {
            seq,
            time,
            category: draft.category,
            tickers: draft.tickers.into(),
            tickers_text: tickers_text.into(),
            headline: draft.headline.into(),
            body: draft.body.into(),
            read,
        })
    }

    /// A story of a random category, with the disclaimer.
    fn draft(&mut self, board: &Board) -> Draft {
        let category = Category::pick(&mut self.rng);
        let mut draft = Draft::new(category);
        match category {
            Category::Markets => self.markets(board, &mut draft),
            Category::Sectors => self.sectors(board, &mut draft),
            Category::Equities => self.equities(board, &mut draft),
            Category::Volume => self.volume(board, &mut draft),
            Category::MacroSim => self.gauges(board, &mut draft),
        }
        draft.paragraph();
        draft.body.push_str(DISCLAIMER);
        draft
    }

    /// Returns true if one of the last `RECENT` stories has `headline`.
    fn is_recent(&self, headline: &str) -> bool {
        self.stories
            .iter()
            .rev()
            .take(RECENT)
            .any(|story| *story.headline == *headline)
    }

    fn random_stock(&mut self, board: &Board) -> InstrumentId {
        let stocks: Vec<InstrumentId> = board
            .iter()
            .filter(|quote| is_stock(quote))
            .map(|quote| quote.id)
            .collect();
        stocks[self.rng.usize(..stocks.len())]
    }

    fn random_sector(&mut self) -> Sector {
        STOCK_SECTORS[self.rng.usize(..STOCK_SECTORS.len())]
    }

    // Markets: breadth, the index funds, wide moves.
    fn markets(&mut self, board: &Board, draft: &mut Draft) {
        let stocks = || board.iter().filter(|quote| is_stock(quote));
        let _ = match self.rng.u8(..3) {
            0 => {
                let up = stocks().filter(|quote| quote.last > quote.close).count();
                let total = stocks().count();
                let average = mean(stocks().map(|quote| quote.change_pct()));
                let wide = stocks()
                    .filter(|quote| quote.change_pct().abs() > 1.)
                    .count();
                let (spy, iwm) = (etf(board, "SPY"), etf(board, "IWM"));
                draft.mention(spy.id);
                draft.mention(iwm.id);
                write!(
                    draft.headline,
                    "Simulated breadth: {up} advancing, {} declining",
                    total - up
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "Across the {total} simulated stocks, {up} trade above their simulated \
                         previous close and {} at or below it. The equal-weighted average move \
                         is {average:+.2}%, and {wide} names are more than 1% from the close.\n\n\
                         The simulated SPY stands at {:.2} ({:+.2}%) and IWM at {:.2} ({:+.2}%).",
                        total - up,
                        spy.last,
                        spy.change_pct(),
                        iwm.last,
                        iwm.change_pct()
                    )
                })
            }
            1 => {
                let funds: Vec<&Quote> = ["SPY", "QQQ", "IWM", "DIA"]
                    .into_iter()
                    .map(|ticker| etf(board, ticker))
                    .collect();
                for fund in &funds {
                    draft.mention(fund.id);
                }
                let _ = write!(
                    draft.headline,
                    "Simulated index funds: SPY {:+.2}%, QQQ {:+.2}%",
                    funds[0].change_pct(),
                    funds[1].change_pct()
                );
                let _ = write!(draft.body, "The four simulated index funds on the session:");
                for (ix, fund) in funds.iter().enumerate() {
                    let _ = write!(
                        draft.body,
                        "{} {} at {:.2}, {:+.2}%",
                        if ix == 0 { "" } else { ";" },
                        fund.ticker,
                        fund.last,
                        fund.change_pct()
                    );
                }
                write!(
                    draft.body,
                    ".\n\nThe funds are simulated like every other instrument here; their \
                     moves are not computed from the simulated stocks."
                )
            }
            _ => {
                let wide: Vec<&Arc<Quote>> = stocks()
                    .filter(|quote| quote.change_pct().abs() > 1.)
                    .collect();
                let up = wide.iter().filter(|quote| quote.last > quote.close).count();
                let total = stocks().count();
                let (best, worst) = extremes(stocks(), |quote| quote.change_pct());
                draft.mention(best.id);
                draft.mention(worst.id);
                write!(
                    draft.headline,
                    "{} simulated names move more than 1% from the previous close",
                    wide.len()
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "{} of the {total} simulated stocks are more than 1% from their \
                         simulated previous close: {up} higher and {} lower.\n\nThe widest \
                         moves are {} at {:+.2}% and {} at {:+.2}%.",
                        wide.len(),
                        wide.len() - up,
                        best.ticker,
                        best.change_pct(),
                        worst.ticker,
                        worst.change_pct()
                    )
                })
            }
        };
    }

    // Sectors: the ranking, rotation within it, breadth inside a sector.
    fn sectors(&mut self, board: &Board, draft: &mut Draft) {
        let ranking = sector_ranking(board);
        let previous = std::mem::replace(
            &mut self.ranking,
            ranking.iter().map(|(sector, _)| *sector).collect(),
        );
        let place = |ranking: &[Sector], sector: Sector| {
            ranking.iter().position(|ranked| *ranked == sector)
        };
        // The sector that moved furthest in the ranking since the last
        // sector story, if any did.
        let rotation = ranking
            .iter()
            .enumerate()
            .filter_map(|(now, (sector, average))| {
                let before = place(&previous, *sector)?;
                (before != now).then_some((*sector, *average, before, now))
            })
            .max_by_key(|(_, _, before, now)| before.abs_diff(*now));
        let _ = match (self.rng.u8(..3), rotation) {
            (0, Some((sector, average, before, now))) => {
                let (best, _) = sector_extremes(board, sector);
                draft.mention(best.id);
                let verb = if now < before { "climbs" } else { "slips" };
                write!(
                    draft.headline,
                    "{} {verb} to No. {} in the simulated sector ranking",
                    sector.label(),
                    now + 1
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "{} moves from No. {} to No. {} of {} in the simulated ranking by \
                         average move, now at {average:+.2}%. {} holds the top place at \
                         {:+.2}%.\n\nWithin {}, {} leads at {:+.2}%.",
                        sector.label(),
                        before + 1,
                        now + 1,
                        ranking.len(),
                        ranking[0].0.label(),
                        ranking[0].1,
                        sector.label(),
                        best.ticker,
                        best.change_pct()
                    )
                })
            }
            (1, _) => {
                let sector = self.random_sector();
                let members = || board.iter().filter(move |quote| quote.sector == sector);
                let up = members().filter(|quote| quote.last > quote.close).count();
                let total = members().count();
                let (best, worst) = sector_extremes(board, sector);
                draft.mention(best.id);
                draft.mention(worst.id);
                write!(
                    draft.headline,
                    "{up} of {total} simulated {} names higher",
                    sector.label()
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "In the simulated {} group, {up} names trade above their simulated \
                         previous close and {} at or below it. The group's average move is \
                         {:+.2}%.\n\n{} leads at {:+.2}%; {} trails at {:+.2}%.",
                        sector.label(),
                        total - up,
                        sector_average(board, sector),
                        best.ticker,
                        best.change_pct(),
                        worst.ticker,
                        worst.change_pct()
                    )
                })
            }
            _ => {
                let (first, last) = (ranking[0], ranking[ranking.len() - 1]);
                let (leader, _) = sector_extremes(board, first.0);
                let (_, laggard) = sector_extremes(board, last.0);
                draft.mention(leader.id);
                draft.mention(laggard.id);
                let _ = write!(
                    draft.headline,
                    "{} lead the simulated session, {} lag",
                    first.0.label(),
                    last.0.label()
                );
                let _ = write!(draft.body, "Average simulated moves by sector:");
                for (ix, (sector, average)) in ranking.iter().enumerate() {
                    let _ = write!(
                        draft.body,
                        "{} {} {average:+.2}%",
                        if ix == 0 { "" } else { "," },
                        sector.label()
                    );
                }
                write!(
                    draft.body,
                    ".\n\nThe strongest name in {} is {} at {:+.2}%; the weakest in {} is {} \
                     at {:+.2}%.",
                    first.0.label(),
                    leader.ticker,
                    leader.change_pct(),
                    last.0.label(),
                    laggard.ticker,
                    laggard.change_pct()
                )
            }
        };
    }

    // Equities: a name's move, range, standing in its group or spread; two
    // names that diverge.
    fn equities(&mut self, board: &Board, draft: &mut Draft) {
        let id = self.random_stock(board);
        let quote = &board[id.0 as usize];
        let (ticker, sector) = (quote.ticker, quote.sector.label());
        let change = quote.change_pct();
        let average = sector_average(board, quote.sector);
        draft.mention(id);
        let _ = match self.rng.u8(..5) {
            0 => {
                let verb = if change >= 0. { "rises" } else { "falls" };
                write!(
                    draft.headline,
                    "{ticker} {verb} {:.2}% in simulated trade",
                    change.abs()
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "{ticker} ({}, {sector}) last changed hands at {:.2} in the \
                         simulation, {:+.2} or {change:+.2}% against its simulated previous \
                         close of {:.2}.\n\nThe simulated {sector} group averages \
                         {average:+.2}% over the same session.",
                        quote.name,
                        quote.last,
                        quote.change(),
                        quote.close
                    )
                })
            }
            1 => {
                let (low, high) = self.ranges[id.0 as usize];
                let share = if high > low {
                    (quote.last - low) / (high - low)
                } else {
                    0.5
                };
                if share >= 0.8 {
                    write!(
                        draft.headline,
                        "{ticker} trades near its simulated session high of {high:.2}"
                    )
                } else if share <= 0.2 {
                    write!(
                        draft.headline,
                        "{ticker} trades near its simulated session low of {low:.2}"
                    )
                } else {
                    write!(
                        draft.headline,
                        "{ticker} holds mid-range in simulated trade at {:.2}",
                        quote.last
                    )
                }
                .and_then(|()| {
                    write!(
                        draft.body,
                        "Since the session simulator opened, {ticker} has traded between \
                         {low:.2} and {high:.2}. The last simulated price, {:.2}, sits {:.0}% \
                         of the way up that range.\n\nThe simulated quote is {:.2} bid, {:.2} \
                         offered.",
                        quote.last,
                        share * 100.,
                        quote.bid,
                        quote.ask
                    )
                })
            }
            2 => {
                let (best, worst) = sector_extremes(board, quote.sector);
                draft.mention(best.id);
                draft.mention(worst.id);
                let verb = if change >= average {
                    "outpaces"
                } else {
                    "trails"
                };
                write!(
                    draft.headline,
                    "{ticker} {verb} simulated {sector} by {:.2} points",
                    (change - average).abs()
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "{ticker} stands at {change:+.2}% on the simulated session against a \
                         {sector} average of {average:+.2}%.\n\nWithin the group, {} leads at \
                         {:+.2}% and {} trails at {:+.2}%.",
                        best.ticker,
                        best.change_pct(),
                        worst.ticker,
                        worst.change_pct()
                    )
                })
            }
            3 => {
                let (best, worst) = sector_extremes(board, quote.sector);
                draft.tickers.clear();
                draft.mention(best.id);
                draft.mention(worst.id);
                write!(
                    draft.headline,
                    "{} and {} diverge in simulated {sector} trade",
                    best.ticker, worst.ticker
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "{} is {:+.2}% and {} {:+.2}% on the simulated session, a gap of {:.2} \
                         points between the group's strongest and weakest names.\n\nThe \
                         simulated {sector} average is {average:+.2}%.",
                        best.ticker,
                        best.change_pct(),
                        worst.ticker,
                        worst.change_pct(),
                        best.change_pct() - worst.change_pct()
                    )
                })
            }
            _ => {
                let spread = quote.ask - quote.bid;
                let bps = spread / quote.last * 1e4;
                write!(
                    draft.headline,
                    "Simulated spread in {ticker} at {bps:.1} bps"
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "The simulated quote in {ticker} is {:.2} bid, {:.2} offered: a spread \
                         of {spread:.2}, or {bps:.1} basis points of the last price.\n\nSpreads \
                         in the simulation follow the price level; they say nothing about real \
                         liquidity.",
                        quote.bid, quote.ask
                    )
                })
            }
        };
    }

    // Volume: one name against its group, the most active names, turnover
    // by sector.
    fn volume(&mut self, board: &Board, draft: &mut Draft) {
        let _ = match self.rng.u8(..3) {
            0 => {
                let id = self.random_stock(board);
                let quote = &board[id.0 as usize];
                draft.mention(id);
                let peers: Vec<i64> = board
                    .iter()
                    .filter(|peer| peer.sector == quote.sector)
                    .map(|peer| peer.volume)
                    .collect();
                let average = peers.iter().sum::<i64>() as f64 / peers.len().max(1) as f64;
                let rank = 1 + peers
                    .iter()
                    .filter(|volume| **volume > quote.volume)
                    .count();
                write!(
                    draft.headline,
                    "Simulated volume in {} at {:.1}× the {} mean",
                    quote.ticker,
                    quote.volume as f64 / average.max(1.),
                    quote.sector.label()
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "{} has printed {} shares in the simulated session against a {} \
                         average of {}. It ranks {rank} of {} in its group by simulated \
                         volume.",
                        quote.ticker,
                        thousands(quote.volume),
                        quote.sector.label(),
                        thousands(average.round() as i64),
                        peers.len()
                    )
                })
            }
            1 => {
                let mut active: Vec<&Arc<Quote>> =
                    board.iter().filter(|quote| is_stock(quote)).collect();
                active.sort_by_key(|quote| std::cmp::Reverse(quote.volume));
                active.truncate(3);
                for quote in &active {
                    draft.mention(quote.id);
                }
                let _ = write!(
                    draft.headline,
                    "Most active simulated names: {}, {}, {}",
                    active[0].ticker, active[1].ticker, active[2].ticker
                );
                let _ = write!(
                    draft.body,
                    "By simulated share volume the session is led by"
                );
                for (ix, quote) in active.iter().enumerate() {
                    let _ = write!(
                        draft.body,
                        "{} {} ({} shares, {:+.2}%)",
                        match ix {
                            0 => "",
                            1 => ",",
                            _ => " and",
                        },
                        quote.ticker,
                        thousands(quote.volume),
                        quote.change_pct()
                    );
                }
                write!(draft.body, ".")
            }
            _ => {
                let mut turnover: Vec<(Sector, f64)> = STOCK_SECTORS
                    .iter()
                    .map(|&sector| {
                        let sum = board
                            .iter()
                            .filter(|quote| quote.sector == sector)
                            .map(|quote| quote.last * quote.volume as f64)
                            .sum::<f64>();
                        (sector, sum)
                    })
                    .collect();
                turnover.sort_by(|a, b| b.1.total_cmp(&a.1));
                let (first, second) = (turnover[0], turnover[1]);
                let mut members: Vec<&Arc<Quote>> = board
                    .iter()
                    .filter(|quote| quote.sector == first.0)
                    .collect();
                members.sort_by(|a, b| {
                    (b.last * b.volume as f64).total_cmp(&(a.last * a.volume as f64))
                });
                draft.mention(members[0].id);
                draft.mention(members[1].id);
                write!(
                    draft.headline,
                    "{} turnover leads the simulated session",
                    first.0.label()
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "Simulated turnover in {} stands at {:.1} million, the largest of the \
                         {} sectors; {} follows at {:.1} million.\n\nWithin {}, {} and {} \
                         account for the most simulated turnover.",
                        first.0.label(),
                        first.1 / 1e6,
                        turnover.len(),
                        second.0.label(),
                        second.1 / 1e6,
                        first.0.label(),
                        members[0].ticker,
                        members[1].ticker
                    )
                })
            }
        };
    }

    // Macro-sim: gauges computed from the simulated quotes.
    fn gauges(&mut self, board: &Board, draft: &mut Draft) {
        let stocks = || board.iter().filter(|quote| is_stock(quote));
        let total = stocks().count();
        let _ = match self.rng.u8(..3) {
            0 => {
                let average = mean(stocks().map(|quote| quote.change_pct()));
                let dispersion =
                    mean(stocks().map(|quote| (quote.change_pct() - average).powi(2))).sqrt();
                let previous = self.dispersion.replace(dispersion);
                let _ = write!(
                    draft.headline,
                    "Simulated dispersion gauge at {dispersion:.2}"
                );
                match previous {
                    Some(previous) if (dispersion - previous).abs() < 0.005 => {
                        draft.headline.push_str(", little changed");
                    }
                    Some(previous) => {
                        let direction = if dispersion > previous { "up" } else { "down" };
                        let _ = write!(draft.headline, ", {direction} from {previous:.2}");
                    }
                    None => {}
                }
                write!(
                    draft.body,
                    "The gauge is the cross-sectional standard deviation of session moves \
                     across the {total} simulated stocks, in percentage points. A higher \
                     reading means the simulated names drift further apart.\n\nThe average \
                     simulated move is {average:+.2}%."
                )
            }
            1 => {
                let (up, down) = stocks().fold((0i64, 0i64), |(up, down), quote| {
                    if quote.last > quote.close {
                        (up + quote.volume, down)
                    } else {
                        (up, down + quote.volume)
                    }
                });
                write!(
                    draft.headline,
                    "Simulated up/down volume ratio at {:.2}",
                    up as f64 / down.max(1) as f64
                )
                .and_then(|()| {
                    write!(
                        draft.body,
                        "Simulated volume in advancing names totals {} shares, against {} in \
                         names at or below their simulated previous close.",
                        thousands(up),
                        thousands(down)
                    )
                })
            }
            _ => {
                let gauge = 100. * mean(stocks().map(|quote| quote.last / quote.close));
                let spy = etf(board, "SPY");
                draft.mention(spy.id);
                write!(draft.headline, "Simulated equal-weight gauge at {gauge:.2}").and_then(|()| {
                    write!(
                        draft.body,
                        "An equal-weighted gauge of the {total} simulated stocks, set to 100 at \
                         their simulated previous closes, reads {gauge:.2}.\n\nThe simulated \
                         SPY is {:+.2}% over the same session.",
                        spy.change_pct()
                    )
                })
            }
        };
    }
}

/// The sectors of the stocks: every sector but the index funds.
const STOCK_SECTORS: [Sector; 10] = [
    Sector::Technology,
    Sector::Financials,
    Sector::HealthCare,
    Sector::ConsumerDiscretionary,
    Sector::CommunicationServices,
    Sector::Industrials,
    Sector::ConsumerStaples,
    Sector::Energy,
    Sector::Utilities,
    Sector::Materials,
];

fn is_stock(quote: &Quote) -> bool {
    STOCK_SECTORS.contains(&quote.sector)
}

fn etf<'a>(board: &'a Board, ticker: &str) -> &'a Quote {
    board
        .iter()
        .find(|quote| quote.ticker == ticker)
        .expect("the index funds are instruments")
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, count) = values.fold((0., 0u32), |(sum, count), value| (sum + value, count + 1));
    sum / f64::from(count.max(1))
}

/// The quotes with the highest and the lowest `key`.
fn extremes<'a>(
    quotes: impl Iterator<Item = &'a Arc<Quote>>,
    key: impl Fn(&Quote) -> f64,
) -> (&'a Arc<Quote>, &'a Arc<Quote>) {
    let quotes: Vec<&Arc<Quote>> = quotes.collect();
    let best = quotes
        .iter()
        .copied()
        .max_by(|a, b| key(a).total_cmp(&key(b)))
        .expect("quotes");
    let worst = quotes
        .iter()
        .copied()
        .min_by(|a, b| key(a).total_cmp(&key(b)))
        .expect("quotes");
    (best, worst)
}

/// The best and the worst move of `sector`.
fn sector_extremes(board: &Board, sector: Sector) -> (&Arc<Quote>, &Arc<Quote>) {
    extremes(
        board.iter().filter(|quote| quote.sector == sector),
        Quote::change_pct,
    )
}

fn sector_average(board: &Board, sector: Sector) -> f64 {
    mean(
        board
            .iter()
            .filter(|quote| quote.sector == sector)
            .map(|quote| quote.change_pct()),
    )
}

/// The stock sectors by average move, best first.
fn sector_ranking(board: &Board) -> Vec<(Sector, f64)> {
    let mut ranking: Vec<(Sector, f64)> = STOCK_SECTORS
        .iter()
        .map(|&sector| (sector, sector_average(board, sector)))
        .collect();
    ranking.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranking
}

/// `1234567` as `1,234,567`.
pub fn thousands(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value < 0 {
        text.push('-');
    }
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix) % 3 == 0 {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// Runs the newsroom on the calling thread until the news store is gone:
/// waits for the first quotes, writes the backlog, says so on `loaded`,
/// then writes a story every few seconds and marks stories read as the
/// window asks. `counts` follows every change.
pub fn run(
    mut newsroom: Newsroom,
    quotes: GridWriter<Quote>,
    news: GridWriter<Story>,
    commands: Receiver<NewsCommand>,
    counts: Arc<NewsCounts>,
    loaded: Sender<()>,
) {
    let first = loop {
        if news.is_closed() {
            return;
        }
        if let Some(board) = board(&quotes.snapshot()) {
            break board;
        }
        std::thread::sleep(POLL);
    };
    newsroom.observe(&first);
    news.replace_all(newsroom.backlog(&first, SystemTime::now()));
    newsroom.count(&counts);
    let _ = loaded.try_send(());

    let mut next = Instant::now() + newsroom.interval();
    while !news.is_closed() {
        match commands.recv_timeout(POLL) {
            Ok(command) => {
                let changed = newsroom.apply(command);
                if !changed.is_empty() {
                    // Not a new story: no flash.
                    news.batch(|rows| {
                        for story in changed {
                            rows.upsert_silent(story);
                        }
                    });
                    newsroom.count(&counts);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some(board) = board(&quotes.snapshot()) else {
            continue;
        };
        newsroom.observe(&board);
        let now = Instant::now();
        if now < next {
            continue;
        }
        next = now + newsroom.interval();
        let (story, evicted) = newsroom.publish(&board, utc_time(SystemTime::now(), false));
        news.batch(|rows| {
            if let Some(seq) = evicted {
                rows.remove(seq);
            }
            rows.upsert(story);
        });
        newsroom.count(&counts);
    }
}

#[cfg(test)]
mod tests {
    use crate::trading::{instruments::INSTRUMENTS, market::Market};

    use super::*;

    fn quotes(market: &Market) -> Vec<Arc<Quote>> {
        market.quotes().iter().cloned().map(Arc::new).collect()
    }

    /// A market moved a little, so stories have moves to describe.
    fn moved_market() -> Market {
        let mut market = Market::new(Some(7));
        for ix in 0..3_000u32 {
            market.update(InstrumentId((ix % INSTRUMENTS.len() as u32) as u16), 1.);
        }
        market
    }

    #[test]
    fn test_stories_are_simulated_and_neutral() {
        // Words of corporate events the stories must never use.
        const BANNED: [&str; 24] = [
            "earnings",
            "revenue",
            "profit",
            "lawsuit",
            "sued",
            "merger",
            "acquisition",
            "acquire",
            "deal",
            "guidance",
            "forecast",
            "ceo",
            "executive",
            "resign",
            "regulator",
            "sec",
            "fda",
            "approval",
            "investigation",
            "dividend",
            "buyback",
            "upgrade",
            "downgrade",
            "analyst",
        ];
        let market = moved_market();
        let board = quotes(&market);
        let mut newsroom = Newsroom::new(Some(7));
        newsroom.observe(&board);
        let mut seen = [false; Category::ALL.len()];
        for _ in 0..400 {
            let (story, _) = newsroom.publish(&board, Arc::from("00:00:00"));
            seen[story.category.ix()] = true;
            assert!(
                story.headline.to_lowercase().contains("simulated"),
                "{}",
                story.headline
            );
            assert!(story.body.ends_with(DISCLAIMER), "{}", story.body);
            let text = format!("{} {}", story.headline, story.body).to_lowercase();
            for word in text.split(|c: char| !c.is_ascii_alphanumeric()) {
                assert!(!BANNED.contains(&word), "{word:?} in {text}");
            }
            // Every ticker a story mentions is named in it.
            for id in story.tickers.iter() {
                let ticker = id.get().ticker;
                assert!(
                    story.headline.contains(ticker) || story.body.contains(ticker),
                    "{ticker} in {story:?}"
                );
            }
            assert_eq!(story.paragraphs().last(), Some(DISCLAIMER));
        }
        assert!(seen.iter().all(|seen| *seen), "{seen:?}");
    }

    #[test]
    fn test_newsroom_keeps_the_latest_and_marks_them_read() {
        let market = moved_market();
        let board = quotes(&market);
        let mut newsroom = Newsroom::new(Some(3));
        let backlog = newsroom.backlog(&board, SystemTime::now());
        assert_eq!(backlog.len(), BACKLOG as usize);
        // Numbered in order, the older ones read and the newer ones not.
        assert!(
            backlog
                .windows(2)
                .all(|pair| pair[0].seq + 1 == pair[1].seq)
        );
        assert!(backlog[0].read && !backlog[backlog.len() - 1].read);

        let counts = NewsCounts::default();
        newsroom.count(&counts);
        let unread = backlog.iter().filter(|story| !story.read).count() as u32;
        assert_eq!(counts.total_unread(), unread);
        assert_eq!(counts.stories(), BACKLOG);

        let newest = backlog[backlog.len() - 1].seq;
        let changed = newsroom.apply(NewsCommand::Read(newest));
        assert_eq!(changed.len(), 1);
        assert!(changed[0].read && changed[0].seq == newest);
        // Read again: nothing changes.
        assert!(newsroom.apply(NewsCommand::Read(newest)).is_empty());
        newsroom.count(&counts);
        assert_eq!(counts.total_unread(), unread - 1);

        assert_eq!(
            newsroom.apply(NewsCommand::ReadAll).len(),
            unread as usize - 1
        );
        newsroom.count(&counts);
        assert_eq!(counts.total_unread(), 0);

        // The store keeps `STORIES_KEPT`: each new story beyond evicts the
        // oldest.
        let mut evicted = Vec::new();
        for _ in 0..STORIES_KEPT {
            let (_, old) = newsroom.publish(&board, Arc::from("00:00:00"));
            evicted.extend(old);
        }
        assert_eq!(evicted, (0..BACKLOG as u64).collect::<Vec<_>>());
        newsroom.count(&counts);
        assert_eq!(counts.stories(), STORIES_KEPT as u32);
        assert_eq!(counts.total_unread(), STORIES_KEPT as u32);
    }

    #[test]
    fn test_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(-1_234_567), "-1,234,567");
    }
}
