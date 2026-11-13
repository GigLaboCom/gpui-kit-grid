//! Helsinki live: HSL's high-frequency positioning (HFP), every vehicle's
//! position once a second over MQTT on a WebSocket (`wss://mqtt.hsl.fi`),
//! no key. Data © HSL (Helsinki Region Transport), CC BY 4.0. [`HslSource`]
//! is the second [`VehicleSource`]: one row a vehicle, keyed by its
//! operator and number (`oper/veh`), with its line, destination, next
//! stop, delay, speed and occupancy; the names of the lines and stops come
//! from HSL's network as `script/fetch-transit-data --helsinki` compiles
//! it, the vehicles never from it. No GPUI types.
//!
//! Gentle with HSL: one connection, through the session the examples share
//! (`../mqtt/session.rs`): a lost one comes back after 16 s, doubling to
//! five minutes, with jitter, never two starts within 16 s. Only the
//! positions (`vp`) of vehicles in service, of the trams, the metro, the
//! commuter trains and the ferries; the buses, most of the feed, only as
//! an option. If the broker stays out of reach (nothing heard after three
//! connections, or 90 s), a recording replays, with a notice
//! ([`HfpRecording`]: natively `TRANSIT_RECORD` writes one).

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use web_time::{SystemTime, UNIX_EPOCH};

use super::{
    mqtt::{
        client::Publish,
        session::{Session, SessionStatus},
        socket::{MaybeSend, Opener},
    },
    schedule,
    source::{
        FeedStatus, GeoBounds, GeoPoint, Line, LiveState, Mode, Network, Occupancy, SourceClock,
        SourceInfo, Vehicle, VehicleKey, VehicleSource,
    },
    timetable::Timetable,
    zone::Zone,
};

/// HSL's MQTT broker over WebSocket with TLS (documented as
/// `wss://mqtt.hsl.fi:443/`; the default port left out, so the `Host`
/// header is the bare host, as a browser sends it).
pub const MQTT_URL: &str = "wss://mqtt.hsl.fi/";
/// The WebSocket subprotocol MQTT asks for.
pub const MQTT_PROTOCOL: &str = "mqtt";
/// Who the example says it is: the start of its MQTT client id. No
/// personal data.
pub const USER: &str = "gpui-kit-grid/transit";
/// The positions of the vehicles in service on rails and water.
pub const TOPICS: [&str; 4] = [
    "/hfp/v2/journey/ongoing/vp/tram/#",
    "/hfp/v2/journey/ongoing/vp/metro/#",
    "/hfp/v2/journey/ongoing/vp/train/#",
    "/hfp/v2/journey/ongoing/vp/ferry/#",
];
/// The buses' positions, an option: most of the feed.
pub const BUS_TOPIC: &str = "/hfp/v2/journey/ongoing/vp/bus/#";
/// A vehicle not heard for this long has left service.
pub const STALE: Duration = Duration::from_secs(60);
/// The broker is out of reach, and the recording replays, when nothing has
/// been heard after this many failed connections, or this long after the
/// first one started.
pub const ATTEMPTS: u32 = 3;
pub const SILENT: Duration = Duration::from_secs(90);
/// The first line of a recording: what it is and whose data.
pub const RECORDING_HEADER: &str =
    "# gpui-kit-grid transit: HSL high-frequency positioning, (c) HSL, CC BY 4.0";
/// Seconds over which the status bar's rate is counted.
const RATE_SECONDS: u64 = 5;

/// The topics of the vehicles asked for.
pub fn topics(buses: bool) -> Vec<&'static str> {
    TOPICS
        .into_iter()
        .chain(buses.then_some(BUS_TOPIC))
        .collect()
}

/// Unix ms of the wall clock.
pub fn wall_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |since| since.as_secs_f64() * 1e3)
}

/// One vehicle's position message, from its topic and payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Hfp {
    pub mode: Mode,
    pub key: VehicleKey,
    /// GTFS `route_id`, as the topic names it.
    pub route: Arc<str>,
    /// The line's number as passengers see it (`desi`).
    pub desi: Arc<str>,
    pub headsign: Arc<str>,
    /// GTFS `stop_id` of the next stop; `EOL` past the last one.
    pub next_stop: Arc<str>,
    pub at: Option<GeoPoint>,
    /// Metres a second.
    pub speed: Option<f32>,
    /// HFP's `dl`: seconds ahead of the timetable, negative behind it.
    pub dl: Option<i32>,
    pub occupancy: Option<u8>,
}

/// A vehicle's mode from the topic's `transport_mode`.
fn mode_of(transport: &str) -> Option<Mode> {
    match transport {
        "tram" => Some(Mode::Tram),
        "metro" => Some(Mode::Metro),
        "train" => Some(Mode::Train),
        "ferry" => Some(Mode::Ferry),
        "bus" | "ubus" | "robot" => Some(Mode::Bus),
        _ => None,
    }
}

/// Parses an HFP `vp` message: the topic
/// `/hfp/v2/journey/ongoing/vp/<mode>/<operator>/<vehicle>/<route>/<direction>/<headsign>/<start>/<next stop>/...`
/// and the payload `{"VP":{...}}`.
pub fn parse_hfp(topic: &str, payload: &[u8]) -> Result<Hfp, String> {
    let levels: Vec<&str> = topic.split('/').collect();
    if levels.len() < 14 || levels[1] != "hfp" || levels[5] != "vp" {
        return Err(format!("not a vehicle position: {topic}"));
    }
    let mode = mode_of(levels[6]).ok_or_else(|| format!("a mode {:?}", levels[6]))?;
    let oper: u32 = levels[7].parse().map_err(|_| "an operator")?;
    let veh: u32 = levels[8].parse().map_err(|_| "a vehicle number")?;
    let json: serde_json::Value =
        serde_json::from_slice(payload).map_err(|error| format!("JSON: {error}"))?;
    let vp = json.get("VP").ok_or("no VP")?;
    let float = |name: &str| vp.get(name).and_then(serde_json::Value::as_f64);
    let at = match (float("lat"), float("long")) {
        (Some(lat), Some(lon)) if lat != 0. && lon != 0. => Some(GeoPoint::new(lat, lon)),
        _ => None,
    };
    let desi = vp
        .get("desi")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(levels[9]);
    Ok(Hfp {
        mode,
        key: VehicleKey::new((u64::from(oper) << 32) | u64::from(veh)),
        route: levels[9].into(),
        desi: desi.into(),
        headsign: levels[11].into(),
        next_stop: levels[13].into(),
        at,
        speed: float("spd").map(|speed| speed as f32),
        dl: vp
            .get("dl")
            .and_then(serde_json::Value::as_i64)
            .map(|dl| dl as i32),
        occupancy: vp
            .get("occu")
            .and_then(serde_json::Value::as_u64)
            .map(|occu| occu.min(100) as u8),
    })
}

/// A recording: messages with the time each arrived, relative to the
/// recording's start, one a line:
///
/// ```text
/// # gpui-kit-grid transit: HSL high-frequency positioning, (c) HSL, CC BY 4.0
/// # start 1794052605552
/// 41	/hfp/v2/journey/ongoing/vp/tram/0040/00412/1004/1/Pasila/12:31/1180437/4/...	{"VP":{...}}
/// ```
#[derive(Clone, Debug, Default)]
pub struct HfpRecording {
    start_ms: f64,
    events: Vec<(f64, Arc<str>, Arc<[u8]>)>,
}

impl HfpRecording {
    /// Parses a recording; a line that is not offset, topic and payload is
    /// an error.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "a recording is UTF-8 text")?;
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
            events.push((offset, Arc::from(topic), Arc::from(payload.as_bytes())));
        }
        let start_ms = start_ms.ok_or("a recording starts with `# start <Unix ms>`")?;
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(Self { start_ms, events })
    }

    pub fn start_ms(&self) -> f64 {
        self.start_ms
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// From its start to its last message, ms.
    pub fn duration_ms(&self) -> f64 {
        self.events.last().map_or(0., |(offset, ..)| offset.max(0.))
    }

    /// The lines that start a recording made at `start_ms`.
    pub fn header(start_ms: f64) -> String {
        format!("{RECORDING_HEADER}\n# start {start_ms:.0}\n")
    }

    /// One message's line, `offset_ms` after the start.
    pub fn line(offset_ms: f64, topic: &str, payload: &[u8]) -> String {
        format!(
            "{offset_ms:.0}\t{topic}\t{}\n",
            String::from_utf8_lossy(payload).replace(['\n', '\r'], " ")
        )
    }
}

/// The pause between the end of a recording and its next loop.
const LOOP_GAP_MS: f64 = 1_000.;

/// A recording played back at `speed`, over and over.
struct Replay {
    recording: Arc<HfpRecording>,
    speed: f64,
    cursor: usize,
    loops: u32,
    origin: Option<Duration>,
}

impl Replay {
    fn new(recording: Arc<HfpRecording>, speed: f64) -> Self {
        Self {
            recording,
            speed: speed.clamp(0.1, 100.),
            cursor: 0,
            loops: 0,
            origin: None,
        }
    }

    fn elapsed_ms(&self, now: Duration) -> f64 {
        let origin = self.origin.unwrap_or(now);
        now.saturating_sub(origin).as_secs_f64() * 1e3 * self.speed
    }

    fn loop_ms(&self) -> f64 {
        self.recording.duration_ms() + LOOP_GAP_MS
    }

    /// The recording's clock at `now`, Unix ms.
    fn clock_ms(&self, now: Duration) -> f64 {
        self.recording.start_ms() + self.elapsed_ms(now)
    }

    /// The messages due at `now`, into `out`.
    fn poll(&mut self, now: Duration, out: &mut Vec<Publish>) {
        self.origin.get_or_insert(now);
        let elapsed = self.elapsed_ms(now);
        loop {
            let shift = f64::from(self.loops) * self.loop_ms();
            let Some((offset, topic, payload)) = self.recording.events.get(self.cursor) else {
                if self.recording.events.is_empty() || elapsed < shift + self.loop_ms() {
                    return;
                }
                self.loops += 1;
                self.cursor = 0;
                continue;
            };
            if offset + shift > elapsed {
                return;
            }
            out.push(Publish {
                topic: topic.to_string(),
                payload: payload.to_vec(),
                qos: 0,
                packet_id: None,
                retain: false,
            });
            self.cursor += 1;
        }
    }
}

/// The recording to fall back to when HSL cannot be reached: natively the
/// default recording file, read at once; in the browser the page's, which
/// the page fetches only when asked.
pub trait RecordingSupply: MaybeSend {
    /// The recording, or why there is none; `None` while it is on its way
    /// (asked again on the next poll).
    fn recording(&mut self) -> Option<Result<HfpRecording, String>>;
}

/// Where the messages go besides the vehicles: natively a recording file.
pub trait MessageSink: MaybeSend {
    fn record(&mut self, clock_ms: f64, topic: &str, payload: &[u8]);
}

/// Where the positions come from.
pub enum HslFeed {
    /// The broker over `opener`; `fallback` replays a recording if it
    /// stays out of reach.
    Live {
        opener: Box<dyn Opener>,
        fallback: Option<Box<dyn RecordingSupply>>,
    },
    /// A recording at `speed`.
    Replay {
        recording: Arc<HfpRecording>,
        speed: f64,
    },
}

enum Feed {
    Live {
        session: Box<Session>,
        opener: Box<dyn Opener>,
        fallback: Option<Box<dyn RecordingSupply>>,
    },
    Replay(Replay),
}

/// Helsinki's vehicles, live, as a [`VehicleSource`].
pub struct HslSource {
    /// The compiled network's bytes until `load`.
    network: Option<Vec<u8>>,
    feed: Feed,
    sink: Option<Box<dyn MessageSink>>,
    /// The lines by GTFS `route_id`: the network's, and those it lacks as
    /// the feed names them, ranked after.
    lines: HashMap<Arc<str>, Arc<Line>>,
    next_rank: u32,
    /// The stops' names by GTFS `stop_id`.
    stops: HashMap<Arc<str>, Arc<str>>,
    vehicles: HashMap<VehicleKey, (Hfp, f64)>,
    /// The wall clock at the producer's time zero.
    origin_ms: f64,
    last_now: Duration,
    /// Messages a second, by whole second of the producer's time.
    rate: VecDeque<(u64, u32)>,
    messages: u64,
    malformed: u64,
    notice: Option<Arc<str>>,
    /// Are the buses asked for? A recording may hold them anyway.
    buses: bool,
    publishes: Vec<Publish>,
}

impl HslSource {
    /// The vehicles of `feed`, named after `network` (the compiled
    /// `hsl-network.tsv`, or none: then the ids stand in), the buses too if
    /// `buses`. `seed` draws the client id and the jitter.
    pub fn new(network: Option<Vec<u8>>, feed: HslFeed, buses: bool, seed: u64) -> Self {
        let mut rng = fastrand::Rng::with_seed(seed);
        let client_id = format!("{USER}; {:016x}", rng.u64(..));
        let feed = match feed {
            HslFeed::Live { opener, fallback } => Feed::Live {
                session: Box::new(Session::new(
                    "transit",
                    MQTT_URL,
                    MQTT_PROTOCOL,
                    topics(buses),
                    client_id,
                    rng.u64(..),
                )),
                opener,
                fallback,
            },
            HslFeed::Replay { recording, speed } => Feed::Replay(Replay::new(recording, speed)),
        };
        Self {
            network,
            feed,
            sink: None,
            lines: HashMap::new(),
            next_rank: 0,
            stops: HashMap::new(),
            vehicles: HashMap::new(),
            origin_ms: wall_ms(),
            last_now: Duration::ZERO,
            rate: VecDeque::new(),
            messages: 0,
            malformed: 0,
            notice: None,
            buses,
            publishes: Vec::new(),
        }
    }

    /// Every message also goes to `sink` (natively, a recording file).
    pub fn with_sink(mut self, sink: Option<Box<dyn MessageSink>>) -> Self {
        self.sink = sink;
        self
    }

    /// The same with its clock at `origin_ms` at the producer's time zero.
    #[cfg(test)]
    pub fn with_origin(mut self, origin_ms: f64) -> Self {
        self.origin_ms = origin_ms;
        self
    }

    /// The feed's clock at `now`, Unix ms: the wall clock live, the
    /// recording's in a replay.
    fn clock_ms(&self, now: Duration) -> f64 {
        match &self.feed {
            Feed::Live { .. } => self.origin_ms + now.as_secs_f64() * 1e3,
            Feed::Replay(replay) => replay.clock_ms(now),
        }
    }

    /// The line of `hfp`: the network's by route, else one made from the
    /// message and kept.
    fn line(&mut self, hfp: &Hfp) -> Arc<Line> {
        if let Some(line) = self.lines.get(&hfp.route) {
            return line.clone();
        }
        let line = Arc::new(Line {
            mode: hfp.mode,
            name: hfp.desi.clone(),
            color: None,
            text_color: None,
            rank: self.next_rank,
        });
        self.next_rank += 1;
        self.lines.insert(hfp.route.clone(), line.clone());
        line
    }

    /// The name of the next stop.
    fn stop_name(&self, id: &Arc<str>) -> Arc<str> {
        match id.as_ref() {
            "EOL" => "terminus".into(),
            "" => "leaving the HSL area".into(),
            _ => self.stops.get(id).cloned().unwrap_or_else(|| id.clone()),
        }
    }

    fn take(&mut self, now: Duration, clock_ms: f64) {
        let second = now.as_secs();
        for publish in std::mem::take(&mut self.publishes) {
            if let Some(sink) = self.sink.as_mut() {
                sink.record(clock_ms, &publish.topic, &publish.payload);
            }
            match parse_hfp(&publish.topic, &publish.payload) {
                // A recording's buses, when they are not asked for.
                Ok(hfp) if hfp.mode == Mode::Bus && !self.buses => {}
                Ok(hfp) => {
                    self.messages += 1;
                    match self.rate.back_mut() {
                        Some((at, count)) if *at == second => *count += 1,
                        _ => self.rate.push_back((second, 1)),
                    }
                    self.vehicles.insert(hfp.key, (hfp, clock_ms));
                }
                Err(error) => {
                    self.malformed += 1;
                    if self.malformed <= 3 {
                        err!("transit: HFP {error}");
                    }
                }
            }
        }
        while self
            .rate
            .front()
            .is_some_and(|(at, _)| *at + RATE_SECONDS + 1 < second)
        {
            self.rate.pop_front();
        }
    }

    /// Swaps a broker out of reach for the recording, once it is there.
    fn fall_back(&mut self, now: Duration) {
        let Feed::Live {
            session, fallback, ..
        } = &mut self.feed
        else {
            return;
        };
        if !session.is_out_of_reach(now, ATTEMPTS, SILENT) {
            return;
        }
        let Some(recording) = fallback.as_mut().and_then(|fallback| fallback.recording()) else {
            return;
        };
        *fallback = None;
        let reason = match session.status() {
            SessionStatus::Reconnecting { reason, .. } => reason,
            _ => format!("no message in {} s", SILENT.as_secs()).into(),
        };
        match recording {
            Ok(recording) => {
                err!("transit: HSL unreachable ({reason}); replaying the recording");
                session.close(now);
                self.notice =
                    Some(format!("HSL unreachable ({reason}): a recording replays").into());
                self.vehicles.clear();
                self.feed = Feed::Replay(Replay::new(Arc::new(recording), 1.));
            }
            Err(error) => err!("transit: HSL unreachable, and no recording ({error})"),
        }
    }
}

/// Helsinki when the network is not there: the region the trains reach
/// and the city centre.
fn default_network() -> Network {
    Network {
        region: Some(GeoBounds {
            min: GeoPoint::new(60.10, 24.55),
            max: GeoPoint::new(60.40, 25.25),
        }),
        city: Some(GeoBounds {
            min: GeoPoint::new(60.15, 24.88),
            max: GeoPoint::new(60.22, 25.10),
        }),
        ..Network::default()
    }
}

impl VehicleSource for HslSource {
    fn load(&mut self) -> Result<SourceInfo, String> {
        let year = Zone::Helsinki.year(wall_ms());
        let parsed = self.network.take().map(|bytes| {
            std::str::from_utf8(&bytes)
                .map_err(|_| "not UTF-8".to_string())
                .and_then(Timetable::parse)
        });
        let (network, version) = match parsed {
            Some(Ok(timetable)) => {
                for (line, id) in timetable.lines.iter().zip(&timetable.line_ids) {
                    if !id.is_empty() {
                        self.lines.insert(id.clone(), line.clone());
                    }
                }
                self.next_rank = timetable.lines.len() as u32;
                for (station, id) in timetable.stations.iter().zip(&timetable.station_ids) {
                    if !id.is_empty() {
                        self.stops.insert(id.clone(), station.name.clone());
                    }
                }
                let version = format!(
                    "network: GTFS {}, retrieved {}",
                    timetable.feed.version, timetable.feed.retrieved
                );
                (schedule::network(&timetable), version)
            }
            Some(Err(error)) => {
                err!("transit: no Helsinki network ({error}); stop ids stand for names");
                (default_network(), "no network".to_string())
            }
            None => (default_network(), "no network".to_string()),
        };
        let replay = matches!(self.feed, Feed::Replay(_));
        let kind = if replay { "a recording" } else { "live" };
        Ok(SourceInfo {
            network: Arc::new(network),
            attribution: format!(
                "Vehicle positions © HSL {year}, CC BY 4.0: high-frequency positioning, {kind}. {}.",
                capitalize(&version)
            )
            .into(),
            short: format!("© HSL {year}, CC BY 4.0, {kind}").into(),
            credit: format!(
                "© HSL {year} (Helsinki Region Transport), CC BY 4.0 · vehicle positions (HFP), {kind} · {version}"
            )
            .into(),
            clock: SourceClock::LiveOnly,
            clock_start_ms: match &self.feed {
                Feed::Replay(replay) => Some(replay.recording.start_ms()),
                Feed::Live { .. } => None,
            },
            zone: Zone::Helsinki,
            has_board: false,
        })
    }

    fn poll(&mut self, now: Duration) {
        self.last_now = now;
        let clock_ms = self.clock_ms(now);
        let mut publishes = std::mem::take(&mut self.publishes);
        match &mut self.feed {
            Feed::Live {
                session, opener, ..
            } => session.poll(now, &mut **opener, &mut publishes),
            Feed::Replay(replay) => replay.poll(now, &mut publishes),
        }
        self.publishes = publishes;
        self.take(now, clock_ms);
        self.fall_back(now);
    }

    fn vehicles(&mut self, _: f64, out: &mut Vec<Vehicle>) {
        let now_ms = self.clock_ms(self.last_now);
        let stale_ms = STALE.as_secs_f64() * 1e3;
        self.vehicles
            .retain(|_, (_, seen_ms)| now_ms - *seen_ms < stale_ms);
        let heard: Vec<Hfp> = self
            .vehicles
            .values()
            .filter(|(hfp, _)| hfp.at.is_some())
            .map(|(hfp, _)| hfp.clone())
            .collect();
        for hfp in heard {
            let Some(at) = hfp.at else {
                continue;
            };
            let line = self.line(&hfp);
            out.push(Vehicle {
                key: hfp.key,
                line,
                headsign: hfp.headsign.clone(),
                leg: self.stop_name(&hfp.next_stop),
                progress: 0.,
                next_arrival_ms: now_ms,
                stops_left: 0,
                at,
                path: Arc::from([at]),
                leg_ix: 0,
                live: Some(LiveState {
                    delay_s: hfp.dl.map(|dl| -dl),
                    speed_kmh: hfp.speed.map(|speed| speed * 3.6),
                    occupancy: hfp.occupancy.map(Occupancy),
                }),
            });
        }
    }

    fn trips_on_day(&mut self, _: f64) -> Option<u32> {
        None
    }

    fn status(&self) -> Option<FeedStatus> {
        let (connection, is_live): (Arc<str>, bool) = match &self.feed {
            Feed::Live { session, .. } => match session.status() {
                SessionStatus::Connecting => ("connecting to HSL".into(), false),
                SessionStatus::Live => ("live".into(), true),
                SessionStatus::Reconnecting { in_s, reason, .. } if in_s > 0 => {
                    (format!("reconnecting in {in_s} s ({reason})").into(), false)
                }
                SessionStatus::Reconnecting { reason, .. } => {
                    (format!("reconnecting ({reason})").into(), false)
                }
            },
            Feed::Replay(replay) => (format!("replay, loop {}", replay.loops + 1).into(), false),
        };
        let second = self.last_now.as_secs();
        let counted: u32 = self
            .rate
            .iter()
            .filter(|(at, _)| *at < second && *at + RATE_SECONDS >= second)
            .map(|(_, count)| *count)
            .sum();
        Some(FeedStatus {
            connection,
            is_live,
            rate: counted as f32 / RATE_SECONDS as f32,
            messages: self.messages,
            notice: self.notice.clone(),
        })
    }

    fn close(&mut self) {
        if let Feed::Live { session, .. } = &mut self.feed {
            session.close(self.last_now);
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A network for the tests, in the compiled format: tram 4 over three
/// stops with HSL's ids.
#[cfg(test)]
pub const NETWORK: &str = "transit-timetable\t2
source\tHelsinki Region Transport (HSL)\tCC BY 4.0\thttps://example.org/hsl.zip\t2026-11-07 06:41:30\t2026-11-07\t20261106\t20270104
zone\tEurope/Helsinki
L\tM\tM1\t\t\t31M1
L\tT\t4\t\t\t1004
S\tKatajanokka\t6016700\t2496700\t1080416
S\tRautatientori\t6017118\t2494507\t1020456
S\tMunkkiniemi\t6019800\t2487700\t1304143
P\t1\tMunkkiniemi\t0,1,2\t240,360\t
";

/// A position of tram 4, `oper/veh` 40/412, before Rautatientori.
#[cfg(test)]
pub const TOPIC: &str =
    "/hfp/v2/journey/ongoing/vp/tram/0040/00412/1004/1/Munkkiniemi/12:31/1020456/4/60;24/19/73/35";
#[cfg(test)]
pub const PAYLOAD: &str = r#"{"VP":{"desi":"4","dir":"1","oper":40,"veh":412,"tst":"2026-11-07T10:40:12.123Z","tsi":1794048012,"spd":5.2,"hdg":270,"lat":60.17092,"long":24.94381,"acc":0.1,"dl":-95,"odo":1234,"drst":0,"oday":"2026-11-07","jrn":1,"line":1,"start":"12:31","loc":"GPS","stop":null,"route":"1004","occu":0}}"#;

#[cfg(test)]
mod tests {
    use super::super::mqtt::{
        session::fake::{self, FakeOpener},
        socket::SocketEvent,
    };
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn test_a_position_parses_from_topic_and_payload() {
        let hfp = parse_hfp(TOPIC, PAYLOAD.as_bytes()).expect("a position");
        assert_eq!(hfp.mode, Mode::Tram);
        assert_eq!(hfp.key, VehicleKey::new((40 << 32) | 412));
        assert_eq!(hfp.route.as_ref(), "1004");
        assert_eq!(hfp.desi.as_ref(), "4");
        assert_eq!(hfp.headsign.as_ref(), "Munkkiniemi");
        assert_eq!(hfp.next_stop.as_ref(), "1020456");
        assert_eq!(hfp.dl, Some(-95));
        assert_eq!(hfp.occupancy, Some(0));
        assert!((hfp.speed.expect("a speed") - 5.2).abs() < 1e-6);
        assert!((hfp.at.expect("a place").lat - 60.17092).abs() < 1e-9);
        // No position, a deadrun, and what is not JSON.
        let nowhere = PAYLOAD.replace(
            r#""lat":60.17092,"long":24.94381"#,
            r#""lat":null,"long":null"#,
        );
        assert_eq!(
            parse_hfp(TOPIC, nowhere.as_bytes()).expect("parses").at,
            None
        );
        assert!(parse_hfp("/hfp/v2/deadrun/ongoing/dep/bus", b"{}").is_err());
        assert!(parse_hfp(TOPIC, b"not json").is_err());
        assert_eq!(topics(false).len(), 4);
        assert!(topics(true).contains(&BUS_TOPIC));
    }

    #[test]
    fn test_a_recording_round_trips_and_replays_in_time() {
        let text = format!(
            "{}{}{}",
            HfpRecording::header(1_000_000.),
            HfpRecording::line(0., TOPIC, PAYLOAD.as_bytes()),
            HfpRecording::line(2_000., TOPIC, PAYLOAD.as_bytes()),
        );
        let recording = Arc::new(HfpRecording::parse(text.as_bytes()).expect("a recording"));
        assert_eq!(recording.len(), 2);
        assert_eq!(recording.duration_ms(), 2_000.);
        assert!(HfpRecording::parse(b"0\tno payload\n").is_err());
        let mut replay = Replay::new(recording, 1.);
        let mut out = Vec::new();
        replay.poll(S(10), &mut out);
        assert_eq!(out.len(), 1);
        replay.poll(S(12), &mut out);
        assert_eq!(out.len(), 2);
        // The next loop, a second after the end.
        replay.poll(S(13), &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(replay.clock_ms(S(13)), 1_003_000.);
    }

    fn live(server: &fake::Shared) -> HslSource {
        HslSource::new(
            Some(NETWORK.as_bytes().to_vec()),
            HslFeed::Live {
                opener: Box::new(FakeOpener(server.clone())),
                fallback: None,
            },
            false,
            7,
        )
        .with_origin(1_000_000.)
    }

    #[test]
    fn test_live_vehicles_are_named_after_the_network_and_leave_when_quiet() {
        let server = fake::Shared::default();
        server.lock().expect("server").accept = true;
        let mut source = live(&server);
        let info = source.load().expect("loads");
        assert_eq!(info.zone, Zone::Helsinki);
        assert_eq!(info.clock, SourceClock::LiveOnly);
        assert!(!info.has_board);
        assert!(info.attribution.contains("© HSL"));
        assert_eq!(info.network.stations.len(), 3);
        source.poll(S(0));
        {
            let server = server.lock().expect("server");
            assert_eq!(server.opened, [format!("{MQTT_URL} mqtt")]);
        }
        let mut bytes = fake::CONNACK.to_vec();
        bytes.extend(fake::SUBACK);
        bytes.extend(fake::publish(TOPIC, PAYLOAD));
        let unknown = TOPIC
            .replace("/1004/", "/1099/")
            .replace("/00412/", "/00413/");
        bytes.extend(fake::publish(&unknown, PAYLOAD));
        server
            .lock()
            .expect("server")
            .pending
            .push(SocketEvent::Data(bytes));
        source.poll(S(1));
        let mut vehicles = Vec::new();
        source.vehicles(0., &mut vehicles);
        vehicles.sort_by_key(|vehicle| vehicle.key);
        assert_eq!(vehicles.len(), 2);
        let tram = &vehicles[0];
        assert_eq!(tram.line.name.as_ref(), "4");
        assert_eq!(tram.line.rank, 1, "the network's own line");
        assert_eq!(tram.leg.as_ref(), "Rautatientori");
        let state = tram.live.expect("live");
        assert_eq!(state.delay_s, Some(95), "95 s behind");
        assert!((state.speed_kmh.expect("a speed") - 18.72).abs() < 1e-3);
        // A route the network lacks: named as the feed names it, ranked
        // after the network's lines.
        assert_eq!(vehicles[1].line.rank, 2);
        let status = source.status().expect("a status");
        assert!(status.is_live);
        assert_eq!(status.messages, 2);
        // Quiet for a minute: gone.
        source.poll(S(62));
        vehicles.clear();
        source.vehicles(0., &mut vehicles);
        assert!(vehicles.is_empty());
    }

    struct Ready(Option<HfpRecording>);

    impl RecordingSupply for Ready {
        fn recording(&mut self) -> Option<Result<HfpRecording, String>> {
            self.0.take().map(Ok)
        }
    }

    #[test]
    fn test_an_unreachable_broker_falls_back_to_the_recording() {
        let server = fake::Shared::default();
        let text = format!(
            "{}{}",
            HfpRecording::header(5_000_000.),
            HfpRecording::line(0., TOPIC, PAYLOAD.as_bytes()),
        );
        let recording = HfpRecording::parse(text.as_bytes()).expect("a recording");
        let mut source = HslSource::new(
            None,
            HslFeed::Live {
                opener: Box::new(FakeOpener(server.clone())),
                fallback: Some(Box::new(Ready(Some(recording)))),
            },
            true,
            7,
        );
        source.load().expect("loads without a network");
        for second in 0..70 {
            source.poll(S(second));
        }
        // Three refused connections, then the recording.
        assert_eq!(server.lock().expect("server").opened.len(), 3);
        let status = source.status().expect("a status");
        assert!(status.connection.starts_with("replay"), "{status:?}");
        assert!(status.notice.expect("a notice").contains("HSL unreachable"));
        let mut vehicles = Vec::new();
        source.vehicles(0., &mut vehicles);
        assert_eq!(vehicles.len(), 1);
        // Without the network the stop's id stands for its name.
        assert_eq!(vehicles[0].leg.as_ref(), "1020456");
    }
}
