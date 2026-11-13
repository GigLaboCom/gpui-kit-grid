//! Live AIS from Digitraffic: the latest locations and metadata from its
//! REST API once at the start, then every vessel's messages over MQTT on
//! a WebSocket (`wss://meri.digitraffic.fi:443/mqtt`), with the session
//! and the MQTT client the examples share (`../mqtt/`). The transport is a
//! [`Connector`]: tungstenite natively (`native.rs`), the browser's
//! WebSocket on the page (`crates/grid-web/apps/ships`). This module opens
//! no socket itself.
//!
//! Gentle by design, as Digitraffic asks: one connection at a time; a
//! lost one comes back only after the backoff (16 s, doubling to five
//! minutes, with jitter, never two starts within 16 s); the REST snapshot
//! once at the start, and again only after an outage of ten minutes or
//! more. Every client says who it is: natively with the `Digitraffic-User`
//! header, in the browser (where a WebSocket cannot send headers) with its
//! MQTT client id.

use std::{sync::Arc, time::Duration};

use web_time::{SystemTime, UNIX_EPOCH};

use super::{
    ais::{self, Message},
    mqtt::{
        client::Publish,
        session::{Session, SessionStatus},
        socket::Opener,
    },
    source::{Connection, SourceEvent, VesselSource},
};

/// Digitraffic's marine MQTT broker over WebSocket (documented as
/// `wss://meri.digitraffic.fi:443/mqtt`; the default port left out, so the
/// `Host` header is the bare host, as a browser sends it).
pub const MQTT_URL: &str = "wss://meri.digitraffic.fi/mqtt";
/// The WebSocket subprotocol MQTT asks for.
pub const MQTT_PROTOCOL: &str = "mqtt";
/// Every vessel's location and metadata, and the feed's status.
pub const TOPICS: [&str; 1] = ["vessels-v2/#"];
/// Who the example says it is: the `Digitraffic-User` header natively,
/// the start of its MQTT client id everywhere. No personal data.
pub const USER: &str = "gpui-kit-grid/ships";
/// The REST API's host and paths.
pub const API: &str = "https://meri.digitraffic.fi/api/ais/v1";
/// How far back the snapshot's locations reach: a vessel silent longer
/// than this leaves the table anyway (`fleet.rs`, `StaleRule`).
pub const SNAPSHOT_LOCATIONS: Duration = Duration::from_secs(30 * 60);
/// How far back the snapshot's metadata reaches: a vessel repeats its
/// metadata every six minutes, but Digitraffic stamps a record when it
/// changes.
pub const SNAPSHOT_METADATA: Duration = Duration::from_secs(2 * 24 * 3600);

/// An outage this long fetches the snapshot again on the way back.
const LONG_OUTAGE: Duration = Duration::from_secs(10 * 60);
/// The MQTT feed is out of reach, and the recording replays, when no
/// message has ever arrived after this many failed connections, or this
/// long after the first one started (a broker that takes the subscription
/// and stays silent).
pub const MQTT_ATTEMPTS: u32 = 3;
pub const MQTT_SILENT: Duration = Duration::from_secs(90);

/// The REST snapshot's locations newer than `now_ms - SNAPSHOT_LOCATIONS`.
pub fn locations_url(now_ms: f64) -> String {
    let from = now_ms - SNAPSHOT_LOCATIONS.as_secs_f64() * 1e3;
    format!("{API}/locations?from={from:.0}")
}

/// The REST snapshot's metadata newer than `now_ms - SNAPSHOT_METADATA`.
pub fn vessels_url(now_ms: f64) -> String {
    let from = now_ms - SNAPSHOT_METADATA.as_secs_f64() * 1e3;
    format!("{API}/vessels?from={from:.0}")
}

/// Unix ms of the wall clock.
pub fn wall_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |since| since.as_secs_f64() * 1e3)
}

/// The two REST responses, as fetched.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub locations: Vec<u8>,
    pub vessels: Vec<u8>,
}

impl Snapshot {
    /// Its messages, each at the time Digitraffic stamped it.
    pub fn events(&self) -> Result<Vec<SourceEvent>, String> {
        let locations = ais::parse_locations(&self.locations)?;
        let vessels = ais::parse_vessels(&self.vessels)?;
        let mut events = Vec::with_capacity(locations.len() + vessels.len());
        events.extend(vessels.into_iter().map(|(mmsi, metadata)| SourceEvent {
            at_ms: metadata.time_ms,
            message: Message::Metadata {
                mmsi,
                metadata: Arc::new(metadata),
            },
            snapshot: true,
        }));
        events.extend(locations.into_iter().map(|(mmsi, location)| SourceEvent {
            at_ms: location.time_ms,
            message: Message::Location { mmsi, location },
            snapshot: true,
        }));
        Ok(events)
    }
}

/// The transport: opens WebSockets ([`Opener`]) and fetches the REST
/// snapshot.
pub trait Connector: Opener {
    /// The latest locations and metadata: fetched now (natively), or what
    /// the page fetched before the module started (in the browser, once).
    /// `None` when there is no snapshot to give.
    fn snapshot(&mut self) -> Option<Result<Snapshot, String>>;
}

/// Live AIS from Digitraffic over a [`Connector`].
pub struct LiveSource {
    connector: Box<dyn Connector>,
    session: Session,
    /// The wall clock at the driver's time zero.
    origin_ms: f64,
    snapshot_asked: bool,
    snapshot_error: Option<Arc<str>>,
    /// The driver's time at the last poll.
    last_now: Duration,
    malformed: u64,
    publishes: Vec<Publish>,
}

impl LiveSource {
    /// A source over `connector`; `seed` draws the client id and the
    /// jitter. Its clock is the wall clock now at the driver's time zero.
    pub fn new(connector: Box<dyn Connector>, seed: u64) -> Self {
        let mut rng = fastrand::Rng::with_seed(seed);
        let client_id = format!("{USER}; {:016x}", rng.u64(..));
        Self {
            connector,
            session: Session::new(
                "ships",
                MQTT_URL,
                MQTT_PROTOCOL,
                TOPICS,
                client_id,
                rng.u64(..),
            ),
            origin_ms: wall_ms(),
            snapshot_asked: false,
            snapshot_error: None,
            last_now: Duration::ZERO,
            malformed: 0,
            publishes: Vec::new(),
        }
    }

    /// The same with its clock at `origin_ms` at the driver's time zero.
    #[cfg(test)]
    pub fn with_origin(mut self, origin_ms: f64) -> Self {
        self.origin_ms = origin_ms;
        self
    }

    #[cfg(test)]
    pub fn client_id(&self) -> &str {
        self.session.client_id()
    }

    fn take_snapshot(&mut self, out: &mut Vec<SourceEvent>) {
        let Some(snapshot) = self.connector.snapshot() else {
            return;
        };
        match snapshot.and_then(|snapshot| snapshot.events()) {
            Ok(events) => {
                self.snapshot_error = None;
                out.extend(events);
            }
            Err(error) => {
                err!("ships: no snapshot ({error})");
                self.snapshot_error = Some(error.into());
            }
        }
    }
}

impl VesselSource for LiveSource {
    fn poll(&mut self, now: Duration, out: &mut Vec<SourceEvent>) {
        self.last_now = now;
        if !self.snapshot_asked {
            self.snapshot_asked = true;
            self.take_snapshot(out);
        }
        // Back after a long outage: the vessels that came and went meanwhile.
        if self.session.is_due(now)
            && self
                .session
                .down_for(now)
                .is_some_and(|down| down >= LONG_OUTAGE)
        {
            self.take_snapshot(out);
        }
        let clock_ms = self.clock_ms(now);
        self.session
            .poll(now, &mut *self.connector, &mut self.publishes);
        for publish in self.publishes.drain(..) {
            match ais::parse_message(&publish.topic, &publish.payload) {
                Ok(message) => out.push(SourceEvent {
                    at_ms: clock_ms,
                    message,
                    snapshot: false,
                }),
                Err(error) => {
                    self.malformed += 1;
                    if self.malformed <= 3 {
                        err!("ships: {error}");
                    }
                }
            }
        }
    }

    fn clock_ms(&self, now: Duration) -> f64 {
        self.origin_ms + now.as_secs_f64() * 1e3
    }

    /// Unreachable when the snapshot failed and so did MQTT, or when MQTT
    /// stayed out of reach even with the snapshot ([`MQTT_ATTEMPTS`],
    /// [`MQTT_SILENT`]); the driver then replays the recording.
    fn connection(&self) -> Connection {
        let out_of_reach = self
            .session
            .is_out_of_reach(self.last_now, MQTT_ATTEMPTS, MQTT_SILENT);
        match self.session.status() {
            SessionStatus::Reconnecting { reason, .. } if out_of_reach => Connection::Unreachable {
                reason: format!("MQTT: {reason}").into(),
            },
            SessionStatus::Live | SessionStatus::Connecting if out_of_reach => {
                Connection::Unreachable {
                    reason: format!("MQTT: no message in {} s", MQTT_SILENT.as_secs()).into(),
                }
            }
            SessionStatus::Connecting => Connection::Connecting,
            SessionStatus::Live => Connection::Live,
            SessionStatus::Reconnecting { reason, .. }
                if self.session.is_waiting()
                    && !self.session.is_ever_live()
                    && self.snapshot_error.is_some() =>
            {
                Connection::Unreachable { reason }
            }
            SessionStatus::Reconnecting {
                attempt,
                in_s,
                reason,
            } => Connection::Reconnecting {
                attempt,
                in_s,
                reason,
            },
        }
    }

    fn close(&mut self, now: Duration) {
        self.session.close(now);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc as SyncArc, Mutex};

    use super::super::ais::tests::LOCATION;
    use super::super::mqtt::{
        session::fake::{self, FakeOpener},
        socket::{Socket, SocketEvent},
    };
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    /// The fake server's sockets and the snapshot it gives.
    struct FakeConnector {
        opener: FakeOpener,
        snapshot: SyncArc<Mutex<Option<Result<Snapshot, String>>>>,
    }

    impl Opener for FakeConnector {
        fn open(&mut self, url: &str, protocol: &str) -> Result<Box<dyn Socket>, String> {
            self.opener.open(url, protocol)
        }
    }

    impl Connector for FakeConnector {
        fn snapshot(&mut self) -> Option<Result<Snapshot, String>> {
            self.snapshot.lock().expect("snapshot").take()
        }
    }

    type Shared = fake::Shared;

    fn source_with(server: &Shared, snapshot: Option<Result<Snapshot, String>>) -> LiveSource {
        let connector = FakeConnector {
            opener: FakeOpener(server.clone()),
            snapshot: SyncArc::new(Mutex::new(snapshot)),
        };
        LiveSource::new(Box::new(connector), 7).with_origin(1_000_000.)
    }

    fn source(server: &Shared) -> LiveSource {
        source_with(server, None)
    }

    #[test]
    fn test_live_source_connects_subscribes_and_delivers() {
        let server: Shared = Shared::default();
        server.lock().expect("server").accept = true;
        let mut live = source(&server);
        assert!(live.client_id().starts_with("gpui-kit-grid/ships; "));
        let mut out = Vec::new();
        live.poll(S(0), &mut out);
        {
            let mut server = server.lock().expect("server");
            assert_eq!(server.opened, [format!("{MQTT_URL} mqtt")]);
            assert_eq!(server.sent.len(), 1, "CONNECT");
            assert_eq!(server.sent[0][0], 0x10);
            server.pending.push(SocketEvent::Data(vec![0x20, 2, 0, 0]));
        }
        assert_eq!(live.connection(), Connection::Connecting);
        live.poll(S(1), &mut out);
        {
            let mut server = server.lock().expect("server");
            assert_eq!(server.sent[1][0], 0x82, "SUBSCRIBE");
            // The SUBACK and a message in one WebSocket message.
            let mut bytes = vec![0x90, 3, 0, 1, 0];
            bytes.extend(fake::publish("vessels-v2/230000001/location", LOCATION));
            server.pending.push(SocketEvent::Data(bytes));
        }
        live.poll(S(2), &mut out);
        assert_eq!(live.connection(), Connection::Live);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].at_ms, 1_002_000.);
        assert!(matches!(
            out[0].message,
            Message::Location {
                mmsi: 230000001,
                ..
            }
        ));
        assert!(!out[0].snapshot);
        // Quiet: a ping after three quarters of the keep-alive.
        live.poll(S(25), &mut out);
        assert_eq!(
            server.lock().expect("server").sent.last(),
            Some(&vec![0xc0, 0])
        );
        // Closing says goodbye.
        live.close(S(26));
        assert_eq!(
            server.lock().expect("server").sent.last(),
            Some(&vec![0xe0, 0])
        );
    }

    #[test]
    fn test_live_source_waits_out_the_backoff() {
        let server: Shared = Shared::default();
        let mut live = source_with(&server, Some(Err("offline".into())));
        let mut out = Vec::new();
        let mut attempts = Vec::new();
        for second in 0..400 {
            live.poll(S(second), &mut out);
            let opened = server.lock().expect("server").opened.len();
            if opened > attempts.len() {
                attempts.push(second);
            }
        }
        // 0, then 16, 32, 64, 128 s later (no jitter below the base).
        assert!(attempts.len() >= 4, "{attempts:?}");
        for pair in attempts.windows(2) {
            assert!(pair[1] - pair[0] >= 16, "{attempts:?}");
        }
        assert!(attempts[2] - attempts[1] >= 2 * (attempts[1] - attempts[0]) - 8);
        // The snapshot failed and so did the connection: unreachable.
        assert!(matches!(live.connection(), Connection::Unreachable { .. }));
        assert!(out.is_empty());
    }

    #[test]
    fn test_a_lost_connection_reconnects_no_sooner_than_allowed() {
        let server: Shared = Shared::default();
        server.lock().expect("server").accept = true;
        let mut live = source(&server);
        let mut out = Vec::new();
        live.poll(S(0), &mut out);
        server
            .lock()
            .expect("server")
            .pending
            .push(SocketEvent::Closed("gone".into()));
        live.poll(S(1), &mut out);
        let Connection::Reconnecting { attempt, in_s, .. } = live.connection() else {
            panic!("reconnecting: {:?}", live.connection());
        };
        assert_eq!(attempt, 1);
        assert!((16..=21).contains(&in_s), "{in_s}");
        for second in 2..17 {
            live.poll(S(second), &mut out);
        }
        assert_eq!(server.lock().expect("server").opened.len(), 1);
        live.poll(S(22), &mut out);
        assert_eq!(server.lock().expect("server").opened.len(), 2);
    }

    #[test]
    fn test_snapshot_arrives_first_marked_as_such() {
        let server: Shared = Shared::default();
        let locations = r#"{"features":[{"mmsi":5,"geometry":{"coordinates":[24.9,60.1]},"properties":{"sog":0.0,"cog":0.0,"navStat":5,"heading":90,"timestampExternal":999000}}]}"#;
        let snapshot = Snapshot {
            locations: locations.into(),
            vessels: b"[]".to_vec(),
        };
        let mut live = source_with(&server, Some(Ok(snapshot)));
        let mut out = Vec::new();
        live.poll(S(0), &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].snapshot);
        assert_eq!(out[0].at_ms, 999_000.);
        // Asked once only.
        out.clear();
        live.poll(S(1), &mut out);
        assert!(out.is_empty());
    }

    fn empty_snapshot() -> Option<Result<Snapshot, String>> {
        Some(Ok(Snapshot {
            locations: br#"{"features":[]}"#.to_vec(),
            vessels: b"[]".to_vec(),
        }))
    }

    #[test]
    fn test_mqtt_refused_with_a_snapshot_is_unreachable_after_three_tries() {
        let server: Shared = Shared::default();
        let mut live = source_with(&server, empty_snapshot());
        let mut out = Vec::new();
        let mut unreachable = None;
        for second in 0..120 {
            live.poll(S(second), &mut out);
            if unreachable.is_none()
                && let Connection::Unreachable { reason } = live.connection()
            {
                unreachable = Some((second, reason));
            }
        }
        let (second, reason) = unreachable.expect("unreachable");
        // The third refusal, about 48 s in; the backoff kept between tries.
        assert!((48..=60).contains(&second), "{second}");
        assert_eq!(reason.as_ref(), "MQTT: refused");
        assert_eq!(server.lock().expect("server").opened.len(), 3);
    }

    #[test]
    fn test_a_silent_mqtt_feed_is_unreachable_in_time() {
        let server: Shared = Shared::default();
        server.lock().expect("server").accept = true;
        let mut live = source_with(&server, empty_snapshot());
        let mut out = Vec::new();
        live.poll(S(0), &mut out);
        let mut bytes = fake::CONNACK.to_vec();
        bytes.extend(fake::SUBACK);
        server
            .lock()
            .expect("server")
            .pending
            .push(SocketEvent::Data(bytes));
        live.poll(S(1), &mut out);
        assert_eq!(live.connection(), Connection::Live);
        // Silent a minute: a new connection, the backoff kept.
        live.poll(S(89), &mut out);
        assert!(matches!(
            live.connection(),
            Connection::Reconnecting { attempt: 1, .. }
        ));
        live.poll(S(90), &mut out);
        assert!(matches!(live.connection(), Connection::Unreachable { .. }));
        // A feed that was heard once is never out of reach: it reconnects.
        let server: Shared = Shared::default();
        server.lock().expect("server").accept = true;
        let mut live = source_with(&server, empty_snapshot());
        live.poll(S(0), &mut out);
        let mut bytes = fake::CONNACK.to_vec();
        bytes.extend(fake::SUBACK);
        bytes.extend(fake::publish("vessels-v2/230000001/location", LOCATION));
        server
            .lock()
            .expect("server")
            .pending
            .push(SocketEvent::Data(bytes));
        for second in 1..400 {
            live.poll(S(second), &mut out);
            assert!(!matches!(live.connection(), Connection::Unreachable { .. }));
        }
    }

    #[test]
    fn test_urls_reach_back_as_far_as_promised() {
        assert_eq!(
            locations_url(10_000_000.),
            format!("{API}/locations?from=8200000")
        );
        assert!(vessels_url(1e12).starts_with(&format!("{API}/vessels?from=")));
    }
}
