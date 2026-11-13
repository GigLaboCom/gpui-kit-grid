//! Who watches the live stream, and what the view does about it: a pure
//! state machine over the page's visibility and interactions, stepped on
//! the view's clock ten times a second.
//!
//! - A tab hidden for [`HIDDEN_GRACE`] closes the stream; a shorter look
//!   away (a page link opened in a new tab and closed again) does not.
//! - A stream closed because the tab was hidden opens again by itself when
//!   the tab is shown, with the transport's usual reconnect: its backoff,
//!   and `Last-Event-ID` or `?since=` while the gap is short enough.
//! - [`IDLE_PAUSE`] without a press, a key or a scroll closes the stream
//!   too; that pause waits for Resume.

use std::time::Duration;

use web_time::Instant;

use super::PauseReason;

/// A live stream closes after this long without a press, a key or a
/// scroll where a `Presence` counts them.
pub const IDLE_PAUSE: Duration = Duration::from_secs(5 * 60);

/// How long the tab may be hidden before the live stream closes: a link
/// opened in a new tab and read for a minute keeps it open.
pub const HIDDEN_GRACE: Duration = Duration::from_secs(60);

/// What the view does after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing changes.
    Keep,
    /// Close the stream, for this reason.
    Pause(PauseReason),
    /// Open the stream again: the tab that closed it is shown.
    Resume,
}

/// The view's stream as attendance sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Live,
    Paused(PauseReason),
    /// Idle, a replay: nothing to watch over.
    Other,
}

/// What attendance remembers between steps.
#[derive(Clone, Copy, Debug)]
pub struct Attendance {
    /// The interaction count last seen and when it last rose.
    interactions: (u64, Instant),
    /// Since when the tab is hidden.
    hidden_since: Option<Instant>,
}

impl Attendance {
    pub fn new(now: Instant) -> Self {
        Self {
            interactions: (0, now),
            hidden_since: None,
        }
    }

    /// The stream opened at `now`: the idle time counts from here.
    pub fn started(&mut self, now: Instant) {
        self.interactions.1 = now;
    }

    /// One step at `now`, with the page `hidden` or not and its count of
    /// `interactions`.
    pub fn step(
        &mut self,
        now: Instant,
        hidden: bool,
        interactions: u64,
        stream: Stream,
    ) -> Verdict {
        if interactions != self.interactions.0 {
            self.interactions = (interactions, now);
        }
        if hidden {
            self.hidden_since.get_or_insert(now);
        } else {
            self.hidden_since = None;
        }
        match stream {
            Stream::Live => match self.hidden_since {
                Some(since) if now.saturating_duration_since(since) >= HIDDEN_GRACE => {
                    Verdict::Pause(PauseReason::Hidden)
                }
                Some(_) => Verdict::Keep,
                None if now.saturating_duration_since(self.interactions.1) >= IDLE_PAUSE => {
                    Verdict::Pause(PauseReason::Idle)
                }
                None => Verdict::Keep,
            },
            Stream::Paused(PauseReason::Hidden) if !hidden => {
                // Back: the idle time starts again.
                self.interactions.1 = now;
                Verdict::Resume
            }
            Stream::Paused(_) | Stream::Other => Verdict::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steps `attendance` every 100 ms from `from` for `span`, as the view
    /// does, applying its verdicts to `stream`; returns the verdicts that
    /// were not `Keep`.
    fn run(
        attendance: &mut Attendance,
        stream: &mut Stream,
        from: Instant,
        span: Duration,
        hidden: bool,
        interactions: u64,
    ) -> Vec<Verdict> {
        let mut seen = Vec::new();
        let mut at = Duration::ZERO;
        while at <= span {
            let verdict = attendance.step(from + at, hidden, interactions, *stream);
            match verdict {
                Verdict::Keep => {}
                Verdict::Pause(reason) => *stream = Stream::Paused(reason),
                Verdict::Resume => *stream = Stream::Live,
            }
            if verdict != Verdict::Keep {
                seen.push(verdict);
            }
            at += Duration::from_millis(100);
        }
        seen
    }

    #[test]
    fn test_a_short_look_away_keeps_the_stream() {
        let start = Instant::now();
        let mut attendance = Attendance::new(start);
        let mut stream = Stream::Live;
        // A link opened in a new tab and read for 59 s, then back.
        let seen = run(
            &mut attendance,
            &mut stream,
            start,
            Duration::from_secs(59),
            true,
            0,
        );
        assert!(seen.is_empty(), "{seen:?}");
        let back = start + Duration::from_secs(60);
        assert_eq!(attendance.step(back, false, 1, stream), Verdict::Keep);
        assert_eq!(stream, Stream::Live);
        // Hidden again: the grace starts over.
        let again = back + Duration::from_secs(1);
        let seen = run(
            &mut attendance,
            &mut stream,
            again,
            Duration::from_secs(30),
            true,
            1,
        );
        assert!(seen.is_empty(), "{seen:?}");
    }

    #[test]
    fn test_a_long_hide_pauses_and_the_return_resumes() {
        let start = Instant::now();
        let mut attendance = Attendance::new(start);
        let mut stream = Stream::Live;
        let seen = run(
            &mut attendance,
            &mut stream,
            start,
            Duration::from_secs(90),
            true,
            0,
        );
        assert_eq!(seen, [Verdict::Pause(PauseReason::Hidden)]);
        assert_eq!(stream, Stream::Paused(PauseReason::Hidden));
        // Still hidden: still paused, even past the idle time.
        let later = start + Duration::from_secs(400);
        assert_eq!(attendance.step(later, true, 0, stream), Verdict::Keep);
        // Shown: it opens again by itself, and the idle time starts anew.
        assert_eq!(attendance.step(later, false, 0, stream), Verdict::Resume);
        stream = Stream::Live;
        let soon = later + Duration::from_secs(10);
        assert_eq!(attendance.step(soon, false, 0, stream), Verdict::Keep);
    }

    #[test]
    fn test_idle_pauses_until_resumed_by_hand() {
        let start = Instant::now();
        let mut attendance = Attendance::new(start);
        let mut stream = Stream::Live;
        let seen = run(&mut attendance, &mut stream, start, IDLE_PAUSE, false, 0);
        assert_eq!(seen, [Verdict::Pause(PauseReason::Idle)]);
        // Neither an interaction nor hiding and showing the tab resumes it.
        let later = start + IDLE_PAUSE + Duration::from_secs(5);
        assert_eq!(attendance.step(later, false, 7, stream), Verdict::Keep);
        assert_eq!(attendance.step(later, true, 7, stream), Verdict::Keep);
        assert_eq!(attendance.step(later, false, 7, stream), Verdict::Keep);
        assert_eq!(stream, Stream::Paused(PauseReason::Idle));
        // Resume, by hand: the idle time counts from then.
        attendance.started(later);
        stream = Stream::Live;
        let soon = later + Duration::from_secs(60);
        assert_eq!(attendance.step(soon, false, 7, stream), Verdict::Keep);
        // An interaction keeps it live past the idle time.
        let seen = run(
            &mut attendance,
            &mut stream,
            soon,
            IDLE_PAUSE - Duration::from_secs(61),
            false,
            8,
        );
        assert!(seen.is_empty(), "{seen:?}");
    }

    #[test]
    fn test_a_user_pause_and_a_replay_are_left_alone() {
        let start = Instant::now();
        let mut attendance = Attendance::new(start);
        for stream in [Stream::Paused(PauseReason::User), Stream::Other] {
            let mut stream = stream;
            let seen = run(
                &mut attendance,
                &mut stream,
                start,
                Duration::from_secs(120),
                true,
                0,
            );
            assert!(seen.is_empty());
            assert_eq!(attendance.step(start, false, 0, stream), Verdict::Keep);
        }
    }
}
