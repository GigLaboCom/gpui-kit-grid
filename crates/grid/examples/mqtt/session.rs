//! One MQTT subscription kept up over an [`Opener`]: a connection opens,
//! connects, subscribes and stays alive; a lost one (closed, refused, an
//! answer that never came, or silence once live) comes back only after
//! the [`Backoff`]. The owner polls it with its own monotonic time, so a
//! session never reads a clock and tests run it on a fake one, and takes
//! the messages it delivers. One session holds at most one connection.

use std::{sync::Arc, time::Duration};

use super::{
    client::{Client, ClientState, Publish},
    socket::{Backoff, Opener, Socket, SocketEvent},
};

/// The MQTT keep-alive.
pub const KEEP_ALIVE: Duration = Duration::from_secs(30);
/// Silence that counts as a lost connection once live.
pub const SILENCE: Duration = Duration::from_secs(60);
/// A connection up this long starts the backoff over.
pub const STAYED_UP: Duration = Duration::from_secs(60);

enum Phase {
    /// No connection; the next starts at `until`.
    Waiting { until: Duration, reason: Arc<str> },
    Open {
        socket: Box<dyn Socket>,
        client: Client,
        since: Duration,
        live_since: Option<Duration>,
        heard: Duration,
    },
}

/// Where a session is.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionStatus {
    /// Before the first connection, or opening it.
    Connecting,
    /// Subscribed: messages flow.
    Live,
    /// The connection was lost (or never came up); the next attempt is
    /// `in_s` seconds away, 0 while it is being opened.
    Reconnecting {
        attempt: u32,
        in_s: u32,
        reason: Arc<str>,
    },
}

/// A subscription to `filters` at `url`, kept up.
pub struct Session {
    /// Who logs: `ships`, `transit`.
    name: &'static str,
    url: String,
    protocol: String,
    filters: Vec<String>,
    client_id: String,
    keep_alive: Duration,
    silence: Duration,
    phase: Phase,
    backoff: Backoff,
    rng: fastrand::Rng,
    ever_live: bool,
    ever_heard: bool,
    first_start: Option<Duration>,
    down_since: Option<Duration>,
    /// The owner's time at the last poll.
    last_now: Duration,
    connections: u32,
    // Buffers reused between polls.
    socket_events: Vec<SocketEvent>,
    outgoing: Vec<Vec<u8>>,
}

impl Session {
    /// A session named `name` in the logs, to `url` with the WebSocket
    /// subprotocol `protocol`, subscribing to `filters` as `client_id`;
    /// `seed` draws the jitter. The backoff is [`Backoff::gentle`].
    pub fn new(
        name: &'static str,
        url: impl Into<String>,
        protocol: impl Into<String>,
        filters: impl IntoIterator<Item = impl Into<String>>,
        client_id: impl Into<String>,
        seed: u64,
    ) -> Self {
        Self {
            name,
            url: url.into(),
            protocol: protocol.into(),
            filters: filters.into_iter().map(Into::into).collect(),
            client_id: client_id.into(),
            keep_alive: KEEP_ALIVE,
            silence: SILENCE,
            phase: Phase::Waiting {
                until: Duration::ZERO,
                reason: "".into(),
            },
            backoff: Backoff::gentle(),
            rng: fastrand::Rng::with_seed(seed),
            ever_live: false,
            ever_heard: false,
            first_start: None,
            down_since: None,
            last_now: Duration::ZERO,
            connections: 0,
            socket_events: Vec::new(),
            outgoing: Vec::new(),
        }
    }

    /// The same, waiting `backoff` between attempts.
    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Has a connection ever been subscribed?
    pub fn is_ever_live(&self) -> bool {
        self.ever_live
    }

    /// Has a message ever arrived?
    pub fn has_heard(&self) -> bool {
        self.ever_heard
    }

    /// Connections started so far.
    pub fn connections(&self) -> u32 {
        self.connections
    }

    /// Is a new connection due at `now`: none is open and the backoff is
    /// over?
    pub fn is_due(&self, now: Duration) -> bool {
        matches!(&self.phase, Phase::Waiting { until, .. }
            if now >= (*until).max(self.backoff.earliest()))
    }

    /// How long the feed has been down at `now`, if it is: since the first
    /// failure after the last connection that was live.
    pub fn down_for(&self, now: Duration) -> Option<Duration> {
        self.down_since.map(|since| now.saturating_sub(since))
    }

    /// Has the feed stayed out of reach: nothing ever heard after
    /// `attempts` failed connections, or `silent_for` after the first one
    /// started?
    pub fn is_out_of_reach(&self, now: Duration, attempts: u32, silent_for: Duration) -> bool {
        !self.ever_heard
            && self.first_start.is_some_and(|start| {
                self.backoff.attempt() >= attempts || now.saturating_sub(start) >= silent_for
            })
    }

    /// One poll at `now`: opens a connection when one is due, pumps the
    /// open one and puts the messages that arrived into `messages`.
    pub fn poll<O: Opener + ?Sized>(
        &mut self,
        now: Duration,
        opener: &mut O,
        messages: &mut Vec<Publish>,
    ) {
        self.last_now = now;
        if let Phase::Waiting { .. } = &self.phase {
            if !self.is_due(now) {
                return;
            }
            self.connect(now, opener);
        }
        if let Err(reason) = self.pump(now, messages) {
            self.fail(now, reason);
        }
    }

    fn connect<O: Opener + ?Sized>(&mut self, now: Duration, opener: &mut O) {
        self.backoff.started(now);
        self.first_start.get_or_insert(now);
        self.connections += 1;
        out!(
            "{}: connection {} to {}",
            self.name,
            self.connections,
            self.url
        );
        match opener.open(&self.url, &self.protocol) {
            Ok(socket) => {
                let filters: Vec<&str> = self.filters.iter().map(String::as_str).collect();
                self.phase = Phase::Open {
                    socket,
                    client: Client::new(self.client_id.clone(), self.keep_alive, &filters),
                    since: now,
                    live_since: None,
                    heard: now,
                };
            }
            Err(error) => self.fail(now, error),
        }
    }

    /// The connection is lost (or never was): wait for the backoff.
    fn fail(&mut self, now: Duration, reason: String) {
        if let Phase::Open { live_since, .. } = &self.phase
            && live_since.is_some_and(|since| now.saturating_sub(since) >= STAYED_UP)
        {
            self.backoff.reset();
        }
        err!("{}: connection lost ({reason})", self.name);
        let until = self.backoff.failed(now, self.rng.f64());
        self.phase = Phase::Waiting {
            until,
            reason: reason.into(),
        };
        self.down_since.get_or_insert(now);
    }

    /// One poll of the open socket; an error ends the connection.
    fn pump(&mut self, now: Duration, messages: &mut Vec<Publish>) -> Result<(), String> {
        let Phase::Open {
            socket,
            client,
            live_since,
            heard,
            ..
        } = &mut self.phase
        else {
            return Ok(());
        };
        self.socket_events.clear();
        socket.poll(&mut self.socket_events);
        let before = messages.len();
        for event in self.socket_events.drain(..) {
            match event {
                SocketEvent::Open => client.on_open(now, &mut self.outgoing),
                SocketEvent::Data(bytes) => {
                    *heard = now;
                    client
                        .on_bytes(&bytes, now, &mut self.outgoing, messages)
                        .map_err(|error| error.to_string())?;
                }
                SocketEvent::Closed(reason) => return Err(reason),
            }
        }
        self.ever_heard |= messages.len() > before;
        client
            .on_tick(now, &mut self.outgoing)
            .map_err(|error| error.to_string())?;
        for bytes in self.outgoing.drain(..) {
            socket.send(&bytes)?;
        }
        if client.state() == ClientState::Live {
            if live_since.is_none() {
                *live_since = Some(now);
                self.ever_live = true;
                self.down_since = None;
            }
            if now.saturating_sub(*heard) > self.silence {
                return Err("no message for a minute".into());
            }
        }
        Ok(())
    }

    /// Where the session is.
    pub fn status(&self) -> SessionStatus {
        match &self.phase {
            Phase::Open {
                live_since: Some(_),
                ..
            } => SessionStatus::Live,
            Phase::Open { .. } if !self.ever_live && self.backoff.attempt() == 0 => {
                SessionStatus::Connecting
            }
            Phase::Open { since, .. } => SessionStatus::Reconnecting {
                attempt: self.backoff.attempt(),
                in_s: 0,
                reason: format!(
                    "connecting for {} s",
                    self.last_now.saturating_sub(*since).as_secs()
                )
                .into(),
            },
            Phase::Waiting { reason, .. } if reason.is_empty() => SessionStatus::Connecting,
            Phase::Waiting { until, reason } => SessionStatus::Reconnecting {
                attempt: self.backoff.attempt(),
                in_s: until.saturating_sub(self.last_now).as_secs_f64().ceil() as u32,
                reason: reason.clone(),
            },
        }
    }

    /// Is no connection open, after one was lost or refused?
    pub fn is_waiting(&self) -> bool {
        matches!(&self.phase, Phase::Waiting { reason, .. } if !reason.is_empty())
    }

    /// Closes the connection politely, and opens no other.
    pub fn close(&mut self, now: Duration) {
        if let Phase::Open { socket, client, .. } = &mut self.phase {
            client.disconnect(now, &mut self.outgoing);
            for bytes in self.outgoing.drain(..) {
                let _ = socket.send(&bytes);
            }
        }
        self.phase = Phase::Waiting {
            until: Duration::MAX,
            reason: "closed".into(),
        };
    }
}

/// A fake server for tests: its sockets' events and what the client sent.
#[cfg(test)]
pub mod fake {
    use std::sync::{Arc, Mutex};

    use super::super::client::encode_remaining_length;
    use super::*;

    #[derive(Default)]
    pub struct Server {
        /// Whether a connection is accepted.
        pub accept: bool,
        pub pending: Vec<SocketEvent>,
        pub sent: Vec<Vec<u8>>,
        pub opened: Vec<String>,
    }

    pub type Shared = Arc<Mutex<Server>>;

    pub struct FakeOpener(pub Shared);
    pub struct FakeSocket(pub Shared);

    impl Opener for FakeOpener {
        fn open(&mut self, url: &str, protocol: &str) -> Result<Box<dyn Socket>, String> {
            let mut server = self.0.lock().expect("server");
            server.opened.push(format!("{url} {protocol}"));
            if !server.accept {
                return Err("refused".into());
            }
            server.pending.push(SocketEvent::Open);
            Ok(Box::new(FakeSocket(self.0.clone())))
        }
    }

    impl Socket for FakeSocket {
        fn poll(&mut self, out: &mut Vec<SocketEvent>) {
            out.append(&mut self.0.lock().expect("server").pending);
        }

        fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.0.lock().expect("server").sent.push(bytes.to_vec());
            Ok(())
        }
    }

    /// A PUBLISH packet of `payload` on `topic`, QoS 0.
    pub fn publish(topic: &str, payload: &str) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
        body.extend_from_slice(topic.as_bytes());
        body.extend_from_slice(payload.as_bytes());
        let mut out = vec![0x30];
        encode_remaining_length(body.len(), &mut out);
        out.extend(body);
        out
    }

    /// A CONNACK, and a SUBACK of the first subscription.
    pub const CONNACK: [u8; 4] = [0x20, 2, 0, 0];
    pub const SUBACK: [u8; 5] = [0x90, 3, 0, 1, 0];
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    fn session() -> Session {
        Session::new("test", "wss://example.org/mqtt", "mqtt", ["a/#"], "id", 7)
    }

    #[test]
    fn test_session_connects_subscribes_and_delivers() {
        let server: Shared = Shared::default();
        server.lock().expect("server").accept = true;
        let mut opener = FakeOpener(server.clone());
        let mut session = session();
        let mut messages = Vec::new();
        assert!(session.is_due(S(0)));
        session.poll(S(0), &mut opener, &mut messages);
        assert_eq!(session.status(), SessionStatus::Connecting);
        server
            .lock()
            .expect("server")
            .pending
            .push(SocketEvent::Data(CONNACK.to_vec()));
        session.poll(S(1), &mut opener, &mut messages);
        let mut bytes = SUBACK.to_vec();
        bytes.extend(publish("a/b", "{}"));
        server
            .lock()
            .expect("server")
            .pending
            .push(SocketEvent::Data(bytes));
        assert!(!session.has_heard());
        session.poll(S(2), &mut opener, &mut messages);
        assert_eq!(session.status(), SessionStatus::Live);
        assert!(session.has_heard());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].topic, "a/b");
        assert!(!session.is_out_of_reach(S(500), 3, S(90)));
        session.close(S(3));
        assert_eq!(
            server.lock().expect("server").sent.last(),
            Some(&vec![0xe0, 0])
        );
        assert!(!session.is_due(S(10_000)));
    }

    #[test]
    fn test_refused_connections_are_out_of_reach_after_three() {
        let server: Shared = Shared::default();
        let mut opener = FakeOpener(server.clone());
        let mut session = session();
        let mut messages = Vec::new();
        let mut reached = None;
        for second in 0..200 {
            session.poll(S(second), &mut opener, &mut messages);
            if reached.is_none() && session.is_out_of_reach(S(second), 3, S(600)) {
                reached = Some(second);
            }
        }
        // Refused at 0, about 16 and about 48 s (jitter stretches each wait
        // by a quarter at most): out of reach at the third.
        assert!(reached.is_some_and(|second| (48..=60).contains(&second)));
        assert_eq!(server.lock().expect("server").opened.len(), 4);
        assert!(session.is_waiting());
        assert!(matches!(
            session.status(),
            SessionStatus::Reconnecting { attempt, .. } if attempt >= 3
        ));
    }

    #[test]
    fn test_a_silent_broker_is_out_of_reach_in_time() {
        let server: Shared = Shared::default();
        server.lock().expect("server").accept = true;
        let mut opener = FakeOpener(server.clone());
        let mut session = session();
        let mut messages = Vec::new();
        session.poll(S(0), &mut opener, &mut messages);
        let mut bytes = CONNACK.to_vec();
        bytes.extend(SUBACK);
        server
            .lock()
            .expect("server")
            .pending
            .push(SocketEvent::Data(bytes));
        session.poll(S(1), &mut opener, &mut messages);
        assert_eq!(session.status(), SessionStatus::Live);
        assert!(!session.is_out_of_reach(S(89), 3, S(90)));
        assert!(session.is_out_of_reach(S(90), 3, S(90)));
    }
}
