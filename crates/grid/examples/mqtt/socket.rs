//! The transport's seam: an open WebSocket ([`Socket`]) and what opens one
//! ([`Opener`]), natively tungstenite over rustls (`native.rs`), in the
//! browser the page's WebSocket (`browser.rs`), in tests a fake. Also the
//! reconnect [`Backoff`] every live feed keeps to.

use std::time::Duration;

/// `Send` natively, where a feed runs on a thread of its own; nothing in
/// the browser, where everything runs on the page's one thread and the
/// browser's WebSocket is not `Send`.
#[cfg(not(target_family = "wasm"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_family = "wasm"))]
impl<T: Send + ?Sized> MaybeSend for T {}
#[cfg(target_family = "wasm")]
pub trait MaybeSend {}
#[cfg(target_family = "wasm")]
impl<T: ?Sized> MaybeSend for T {}

/// What a socket reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketEvent {
    Open,
    Data(Vec<u8>),
    Closed(String),
}

/// An open WebSocket: binary messages in and out.
pub trait Socket: MaybeSend {
    /// What happened since the last call, into `out`.
    fn poll(&mut self, out: &mut Vec<SocketEvent>);

    /// Sends one binary message.
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// Opens WebSockets.
pub trait Opener: MaybeSend {
    /// A WebSocket to `url` with the subprotocol `protocol`. It may be open
    /// at once (its first event is `Open`) or open later.
    fn open(&mut self, url: &str, protocol: &str) -> Result<Box<dyn Socket>, String>;
}

/// When to try again after a connection failed. Waits double from `first`
/// up to `max`, each stretched by a random quarter or less (jitter, never
/// shorter), and two connection starts are never closer than `min_gap`:
/// with 16 s that is at most four a minute, under Digitraffic's limit of
/// five for clients without a `Digitraffic-User` header (a browser's
/// WebSocket cannot send one), and gentle with any public broker.
#[derive(Clone, Debug)]
pub struct Backoff {
    first: Duration,
    max: Duration,
    min_gap: Duration,
    attempt: u32,
    last_start: Option<Duration>,
}

impl Backoff {
    pub fn new(first: Duration, max: Duration, min_gap: Duration) -> Self {
        Self {
            first,
            max,
            min_gap,
            attempt: 0,
            last_start: None,
        }
    }

    /// The examples' own: from 16 s up to 5 minutes, starts 16 s apart.
    pub fn gentle() -> Self {
        Self::new(
            Duration::from_secs(16),
            Duration::from_secs(300),
            Duration::from_secs(16),
        )
    }

    /// Failed attempts since the last connection that stayed up.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// A connection starts at `now`.
    pub fn started(&mut self, now: Duration) {
        self.last_start = Some(now);
    }

    /// When a new connection may start at the earliest.
    pub fn earliest(&self) -> Duration {
        self.last_start
            .map_or(Duration::ZERO, |start| start + self.min_gap)
    }

    /// A connection failed at `now`: when to start the next one. `jitter`
    /// is a random number in [0, 1).
    pub fn failed(&mut self, now: Duration, jitter: f64) -> Duration {
        let base = self
            .first
            .saturating_mul(1 << self.attempt.min(16))
            .min(self.max);
        self.attempt = self.attempt.saturating_add(1);
        let wait = base.mul_f64(1. + jitter.clamp(0., 1.) * 0.25);
        (now + wait).max(self.earliest())
    }

    /// A connection stayed up: the next failure waits `first` again.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn test_backoff_doubles_with_jitter_up_to_its_cap() {
        let mut backoff = Backoff::gentle();
        let mut now = S(0);
        let mut previous = Duration::ZERO;
        for attempt in 0..10 {
            backoff.started(now);
            let next = backoff.failed(now + S(1), 0.);
            let wait = next - (now + S(1));
            let expected = (S(16) * (1 << attempt)).min(S(300));
            assert_eq!(wait, expected, "attempt {attempt}");
            assert!(wait >= previous);
            previous = wait;
            now = next;
        }
        // Jitter only stretches a wait, by a quarter at most.
        let mut backoff = Backoff::gentle();
        let next = backoff.failed(S(100), 0.999);
        assert!(next > S(116) && next <= S(120), "{next:?}");
        backoff.reset();
        assert_eq!(backoff.attempt(), 0);
        assert_eq!(backoff.failed(S(200), 0.), S(216));
    }

    #[test]
    fn test_backoff_never_starts_five_connections_a_minute() {
        // Connections that fail at once, and a reset after each, the worst
        // case: the gap between starts alone holds the limit.
        let mut backoff = Backoff::gentle();
        let mut rng = fastrand::Rng::with_seed(7);
        let mut starts = Vec::new();
        let mut now = S(0);
        for _ in 0..200 {
            now = now.max(backoff.earliest());
            backoff.started(now);
            starts.push(now);
            backoff.reset();
            now = backoff
                .failed(now, rng.f64())
                .min(now + Duration::from_millis(1));
        }
        for (ix, start) in starts.iter().enumerate() {
            let in_a_minute = starts[ix..]
                .iter()
                .take_while(|later| **later < *start + S(60))
                .count();
            assert!(in_a_minute <= 4, "{in_a_minute} starts from {start:?}");
        }
    }
}
