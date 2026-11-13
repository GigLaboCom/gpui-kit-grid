//! The aggregation: every edit in, one row per wiki out. Pure and
//! single-threaded, on the source's clock (ms), so tests drive it with any
//! times; the pump (`pump.rs`) runs it.
//!
//! - **Edits a minute** is a sliding window: every edit of the last 60 s
//!   in one queue, oldest first; a step drops those that fell out and
//!   lowers their wikis' counts. A second mark at 30 s gives the trend:
//!   rising when the last half minute had clearly more edits than the one
//!   before, falling when clearly fewer.
//! - **Totals** (edits, bots, new pages, bytes added and removed) count
//!   from the source's start.
//! - **Quiet**: a wiki without an edit for 20 s, 1, 3 and 10 minutes goes
//!   one step quieter; the table fades it.
//! - Only the wikis whose numbers changed are written again.
//!
//! The same queue is the pulse panel's last minute of edits (`Tick`).

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use gpui_kit_grid::ChangeDirection;

use super::{
    edit::{Edit, Family, Page, TitleGlyphs, WikiInfo, wiki_info},
    rows::{QUIET_STEPS, Quiet, WikiRow},
};

/// The window of edits a minute.
pub const WINDOW_MS: u64 = 60_000;
const HALF_MS: u64 = WINDOW_MS / 2;
/// Events a second are averaged over this long.
const RATE_MS: u64 = 5_000;
/// The recent edits kept per wiki, for the detail panel.
pub const RECENT_EDITS: usize = 12;
/// Quiet for this long goes one step quieter.
const QUIET_AFTER_MS: [u64; QUIET_STEPS as usize] = [20_000, 60_000, 180_000, 600_000];
/// A trend needs this many edits more in one half than in the other, and
/// a quarter more.
const TREND_MIN: u32 = 2;

/// One edit of the last minute, for the counts and the pulse panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    pub at_ms: u64,
    wiki: u32,
    pub family: Family,
    /// Bytes added (positive) or removed.
    pub delta: i32,
    pub bot: bool,
    pub new_page: bool,
}

/// An edit of one wiki, newest first in [`WikiDetail::recent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentEdit {
    pub time: Arc<str>,
    pub delta: i64,
    pub bot: bool,
    pub new_page: bool,
    /// The title as shown and the address; `None` for a page that is not
    /// shown (a user page).
    pub page: Option<Page>,
}

/// What the detail panel shows of the selected wiki.
#[derive(Clone, Debug, PartialEq)]
pub struct WikiDetail {
    pub wiki: Arc<str>,
    pub info: WikiInfo,
    pub per_minute: u32,
    pub edits: u64,
    pub recent: Vec<RecentEdit>,
}

/// A family's share of the last minute, for the pulse panel's lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lane {
    pub family: Family,
    pub per_minute: u32,
    pub wikis: u32,
}

struct WikiStats {
    wiki: Arc<str>,
    info: WikiInfo,
    per_minute: u32,
    /// Edits in the last half minute.
    recent_half: u32,
    edits: u64,
    bots: u64,
    new_pages: u64,
    added: u64,
    removed: u64,
    last_at: u64,
    last: Arc<str>,
    page: Option<(Arc<str>, Arc<str>)>,
    recent: VecDeque<RecentEdit>,
    quiet: Quiet,
    dirty: bool,
}

impl WikiStats {
    fn new(wiki: Arc<str>) -> Self {
        Self {
            info: wiki_info(&wiki),
            wiki,
            per_minute: 0,
            recent_half: 0,
            edits: 0,
            bots: 0,
            new_pages: 0,
            added: 0,
            removed: 0,
            last_at: 0,
            last: "".into(),
            page: None,
            recent: VecDeque::with_capacity(RECENT_EDITS),
            quiet: 0,
            dirty: true,
        }
    }

    /// Rising or falling: the last half minute against the one before.
    fn trend(&self) -> Option<ChangeDirection> {
        let recent = self.recent_half;
        let older = self.per_minute - recent;
        let clearly = |more: u32, less: u32| more >= less + TREND_MIN && more * 4 >= less * 5;
        if clearly(recent, older) {
            Some(ChangeDirection::Up)
        } else if clearly(older, recent) {
            Some(ChangeDirection::Down)
        } else {
            None
        }
    }

    fn row(&self) -> WikiRow {
        WikiRow {
            wiki: self.wiki.clone(),
            family: self.info.family,
            lang: self.info.lang.clone(),
            language: self.info.language.clone(),
            per_minute: self.per_minute,
            trend: self.trend(),
            edits: self.edits,
            bots_percent: (self.edits > 0)
                .then(|| ((self.bots * 100 + self.edits / 2) / self.edits).min(100) as u8),
            new_pages: self.new_pages,
            added: self.added,
            removed: self.removed,
            last: self.last.clone(),
            page: self.page.clone(),
            quiet: self.quiet,
        }
    }
}

fn quiet_for(silent_ms: u64) -> Quiet {
    QUIET_AFTER_MS
        .iter()
        .take_while(|after| silent_ms >= **after)
        .count() as Quiet
}

/// The aggregation's state.
pub struct Pulse {
    glyphs: TitleGlyphs,
    wikis: Vec<WikiStats>,
    index: HashMap<Arc<str>, u32>,
    /// The edits of the last minute, oldest first.
    window: VecDeque<Tick>,
    /// The first edit of `window` within the last half minute.
    half: usize,
    /// Arrival times of every event of the last [`RATE_MS`].
    events_window: VecDeque<u64>,
    now_ms: u64,
    events: u64,
    edits: u64,
}

impl Pulse {
    /// An empty aggregation showing titles with `glyphs`.
    pub fn new(glyphs: TitleGlyphs) -> Self {
        Self {
            glyphs,
            wikis: Vec::new(),
            index: HashMap::new(),
            window: VecDeque::new(),
            half: 0,
            events_window: VecDeque::new(),
            now_ms: 0,
            events: 0,
            edits: 0,
        }
    }

    /// Counts `edit`, which arrived at `edit.at_ms`.
    pub fn ingest(&mut self, edit: &Edit) {
        let at = edit.at_ms;
        self.events += 1;
        self.events_window.push_back(at);
        if !edit.kind.is_edit() {
            return;
        }
        self.edits += 1;
        let ix = match self.index.get(&edit.wiki) {
            Some(&ix) => ix,
            None => {
                let ix = self.wikis.len() as u32;
                self.wikis.push(WikiStats::new(edit.wiki.clone()));
                self.index.insert(edit.wiki.clone(), ix);
                ix
            }
        };
        let new_page = edit.kind == super::edit::EditType::New;
        let glyphs = &self.glyphs;
        let stats = &mut self.wikis[ix as usize];
        stats.edits += 1;
        stats.bots += u64::from(edit.bot);
        stats.new_pages += u64::from(new_page);
        if edit.delta >= 0 {
            stats.added += edit.delta as u64;
        } else {
            stats.removed += edit.delta.unsigned_abs();
        }
        stats.per_minute += 1;
        stats.recent_half += 1;
        stats.last_at = stats.last_at.max(at);
        stats.last = edit.time.clone();
        stats.quiet = 0;
        stats.dirty = true;
        let shown = edit.page.as_ref().map(|page| Page {
            title: glyphs.shown(&page.title),
            url: page.url.clone(),
        });
        if let Some(page) = &shown {
            stats.page = Some((page.title.clone(), page.url.clone()));
        }
        if stats.recent.len() == RECENT_EDITS {
            stats.recent.pop_back();
        }
        stats.recent.push_front(RecentEdit {
            time: edit.time.clone(),
            delta: edit.delta,
            bot: edit.bot,
            new_page,
            page: shown,
        });
        self.window.push_back(Tick {
            at_ms: at,
            wiki: ix,
            family: stats.info.family,
            delta: edit.delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            bot: edit.bot,
            new_page,
        });
    }

    /// Moves the clock to `now_ms`: edits leave the windows, wikis go
    /// quiet.
    pub fn advance(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
        let now = self.now_ms;
        while let Some(tick) = self.window.get(self.half)
            && tick.at_ms + HALF_MS < now
        {
            let stats = &mut self.wikis[tick.wiki as usize];
            stats.recent_half = stats.recent_half.saturating_sub(1);
            stats.dirty = true;
            self.half += 1;
        }
        while let Some(tick) = self.window.front()
            && tick.at_ms + WINDOW_MS < now
        {
            let stats = &mut self.wikis[tick.wiki as usize];
            stats.per_minute = stats.per_minute.saturating_sub(1);
            stats.dirty = true;
            self.window.pop_front();
            self.half = self.half.saturating_sub(1);
        }
        while self
            .events_window
            .front()
            .is_some_and(|at| at + RATE_MS < now)
        {
            self.events_window.pop_front();
        }
        for stats in &mut self.wikis {
            let quiet = quiet_for(now.saturating_sub(stats.last_at));
            if quiet != stats.quiet {
                stats.quiet = quiet;
                stats.dirty = true;
            }
        }
    }

    /// Appends the rows that changed since the last call.
    pub fn changed_rows(&mut self, out: &mut Vec<WikiRow>) {
        for stats in &mut self.wikis {
            if stats.dirty {
                stats.dirty = false;
                out.push(stats.row());
            }
        }
    }

    /// The edits of the last minute, oldest first.
    pub fn ticks(&self) -> impl ExactSizeIterator<Item = &Tick> {
        self.window.iter()
    }

    /// The families with an edit in the last minute, busiest first, and
    /// how many wikis each has.
    pub fn lanes(&self) -> Vec<Lane> {
        let mut lanes: Vec<Lane> = Family::ALL
            .iter()
            .map(|&family| Lane {
                family,
                per_minute: 0,
                wikis: 0,
            })
            .collect();
        for stats in &self.wikis {
            let lane = &mut lanes[stats.info.family.ix()];
            lane.per_minute += stats.per_minute;
            lane.wikis += 1;
        }
        lanes.retain(|lane| lane.wikis > 0);
        lanes.sort_by(|a, b| {
            b.per_minute
                .cmp(&a.per_minute)
                .then(a.family.cmp(&b.family))
        });
        lanes
    }

    /// The selected wiki's numbers and recent edits.
    pub fn detail(&self, wiki: &str) -> Option<WikiDetail> {
        let stats = &self.wikis[*self.index.get(wiki)? as usize];
        Some(WikiDetail {
            wiki: stats.wiki.clone(),
            info: stats.info.clone(),
            per_minute: stats.per_minute,
            edits: stats.edits,
            recent: stats.recent.iter().cloned().collect(),
        })
    }

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    pub fn wikis(&self) -> usize {
        self.wikis.len()
    }

    /// Edits in the last minute, every wiki.
    pub fn per_minute(&self) -> usize {
        self.window.len()
    }

    /// Events of every type since the start.
    pub fn events(&self) -> u64 {
        self.events
    }

    /// Edits and new pages since the start.
    pub fn edits(&self) -> u64 {
        self.edits
    }

    /// Events a second over the last few seconds (fewer at the start).
    pub fn events_per_second(&self) -> f64 {
        let span = RATE_MS.min(self.now_ms.max(1_000));
        self.events_window.len() as f64 * 1e3 / span as f64
    }
}

#[cfg(test)]
mod tests {
    use super::super::edit::EditType;
    use super::*;

    fn edit(at_ms: u64, wiki: &str, delta: i64) -> Edit {
        Edit {
            at_ms,
            wiki: wiki.into(),
            kind: EditType::Edit,
            bot: false,
            delta,
            time: "10:00:00".into(),
            page: Some(Page {
                title: format!("Page {at_ms}").into(),
                url: format!("https://{wiki}/wiki/Page_{at_ms}").into(),
            }),
        }
    }

    fn rows(pulse: &mut Pulse) -> Vec<WikiRow> {
        let mut out = Vec::new();
        pulse.changed_rows(&mut out);
        out
    }

    fn row<'a>(rows: &'a [WikiRow], wiki: &str) -> &'a WikiRow {
        rows.iter().find(|row| &*row.wiki == wiki).expect("a row")
    }

    #[test]
    fn test_window_counts_the_last_minute_on_a_fake_clock() {
        let mut pulse = Pulse::new(TitleGlyphs::All);
        for at in [0, 10_000, 20_000] {
            pulse.ingest(&edit(at, "en.wikipedia.org", 5));
        }
        pulse.advance(20_000);
        let first = rows(&mut pulse);
        assert_eq!(first.len(), 1);
        assert_eq!(row(&first, "en.wikipedia.org").per_minute, 3);
        assert!(rows(&mut pulse).is_empty(), "nothing changed since");
        // The first edit leaves the window after 60 s, not before.
        pulse.advance(60_000);
        assert!(rows(&mut pulse).iter().all(|row| row.per_minute == 3));
        pulse.advance(60_001);
        let later = rows(&mut pulse);
        assert_eq!(row(&later, "en.wikipedia.org").per_minute, 2);
        assert_eq!(row(&later, "en.wikipedia.org").edits, 3, "totals stay");
        pulse.advance(200_000);
        assert_eq!(row(&rows(&mut pulse), "en.wikipedia.org").per_minute, 0);
        assert_eq!(pulse.per_minute(), 0);
    }

    #[test]
    fn test_trend_compares_the_halves_of_the_minute() {
        let mut pulse = Pulse::new(TitleGlyphs::All);
        for at in [1_000, 2_000] {
            pulse.ingest(&edit(at, "de.wikipedia.org", 1));
        }
        for at in (31_000..41_000).step_by(1_000) {
            pulse.ingest(&edit(at, "de.wikipedia.org", 1));
        }
        pulse.advance(41_000);
        let rising = rows(&mut pulse);
        assert_eq!(
            row(&rising, "de.wikipedia.org").trend,
            Some(ChangeDirection::Up)
        );
        // Thirty seconds later the busy half is the older one.
        pulse.advance(71_500);
        let falling = rows(&mut pulse);
        assert_eq!(row(&falling, "de.wikipedia.org").per_minute, 10);
        assert_eq!(
            row(&falling, "de.wikipedia.org").trend,
            Some(ChangeDirection::Down)
        );
        // An even minute has no trend.
        let mut even = Pulse::new(TitleGlyphs::All);
        for at in [0, 40_000] {
            even.ingest(&edit(at, "fr.wikipedia.org", 1));
        }
        even.advance(40_000);
        assert_eq!(row(&rows(&mut even), "fr.wikipedia.org").trend, None);
    }

    #[test]
    fn test_totals_bots_new_pages_and_bytes() {
        let mut pulse = Pulse::new(TitleGlyphs::All);
        let mut bot = edit(0, "www.wikidata.org", -30);
        bot.bot = true;
        let mut new = edit(1_000, "www.wikidata.org", 200);
        new.kind = EditType::New;
        let mut log = edit(2_000, "www.wikidata.org", 0);
        log.kind = EditType::Log;
        let mut hidden = edit(3_000, "www.wikidata.org", 7);
        hidden.page = None;
        for each in [&bot, &new, &log, &hidden] {
            pulse.ingest(each);
        }
        pulse.advance(3_000);
        let rows = rows(&mut pulse);
        let wikidata = row(&rows, "www.wikidata.org");
        assert_eq!(wikidata.edits, 3, "a log entry is an event, not an edit");
        assert_eq!(pulse.events(), 4);
        assert_eq!((wikidata.added, wikidata.removed), (207, 30));
        assert_eq!(wikidata.new_pages, 1);
        assert_eq!(wikidata.bots_percent, Some(33));
        assert_eq!(wikidata.family, Family::Wikidata);
        // The last page shown is the last one that may be shown.
        assert_eq!(
            wikidata.page.as_ref().map(|(title, _)| &**title),
            Some("Page 1000")
        );
        let detail = pulse.detail("www.wikidata.org").expect("seen");
        assert_eq!(detail.recent.len(), 3);
        assert!(detail.recent[0].page.is_none() && detail.recent[0].delta == 7);
    }

    #[test]
    fn test_quiet_wikis_fade_step_by_step() {
        let mut pulse = Pulse::new(TitleGlyphs::All);
        pulse.ingest(&edit(0, "nl.wikipedia.org", 1));
        pulse.advance(0);
        rows(&mut pulse);
        let quiet_at = |pulse: &mut Pulse, now: u64| {
            pulse.advance(now);
            rows(pulse).first().map(|row| row.quiet)
        };
        assert_eq!(quiet_at(&mut pulse, 19_999), None, "not yet");
        assert_eq!(quiet_at(&mut pulse, 20_000), Some(1));
        assert_eq!(quiet_at(&mut pulse, 61_000), Some(2));
        assert_eq!(quiet_at(&mut pulse, 600_000), Some(QUIET_STEPS));
        pulse.ingest(&edit(600_000, "nl.wikipedia.org", 1));
        assert_eq!(quiet_at(&mut pulse, 600_000), Some(0), "an edit wakes it");
    }

    #[test]
    fn test_lanes_rate_and_recent_edits_cap() {
        let mut pulse = Pulse::new(TitleGlyphs::All);
        for at in 0..20 {
            pulse.ingest(&edit(at * 100, "en.wikipedia.org", 1));
        }
        pulse.ingest(&edit(2_000, "commons.wikimedia.org", 1));
        pulse.advance(5_000);
        let lanes = pulse.lanes();
        assert_eq!(lanes[0].family, Family::Wikipedia);
        assert_eq!((lanes[0].per_minute, lanes[1].per_minute), (20, 1));
        assert_eq!(pulse.events_per_second(), 21. / 5.);
        assert_eq!(
            pulse.detail("en.wikipedia.org").unwrap().recent.len(),
            RECENT_EDITS
        );
        assert_eq!(pulse.ticks().len(), 21);
        assert_eq!(pulse.wikis(), 2);
    }
}
