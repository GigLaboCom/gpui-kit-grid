//! Where edits come from, between the transports and the aggregation: an
//! [`EditSource`] hands the pump (`pump.rs`) the edits that arrived since
//! its last step, each stamped on the source's own clock, and says what
//! state it is in. Two sources:
//!
//! - [`LiveEdits`]: Wikimedia's stream, read from an [`Inbox`] that a
//!   transport fills: natively an HTTP client on its own thread
//!   (`native.rs`), in the browser an `EventSource` (the web crate). The
//!   transport parses each event (`edit.rs`) as it arrives, so the inbox
//!   never holds more than the fields shown.
//! - [`Replay`]: a recording (JSON lines of edits), played at ×1 or faster
//!   and looped; the snapshots, the browser checks and the fallback when
//!   the stream is unreachable use it.
//!
//! [`Backoff`] is the transports' reconnect timing, pure and shared. No
//! GPUI types.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use gpui_kit_grid::Clock;
use parking_lot::Mutex;
use web_time::Instant;

use super::edit::{Edit, Skipped, parse_change};

/// Wikimedia's stream of recent changes on every wiki (SSE).
pub const STREAM_URL: &str = "https://stream.wikimedia.org/v2/stream/recentchange";

/// What the native client sends as `User-Agent`, as Wikimedia's User-Agent
/// policy asks: the tool, its version and where to find it (the
/// repository), then the library. The browser sends its own and cannot be
/// told otherwise.
pub const USER_AGENT: &str = concat!(
    "gpui-kit-grid-wiki-example/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/GigLaboCom/gpui-kit-grid; a gpui-kit-grid example) ureq/3"
);

/// Longest a connection may have been gone for a reconnect to continue
/// where it stopped (`Last-Event-ID`); after a longer gap the stream
/// starts at the present, so a resume never asks the server for minutes
/// of history.
pub const RESUME_GAP: Duration = Duration::from_secs(60);

/// The time an event ID points at, Unix ms: the newest `timestamp` of
/// its Kafka positions (`[{"topic":…,"partition":0,"timestamp":…}, …]`;
/// the service gives timestamps, not offsets, as it runs in two data
/// centres). The browser cannot send `Last-Event-ID` on a connection it
/// opens itself, so it resumes from this time with `?since=`.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub fn resume_time_ms(event_id: &str) -> Option<u64> {
    let positions: Vec<serde_json::Value> = serde_json::from_str(event_id).ok()?;
    positions
        .iter()
        .filter_map(|position| position.get("timestamp")?.as_u64())
        .max()
}

/// The state of a source, for the status bar.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedState {
    Connecting,
    Live,
    /// Waiting `wait` before connection attempt `attempt` (from 1).
    Reconnecting {
        attempt: u32,
        wait: Duration,
    },
    /// The transport gave up: too many failed attempts in a row.
    Unreachable,
    /// A recording, played `speed` times as fast, `loops` times over.
    Replay {
        speed: u32,
        loops: u32,
    },
}

impl FeedState {
    /// A word for the status bar.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Live => "live",
            Self::Reconnecting { .. } => "reconnecting",
            Self::Unreachable => "unreachable",
            Self::Replay { .. } => "replay",
        }
    }
}

/// What a source counted since it started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Events parsed into edits, of every type.
    pub events: u64,
    pub canary: u64,
    pub malformed: u64,
    /// Connections opened after the first.
    pub reconnects: u32,
}

/// A source after a step.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceStatus {
    /// The source's clock: ms since it started (live), or the recording's
    /// time played (replay). The aggregation's windows run on it, so a
    /// replay at ×4 still counts edits per minute of the recording.
    pub now_ms: u64,
    pub state: FeedState,
    pub counters: Counters,
    /// Why the last connection ended, if it did.
    pub error: Option<String>,
}

/// Where edits come from: the live stream or a recording. The pump calls
/// `take` once a step, from one thread at a time.
pub trait EditSource: Send {
    /// Moves the edits that arrived since the last call into `out`, in
    /// order, each `at_ms` on the source's clock, and reports the source.
    fn take(&mut self, now: Instant, out: &mut Vec<Edit>) -> SourceStatus;
}

/// The live stream's side of a transport: it reports its connection and
/// hands over each event's data; [`LiveEdits`] reads it. `Send + Sync`: the
/// native transport writes from its own thread.
pub struct Inbox {
    clock: Arc<dyn Clock>,
    state: Mutex<InboxState>,
}

struct InboxState {
    edits: Vec<(Instant, Edit)>,
    feed: FeedState,
    counters: Counters,
    error: Option<String>,
    last_event_id: String,
    last_arrival: Option<Instant>,
    opened: bool,
}

impl Inbox {
    /// An inbox stamping arrivals with `clock`, the store's.
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            state: Mutex::new(InboxState {
                edits: Vec::new(),
                feed: FeedState::Connecting,
                counters: Counters::default(),
                error: None,
                last_event_id: String::new(),
                last_arrival: None,
                opened: false,
            }),
        })
    }

    pub fn now(&self) -> Instant {
        self.clock.now()
    }

    /// A connection attempt starts.
    pub fn connecting(&self) {
        self.state.lock().feed = FeedState::Connecting;
    }

    /// The connection is open.
    pub fn opened(&self) {
        let mut state = self.state.lock();
        if state.opened {
            state.counters.reconnects += 1;
        }
        state.opened = true;
        state.feed = FeedState::Live;
    }

    /// One event's `data`, with the stream's last event ID after it.
    pub fn receive(&self, data: &str, event_id: &str) {
        let now = self.clock.now();
        let parsed = parse_change(data);
        let mut state = self.state.lock();
        state.feed = FeedState::Live;
        state.last_arrival = Some(now);
        if !event_id.is_empty() && state.last_event_id != event_id {
            state.last_event_id = event_id.to_owned();
        }
        match parsed {
            Ok(edit) => {
                state.counters.events += 1;
                state.edits.push((now, edit));
            }
            Err(Skipped::Canary) => state.counters.canary += 1,
            Err(Skipped::Malformed) => state.counters.malformed += 1,
            Err(Skipped::NoWiki) => state.counters.events += 1,
        }
    }

    /// The connection ended (`reason`); the next attempt, `attempt`, is in
    /// `wait`.
    pub fn waiting(&self, attempt: u32, wait: Duration, reason: impl Into<String>) {
        let mut state = self.state.lock();
        state.feed = FeedState::Reconnecting { attempt, wait };
        state.error = Some(reason.into());
    }

    /// The transport gave up.
    pub fn unreachable(&self, reason: impl Into<String>) {
        let mut state = self.state.lock();
        state.feed = FeedState::Unreachable;
        state.error = Some(reason.into());
    }

    /// The ID to resume from (`Last-Event-ID`): the last one seen, if an
    /// event arrived within [`RESUME_GAP`].
    pub fn resume_id(&self) -> Option<String> {
        let now = self.clock.now();
        let state = self.state.lock();
        let recent = state
            .last_arrival
            .is_some_and(|at| now.saturating_duration_since(at) <= RESUME_GAP);
        (recent && !state.last_event_id.is_empty()).then(|| state.last_event_id.clone())
    }
}

/// The live stream as a source: what its transport put in the inbox. Its
/// clock counts from `origin`, so a stream resumed after a pause goes on
/// with the same clock, and the minutes in between empty the windows.
pub struct LiveEdits {
    inbox: Arc<Inbox>,
    origin: Instant,
    taken: Vec<(Instant, Edit)>,
}

impl LiveEdits {
    pub fn new(inbox: Arc<Inbox>, origin: Instant) -> Self {
        Self {
            inbox,
            origin,
            taken: Vec::new(),
        }
    }
}

impl EditSource for LiveEdits {
    fn take(&mut self, now: Instant, out: &mut Vec<Edit>) -> SourceStatus {
        let origin = self.origin;
        let ms = |at: Instant| at.saturating_duration_since(origin).as_millis() as u64;
        let (feed, counters, error) = {
            let mut state = self.inbox.state.lock();
            std::mem::swap(&mut state.edits, &mut self.taken);
            (state.feed.clone(), state.counters, state.error.clone())
        };
        let now_ms = ms(now);
        out.extend(self.taken.drain(..).map(|(at, mut edit)| {
            edit.at_ms = ms(at).min(now_ms);
            edit
        }));
        SourceStatus {
            now_ms,
            state: feed,
            counters,
            error,
        }
    }
}

/// A few minutes of edits, recorded by `WIKI_RECORD` (`native.rs`): JSON
/// lines, one [`Edit`] each, `at` in ms since the recording started. Like
/// the stream, it holds no user names, and its titles are the wikis'
/// content (CC BY-SA 4.0).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recording {
    edits: Vec<Edit>,
}

/// The pause between the end of a recording and its next loop.
const LOOP_GAP_MS: u64 = 1_000;
/// The most of the recording one step plays: a step after a long stall
/// does not flood the table.
const MAX_STEP_MS: f64 = 2_000.;

impl Recording {
    /// `edits`, ordered by time.
    pub fn new(mut edits: Vec<Edit>) -> Self {
        edits.sort_by_key(|edit| edit.at_ms);
        Self { edits }
    }

    /// Reads JSON lines; blank lines are skipped, a line that is not an
    /// edit is an error.
    pub fn parse(text: &str) -> Result<Self, String> {
        let edits = text
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(ix, line)| {
                serde_json::from_str::<Edit>(line)
                    .map_err(|error| format!("line {}: {error}", ix + 1))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(edits))
    }

    /// The recording as JSON lines.
    pub fn to_json_lines(&self) -> String {
        let mut text = String::new();
        for edit in &self.edits {
            if let Ok(line) = serde_json::to_string(edit) {
                text.push_str(&line);
                text.push('\n');
            }
        }
        text
    }

    pub fn len(&self) -> usize {
        self.edits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// From the first edit to the last, ms.
    pub fn span_ms(&self) -> u64 {
        self.edits.last().map_or(0, |edit| edit.at_ms)
    }
}

/// A recording played as a source, `speed` times as fast (shared, so the
/// view changes it while the pump plays), looped with a short pause.
pub struct Replay {
    recording: Arc<Recording>,
    speed: Arc<AtomicU32>,
    last: Option<Instant>,
    /// The recording's time played, over every loop.
    played_ms: f64,
    cursor: usize,
    loops: u32,
    counters: Counters,
}

impl Replay {
    pub fn new(recording: Arc<Recording>, speed: Arc<AtomicU32>) -> Self {
        Self {
            recording,
            speed,
            last: None,
            played_ms: 0.,
            cursor: 0,
            loops: 0,
            counters: Counters::default(),
        }
    }

    fn period_ms(&self) -> u64 {
        self.recording.span_ms() + LOOP_GAP_MS
    }
}

impl EditSource for Replay {
    fn take(&mut self, now: Instant, out: &mut Vec<Edit>) -> SourceStatus {
        let speed = self.speed.load(Ordering::Relaxed).max(1);
        let elapsed = self.last.map_or(0., |last| {
            now.saturating_duration_since(last).as_secs_f64() * 1e3
        });
        self.last = Some(now);
        self.played_ms += elapsed.min(MAX_STEP_MS) * f64::from(speed);
        let edits = &self.recording.edits;
        if !edits.is_empty() {
            let period = self.period_ms();
            loop {
                let base = u64::from(self.loops) * period;
                while let Some(edit) = edits.get(self.cursor) {
                    let at = base + edit.at_ms;
                    if at as f64 > self.played_ms {
                        break;
                    }
                    let mut edit = edit.clone();
                    edit.at_ms = at;
                    out.push(edit);
                    self.cursor += 1;
                    self.counters.events += 1;
                }
                if self.cursor == edits.len() && self.played_ms >= (base + period) as f64 {
                    self.loops += 1;
                    self.cursor = 0;
                } else {
                    break;
                }
            }
        }
        SourceStatus {
            now_ms: self.played_ms as u64,
            state: FeedState::Replay {
                speed,
                loops: self.loops,
            },
            counters: self.counters,
            error: None,
        }
    }
}

/// The first wait before reconnecting; it doubles up to `BACKOFF_MAX`.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A connection that stayed up this long was a good one: the next failure
/// waits the shortest again. The server closes every connection after
/// 15 minutes, and that reconnect should be quick.
pub const STABLE: Duration = Duration::from_secs(30);
/// Failed attempts in a row before the transport gives up.
pub const GIVE_UP: u32 = 6;

/// The reconnect timing: exponential backoff with jitter over the upper
/// half of each wait, never below the server's `retry` or `Retry-After`,
/// reset by a stable connection, given up after [`GIVE_UP`] failures in a
/// row.
#[derive(Clone, Debug, Default)]
pub struct Backoff {
    failures: u32,
    floor: Duration,
}

impl Backoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// A connection failed or ended: the wait before the next attempt,
    /// with `random` in `[0, 1)` for the jitter, or `None` to give up.
    pub fn failed(&mut self, random: f64) -> Option<Duration> {
        self.failures += 1;
        if self.failures > GIVE_UP {
            return None;
        }
        let doubled = BACKOFF_MIN.saturating_mul(1 << (self.failures - 1).min(16));
        let base = doubled.min(BACKOFF_MAX);
        let jittered = base.mul_f64(0.5 + 0.5 * random.clamp(0., 1.));
        Some(jittered.max(self.floor))
    }

    /// The connection was up for `duration` before it ended.
    pub fn was_live_for(&mut self, duration: Duration) {
        if duration >= STABLE {
            self.failures = 0;
        }
    }

    /// The server asked for at least `floor` between attempts (the SSE
    /// `retry` field, an HTTP `Retry-After`).
    pub fn set_floor(&mut self, floor: Duration) {
        self.floor = floor.min(BACKOFF_MAX * 5);
    }

    /// The attempt the next connection is, from 1.
    pub fn attempt(&self) -> u32 {
        self.failures + 1
    }
}

#[cfg(test)]
mod tests {
    use super::super::edit::{EditType, Page};
    use super::*;

    /// A clock the test moves.
    struct TestClock(Mutex<Instant>);

    impl TestClock {
        fn advance(&self, ms: u64) {
            *self.0.lock() += Duration::from_millis(ms);
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            *self.0.lock()
        }
    }

    fn edit(at_ms: u64, wiki: &str) -> Edit {
        Edit {
            at_ms,
            wiki: wiki.into(),
            kind: EditType::Edit,
            bot: false,
            delta: 10,
            time: "12:00:00".into(),
            page: Some(Page {
                title: "Page".into(),
                url: "https://en.wikipedia.org/wiki/Page".into(),
            }),
        }
    }

    #[test]
    fn test_backoff_doubles_with_jitter_up_to_the_cap() {
        let mut backoff = Backoff::new();
        let low: Vec<Duration> = (0..GIVE_UP).map(|_| backoff.failed(0.).unwrap()).collect();
        assert_eq!(
            low.iter().map(Duration::as_millis).collect::<Vec<_>>(),
            [500, 1_000, 2_000, 4_000, 8_000, 16_000]
        );
        assert_eq!(
            backoff.failed(0.),
            None,
            "gives up after {GIVE_UP} in a row"
        );
        let mut backoff = Backoff::new();
        for attempt in 0..GIVE_UP {
            let wait = backoff.failed(0.999).unwrap();
            let base = (BACKOFF_MIN * (1 << attempt)).min(BACKOFF_MAX);
            assert!(wait >= base / 2 && wait <= base, "{wait:?} for {base:?}");
        }
    }

    #[test]
    fn test_backoff_resets_after_a_stable_connection_and_keeps_the_floor() {
        let mut backoff = Backoff::new();
        for _ in 0..4 {
            backoff.failed(0.5);
        }
        backoff.was_live_for(STABLE / 2);
        assert_eq!(backoff.attempt(), 5, "a short connection does not reset");
        backoff.was_live_for(STABLE);
        assert_eq!(backoff.attempt(), 1);
        // The 15-minute close of a good connection: back within a second.
        assert!(backoff.failed(0.5).unwrap() <= BACKOFF_MIN);
        backoff.set_floor(Duration::from_secs(5));
        assert_eq!(backoff.failed(0.).unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn test_inbox_drops_canaries_and_stamps_arrivals() {
        let clock = Arc::new(TestClock(Mutex::new(Instant::now())));
        let inbox = Inbox::new(clock.clone());
        let mut live = LiveEdits::new(inbox.clone(), clock.now());
        let mut out = Vec::new();
        assert_eq!(
            live.take(clock.now(), &mut out).state,
            FeedState::Connecting
        );
        inbox.opened();
        clock.advance(100);
        let data = r#"{"meta":{"domain":"de.wikipedia.org","dt":"2026-11-07T10:00:00Z"},"type":"edit","namespace":0,"server_name":"de.wikipedia.org","title":"A","title_url":"https://de.wikipedia.org/wiki/A","length":{"old":1,"new":3}}"#;
        inbox.receive(data, "[1]");
        inbox.receive(r#"{"meta":{"domain":"canary"},"server_name":"x"}"#, "[2]");
        inbox.receive("not json", "[3]");
        clock.advance(400);
        let status = live.take(clock.now(), &mut out);
        assert_eq!(status.state, FeedState::Live);
        assert_eq!(status.now_ms, 500);
        assert_eq!(
            (
                status.counters.events,
                status.counters.canary,
                status.counters.malformed
            ),
            (1, 1, 1)
        );
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].at_ms, &*out[0].wiki), (100, "de.wikipedia.org"));
        assert_eq!(inbox.resume_id().as_deref(), Some("[3]"));
        clock.advance(RESUME_GAP.as_millis() as u64 + 1);
        assert_eq!(
            inbox.resume_id(),
            None,
            "too late to resume: start at the present"
        );
        inbox.opened();
        assert_eq!(live.take(clock.now(), &mut out).counters.reconnects, 1);
    }

    #[test]
    fn test_replay_plays_at_speed_and_loops() {
        let recording = Arc::new(Recording::new(vec![
            edit(0, "a.org"),
            edit(1_000, "b.org"),
            edit(3_000, "c.org"),
        ]));
        let speed = Arc::new(AtomicU32::new(1));
        let mut replay = Replay::new(recording.clone(), speed.clone());
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut out = Vec::new();
        replay.take(at(0), &mut out);
        assert_eq!(out.len(), 1, "the first edit at once");
        replay.take(at(1_500), &mut out);
        assert_eq!(out.len(), 2);
        speed.store(4, Ordering::Relaxed);
        // 1.5 s more at ×4 is 6 s of the recording, 7.5 s played: the
        // rest, then the next loop after its pause (a period of 4 s).
        let status = replay.take(at(3_000), &mut out);
        assert_eq!(status.now_ms, 7_500);
        assert_eq!(status.state, FeedState::Replay { speed: 4, loops: 1 });
        let times: Vec<u64> = out.iter().map(|edit| edit.at_ms).collect();
        assert_eq!(times, [0, 1_000, 3_000, 4_000, 5_000, 7_000]);
        assert_eq!(&*out[3].wiki, "a.org");
    }

    #[test]
    fn test_recording_round_trips_and_names_bad_lines() {
        let recording = Recording::new(vec![edit(5, "b.org"), edit(2, "a.org")]);
        let text = recording.to_json_lines();
        assert!(text.starts_with("{\"at\":2,"), "{text}");
        assert_eq!(Recording::parse(&format!("\n{text}\n")), Ok(recording));
        assert!(Recording::parse("{}\n").unwrap_err().starts_with("line 1"));
    }

    /// The synthetic stream (`fixture.sse`) through the parser, the inbox
    /// and the aggregation, in chunks of 7 bytes.
    #[test]
    fn test_fixture_stream_runs_through_parser_inbox_and_aggregation() {
        use super::super::{
            edit::TitleGlyphs,
            pulse::Pulse,
            sse::{SseItem, SseParser},
        };

        let clock = Arc::new(TestClock(Mutex::new(Instant::now())));
        let inbox = Inbox::new(clock.clone());
        let mut live = LiveEdits::new(inbox.clone(), clock.now());
        inbox.opened();
        let mut parser = SseParser::new();
        let mut items = Vec::new();
        let mut retry = None;
        for chunk in include_bytes!("fixture.sse").chunks(7) {
            parser.feed(chunk, &mut items);
            for item in items.drain(..) {
                match item {
                    SseItem::Event(event) => inbox.receive(&event.data, &event.id),
                    SseItem::Retry(wait) => retry = Some(wait),
                }
            }
            clock.advance(1);
        }
        assert_eq!(retry, Some(Duration::from_secs(3)));
        let mut edits = Vec::new();
        let status = live.take(clock.now(), &mut edits);
        assert_eq!(status.counters.canary, 1);
        assert_eq!(status.counters.malformed, 0, "the last event never ended");
        assert_eq!(edits.len(), 5);
        assert!(
            inbox
                .resume_id()
                .is_some_and(|id| id.contains("1794052805000"))
        );

        let mut pulse = Pulse::new(TitleGlyphs::All);
        for edit in &edits {
            pulse.ingest(edit);
        }
        pulse.advance(status.now_ms);
        let mut rows = Vec::new();
        pulse.changed_rows(&mut rows);
        assert_eq!(rows.len(), 2, "categories and logs make no row");
        let xx = rows
            .iter()
            .find(|row| &*row.wiki == "xx.wikipedia.org")
            .unwrap();
        assert_eq!((xx.edits, xx.added, xx.removed), (2, 50, 30));
        assert_eq!(
            xx.page.as_ref().map(|(title, _)| &**title),
            Some("Synthetic page")
        );
        let wikidata = rows
            .iter()
            .find(|row| &*row.wiki == "www.wikidata.org")
            .unwrap();
        assert_eq!((wikidata.new_pages, wikidata.bots_percent), (1, Some(100)));
        // No one's name is kept: not in the rows, the details or a
        // recording of the edits.
        let kept = format!(
            "{rows:?} {:?} {:?} {}",
            pulse.detail("xx.wikipedia.org"),
            pulse.detail("www.wikidata.org"),
            Recording::new(edits).to_json_lines()
        );
        assert!(!kept.contains("Example Person") && !kept.contains("Example Bot"));
        assert!(!kept.contains("Example comment"));
    }

    #[test]
    fn test_resume_time_is_the_newest_timestamp_of_an_id() {
        let id = r#"[{"topic":"eqiad.mediawiki.recentchange","partition":0,"timestamp":1532031066001},{"topic":"codfw.mediawiki.recentchange","partition":0,"timestamp":1532031066500},{"topic":"x","partition":0,"offset":-1}]"#;
        assert_eq!(resume_time_ms(id), Some(1_532_031_066_500));
        assert_eq!(
            resume_time_ms(r#"[{"topic":"x","partition":0,"offset":12}]"#),
            None
        );
        assert_eq!(resume_time_ms("not json"), None);
    }

    #[test]
    fn test_user_agent_names_the_tool_and_the_repository() {
        assert!(USER_AGENT.starts_with("gpui-kit-grid-wiki-example/"));
        assert!(USER_AGENT.contains("https://github.com/GigLaboCom/gpui-kit-grid"));
        assert!(!USER_AGENT.contains('@'), "no personal address");
    }
}
