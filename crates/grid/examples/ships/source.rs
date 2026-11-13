//! Where the vessels come from: [`VesselSource`], between the transports
//! and the view. A driver polls the source (natively on a thread of its
//! own, in the browser on the page's timer) and hands what it delivers to
//! the fleet. Two sources implement it: live AIS from Digitraffic
//! (`live.rs`, over either WebSocket) and a recording replayed
//! ([`ReplaySource`]).
//!
//! Also here: the recording format the replay reads and the native
//! recorder writes.

use std::{sync::Arc, time::Duration};

use super::ais::{Message, parse_message};

pub use super::mqtt::socket::MaybeSend;

/// One message as a source delivers it, with when it arrived: Unix ms on
/// the source's clock (the wall clock live, the recording's in a replay).
/// A snapshot's messages carry the time Digitraffic stamped them instead,
/// and are not counted in the rate.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceEvent {
    pub at_ms: f64,
    pub message: Message,
    pub snapshot: bool,
}

/// What the status bar says about a source.
#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    /// Fetching the snapshot or opening the first connection.
    Connecting,
    /// Messages flow.
    Live,
    /// The connection was lost; the next attempt is `in_s` seconds away.
    Reconnecting {
        attempt: u32,
        in_s: u32,
        reason: Arc<str>,
    },
    /// Never reached: the snapshot failed and so did the first connection.
    Unreachable { reason: Arc<str> },
    /// A recording played back, `loops` times over so far.
    Replay { loops: u32 },
}

/// Where the vessels come from. The driver polls it every few tens of
/// milliseconds with its own monotonic time since the start (`now`), so a
/// source never reads a clock of its own and tests run it on a fake one.
pub trait VesselSource: MaybeSend {
    /// Everything that arrived since the last poll, in order, into `out`.
    fn poll(&mut self, now: Duration, out: &mut Vec<SourceEvent>);

    /// The source's clock at `now`, Unix ms: what "seen 2 minutes ago" is
    /// measured against.
    fn clock_ms(&self, now: Duration) -> f64;

    fn connection(&self) -> Connection;

    /// Closes what the source holds open, politely.
    fn close(&mut self, now: Duration) {
        let _ = now;
    }
}

/// A recording: messages with the time each arrived, relative to the
/// recording's start. A text file, one message a line, so it compresses
/// well and reads in an editor:
///
/// ```text
/// # gpui-kit-grid ships: AIS from Fintraffic / digitraffic.fi, license CC 4.0 BY
/// # start 1794052605552
/// -1203000	vessels-v2/230000001/location	{"time":...}
/// 41	vessels-v2/230000001/metadata	{"timestamp":...}
/// ```
///
/// Negative offsets are the snapshot fetched at the start: vessels last
/// seen before it.
#[derive(Clone, Debug, Default)]
pub struct Recording {
    start_ms: f64,
    events: Vec<(f64, Message)>,
}

/// The first line of a recording: what it is and whose data.
pub const RECORDING_HEADER: &str =
    "# gpui-kit-grid ships: AIS from Fintraffic / digitraffic.fi, license CC 4.0 BY";

impl Recording {
    /// Parses a recording. Lines that are not messages are skipped, and
    /// one with a message that does not parse is an error.
    pub fn parse(text: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(text).map_err(|_| "a recording is UTF-8 text")?;
        let mut start_ms = None;
        let mut events = Vec::new();
        for (ix, line) in text.lines().enumerate() {
            if let Some(start) = line.strip_prefix("# start ") {
                start_ms = start.trim().parse::<f64>().ok();
                continue;
            }
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut fields = line.splitn(3, '\t');
            let (Some(offset), Some(topic), Some(payload)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return Err(format!("line {}: not offset, topic and payload", ix + 1));
            };
            let offset: f64 = offset
                .parse()
                .map_err(|_| format!("line {}: no offset", ix + 1))?;
            let message = parse_message(topic, payload.as_bytes())
                .map_err(|error| format!("line {}: {error}", ix + 1))?;
            events.push((offset, message));
        }
        let start_ms = start_ms.ok_or("a recording starts with `# start <Unix ms>`")?;
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(Self { start_ms, events })
    }

    /// When it started, Unix ms.
    pub fn start_ms(&self) -> f64 {
        self.start_ms
    }

    /// The messages and their offsets, in order.
    pub fn events(&self) -> &[(f64, Message)] {
        &self.events
    }

    /// From its start to its last message, ms.
    pub fn duration_ms(&self) -> f64 {
        self.events.last().map_or(0., |(offset, _)| offset.max(0.))
    }

    /// The lines that start a recording made at `start_ms`.
    pub fn header(start_ms: f64) -> String {
        format!("{RECORDING_HEADER}\n# start {start_ms:.0}\n")
    }

    /// One message's line, `offset_ms` after the start.
    pub fn line(offset_ms: f64, message: &Message) -> String {
        let (topic, payload) = message.to_mqtt();
        format!("{offset_ms:.0}\t{topic}\t{payload}\n")
    }
}

/// A recording played back at `speed`, over and over: each loop shifts
/// its times by the recording's length, so the clock never runs back. The
/// snapshot at its start (negative offsets) plays only the first time.
pub struct ReplaySource {
    recording: Arc<Recording>,
    speed: f64,
    /// The next message, the loop it plays in, and the source's clock when
    /// the replay began.
    cursor: usize,
    loops: u32,
    origin: Option<Duration>,
}

/// The pause between the end of a recording and its next loop.
const LOOP_GAP_MS: f64 = 1_000.;

impl ReplaySource {
    pub fn new(recording: Arc<Recording>, speed: f64) -> Self {
        Self {
            recording,
            speed: speed.clamp(0.1, 100.),
            cursor: 0,
            loops: 0,
            origin: None,
        }
    }

    /// The recording's time at `now`, ms since its start.
    fn elapsed_ms(&self, now: Duration) -> f64 {
        let origin = self.origin.unwrap_or(now);
        now.saturating_sub(origin).as_secs_f64() * 1e3 * self.speed
    }

    fn loop_ms(&self) -> f64 {
        self.recording.duration_ms() + LOOP_GAP_MS
    }
}

impl VesselSource for ReplaySource {
    fn poll(&mut self, now: Duration, out: &mut Vec<SourceEvent>) {
        self.origin.get_or_insert(now);
        let elapsed = self.elapsed_ms(now);
        let events = self.recording.events();
        loop {
            let shift = f64::from(self.loops) * self.loop_ms();
            let Some((offset, message)) = events.get(self.cursor) else {
                if events.is_empty() || elapsed < shift + self.loop_ms() {
                    return;
                }
                self.loops += 1;
                // The snapshot at the start plays once.
                self.cursor = events.partition_point(|(offset, _)| *offset < 0.);
                continue;
            };
            if offset + shift > elapsed {
                return;
            }
            out.push(SourceEvent {
                at_ms: self.recording.start_ms() + shift + offset,
                message: message.clone(),
                snapshot: *offset < 0.,
            });
            self.cursor += 1;
        }
    }

    fn clock_ms(&self, now: Duration) -> f64 {
        self.recording.start_ms() + self.elapsed_ms(now)
    }

    fn connection(&self) -> Connection {
        Connection::Replay { loops: self.loops }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ais::tests::{LOCATION, METADATA};
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    fn recording() -> Arc<Recording> {
        let text = format!(
            "{}-60000\tvessels-v2/1/location\t{LOCATION}\n0\tvessels-v2/1/metadata\t{METADATA}\n2000\tvessels-v2/2/location\t{LOCATION}\nnot a message line?\n",
            Recording::header(1_000_000.)
        );
        // The stray line is an error; without it the recording parses.
        assert!(Recording::parse(text.as_bytes()).is_err());
        let text = text.replace("not a message line?\n", "");
        Arc::new(Recording::parse(text.as_bytes()).expect("a recording"))
    }

    #[test]
    fn test_recording_parses_and_writes_the_same_lines() {
        let recording = recording();
        assert_eq!(recording.start_ms(), 1_000_000.);
        assert_eq!(recording.events().len(), 3);
        assert_eq!(recording.duration_ms(), 2_000.);
        let (offset, message) = &recording.events()[1];
        let line = Recording::line(*offset, message);
        let again = Recording::parse(format!("{}{line}", Recording::header(5.)).as_bytes())
            .expect("parses");
        assert_eq!(&again.events()[0].1, message);
        assert!(Recording::parse(b"0\tvessels-v2/1/location\t{}\n").is_err());
    }

    #[test]
    fn test_replay_plays_in_time_and_loops_without_the_snapshot() {
        let mut replay = ReplaySource::new(recording(), 1.);
        let mut out = Vec::new();
        replay.poll(S(10), &mut out);
        // The snapshot and the message at 0.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].at_ms, 1_000_000. - 60_000.);
        assert_eq!(replay.clock_ms(S(10)), 1_000_000.);
        out.clear();
        replay.poll(S(11), &mut out);
        assert!(out.is_empty());
        replay.poll(S(12), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].at_ms, 1_002_000.);
        out.clear();
        // The next loop starts a second after the end, shifted by 3 s.
        replay.poll(S(13), &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].at_ms, 1_003_000.);
        assert_eq!(replay.connection(), Connection::Replay { loops: 1 });
        out.clear();
        replay.poll(S(15), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].at_ms, 1_005_000.);
        assert_eq!(replay.clock_ms(S(15)), 1_005_000.);
    }

    #[test]
    fn test_replay_runs_faster_at_speed() {
        let mut replay = ReplaySource::new(recording(), 10.);
        let mut out = Vec::new();
        replay.poll(S(0), &mut out);
        assert_eq!(out.len(), 2);
        replay.poll(Duration::from_millis(200), &mut out);
        assert_eq!(out.len(), 3);
    }
}
