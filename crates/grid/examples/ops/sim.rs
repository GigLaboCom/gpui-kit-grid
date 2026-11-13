//! The simulated data center operations behind the example, natively on
//! its own thread, in the browser on the executor's timer (`Pump`); no
//! GPUI types. **Everything here is simulated**, and every name
//! (regions, data centers, halls, racks, hosts, services) is generic.
//!
//! - **The fleet:** regions → data centers → halls → racks → hosts
//!   (`FleetShape`), each host with CPU, memory, inlet temperature, power,
//!   network and error rate. Racks and halls are written with their
//!   hosts' roll-ups: the hottest host, the summed power, the hosts in
//!   alert and the worst status. The grid's tree sums nothing on a parent
//!   row (its totals add up the leaves), so the producer writes the
//!   roll-ups, as a monitoring backend would.
//! - **Services:** a fixed set with an SLO each, spread over the racks of
//!   several halls; their p99 latency, error rate and error budget follow
//!   the health of their hosts.
//! - **Alerts:** rules over halls, racks, hosts and services, keyed by
//!   rule and subject (`AlertKey`). A condition that holds fires again
//!   every few hundred milliseconds. The simulator counts each alert's
//!   firings and their rate over the last `RATE_WINDOW_MS`, and every
//!   firing upserts the alert's row with both, so a storm of updates
//!   collapses into one row per key: the grid keeps the latest row and
//!   counts nothing. A condition that clears resolves its alert, which
//!   stays a few seconds and is then removed (the grid fades it out).
//! - **Storm:** power feed B of one hall drops. Its PDUs run on one feed,
//!   cooling on that feed fails, inlet temperatures climb, hosts throttle
//!   and the hottest go down, and the services on them degrade: over a
//!   thousand alert updates a second. After `storm_seconds`, or on the
//!   key, the feed comes back, the hall cools and the alerts resolve.
//!
//! `Sim::step` advances simulated time and leaves its writes in `Out`;
//! `Feeds::write` hands them to the grid stores, so tests step the
//! simulator with a fixed clock and seed.

use std::{
    collections::{VecDeque, hash_map::Entry},
    mem,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering},
        mpsc::Receiver,
    },
    time::Duration,
};

use gpui_kit_grid::{GridWriter, PinnedEdge};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use web_time::Instant;

use super::rows::{
    Alert, AlertKey, AlertState, FleetNode, Level, Rule, ServiceRow, Severity, Subject,
};

/// Hosts in the default run: 3 regions × 2 data centers × 3 halls × 16
/// racks × 20 hosts.
pub const DEFAULT_HOSTS: u32 = 5_760;
/// The regions and the prefix of their data centers.
const REGIONS: [(&str, &str); 3] = [("north", "nth"), ("east", "est"), ("west", "wst")];
const DCS_PER_REGION: u32 = 2;
const HALLS_PER_DC: u32 = 3;
const HOSTS_PER_RACK: u32 = 20;

/// The services: a name, the base p99 in ms and the SLO in %.
const SERVICES: [(&str, f64, f64); 14] = [
    ("edge-proxy", 38., 99.95),
    ("auth", 52., 99.95),
    ("checkout", 140., 99.9),
    ("catalog", 85., 99.9),
    ("search", 120., 99.5),
    ("ledger", 95., 99.99),
    ("media", 180., 99.5),
    ("notify", 60., 99.5),
    ("profile", 70., 99.9),
    ("queue", 25., 99.95),
    ("session", 30., 99.95),
    ("ingest", 110., 99.5),
    ("reports", 240., 99.0),
    ("billing", 130., 99.9),
];
/// The service of power and cooling alerts.
const FACILITIES: &str = "facilities";
/// The data center of a service alert.
const GLOBAL: &str = "global";
/// Services whose rows are pinned on top: the lowest error budgets.
pub const PINNED_SERVICES: usize = 3;

/// The thread's step.
const STEP: Duration = Duration::from_millis(50);
/// The longest step a stall is caught up with.
const MAX_STEP: Duration = Duration::from_millis(200);
/// The simulated wall clock at the start: the night shift.
const START_CLOCK_MS: u64 = (2 * 3_600 + 47 * 60) * 1_000;
/// Simulated time before the first load: the quiet baseline's alerts.
const WARM_UP: Duration = Duration::from_secs(60);
/// How long a resolved alert stays before it is removed.
const RESOLVED_LINGER_MS: u64 = 4_000;
/// The window an alert's rate is counted over: its firings in the last
/// ten seconds, a tenth of a firing a second apart.
const RATE_WINDOW_MS: u64 = 10_000;
/// How often a host's metrics are sampled.
const HOST_TICK_MS: u64 = 1_000;
/// How often the services and the fleet's roll-ups are written.
const SERVICES_EVERY_MS: u64 = 250;
const ROLL_UP_EVERY_MS: u64 = 500;
/// Seconds of alert rates the chart keeps.
pub const HISTORY_SECONDS: usize = 120;
/// Baseline alerts (disk, memory, clock) a second across the fleet.
const TRICKLE_RATE: f64 = 0.7;

/// Cooling left in a hall that lost feed B.
const COOLING_ON_ONE_FEED: f64 = 0.3;
/// Degrees a hall heats up with no cooling at all.
const HEAT_SPAN: f64 = 20.;
/// Inlet temperatures of the alert levels and of throttling, °C.
const TEMP_WARNING: i64 = 32;
const TEMP_MAJOR: i64 = 36;
const TEMP_CRITICAL: i64 = 40;
const THROTTLE_AT: f64 = 35.;
const THROTTLE_CLEAR: f64 = 33.;
/// Error budget drained per second at a burn rate of 1, and refilled per
/// second while it burns slower than that, in %: a minute of storm takes
/// most of a budget, a quiet quarter of an hour brings it back.
const BUDGET_DRAIN: f64 = 0.05;
const BUDGET_REFILL: f64 = 0.08;

/// How the fleet is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetShape {
    pub regions: u32,
    pub dcs: u32,
    pub halls: u32,
    pub racks: u32,
    pub hosts: u32,
}

impl FleetShape {
    /// The default shape with as many racks per hall as come closest to
    /// `hosts` hosts, at least one.
    pub fn for_hosts(hosts: u32) -> Self {
        let regions = REGIONS.len() as u32;
        let per_rack_row = regions * DCS_PER_REGION * HALLS_PER_DC * HOSTS_PER_RACK;
        let racks = (f64::from(hosts) / f64::from(per_rack_row)).round().max(1.) as u32;
        Self {
            regions,
            dcs: DCS_PER_REGION,
            halls: HALLS_PER_DC,
            racks,
            hosts: HOSTS_PER_RACK,
        }
    }

    /// A few dozen hosts, for tests.
    #[cfg(test)]
    pub fn small() -> Self {
        Self {
            regions: 2,
            dcs: 1,
            halls: 2,
            racks: 2,
            hosts: 6,
        }
    }

    pub fn host_count(&self) -> u32 {
        self.regions * self.dcs * self.halls * self.racks * self.hosts
    }
}

/// Where the storm is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Quiet,
    /// Feed B of a hall is down.
    Incident,
    /// The feed is back; the hall cools and the alerts resolve.
    Recovering,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Quiet => "QUIET",
            Self::Incident => "INCIDENT",
            Self::Recovering => "RECOVERING",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Incident,
            2 => Self::Recovering,
            _ => Self::Quiet,
        }
    }
}

/// What the console asks of the simulator.
#[derive(Clone, Debug)]
pub enum Command {
    /// Starts a storm, or ends the one running.
    ToggleStorm,
    /// Acknowledges the firing alerts of these keys.
    Ack(Arc<[AlertKey]>),
}

/// The simulator's counters, for the status bar and the app bar; written
/// by the simulator's thread after every step.
#[derive(Debug, Default)]
pub struct Control {
    updates: AtomicU64,
    keys: AtomicU64,
    active: [AtomicU32; 4],
    phase: AtomicU8,
    storm_hall: AtomicU32,
    storm_ms: AtomicU64,
    clock_ms: AtomicU64,
    hosts_down: AtomicU32,
}

impl Control {
    pub fn new() -> Arc<Self> {
        let control = Self::default();
        control.storm_hall.store(u32::MAX, Ordering::Relaxed);
        Arc::new(control)
    }

    pub fn publish(&self, sim: &Sim) {
        self.updates.store(sim.updates(), Ordering::Relaxed);
        self.keys.store(sim.alert_keys() as u64, Ordering::Relaxed);
        for (slot, count) in self.active.iter().zip(sim.active_counts()) {
            slot.store(count, Ordering::Relaxed);
        }
        self.phase.store(sim.phase() as u8, Ordering::Relaxed);
        self.storm_hall
            .store(sim.storm_hall().unwrap_or(u32::MAX), Ordering::Relaxed);
        self.storm_ms
            .store(sim.storm_elapsed_ms(), Ordering::Relaxed);
        self.clock_ms.store(sim.clock_ms(), Ordering::Relaxed);
        self.hosts_down.store(sim.hosts_down(), Ordering::Relaxed);
    }

    /// Alert updates (firings) since the start.
    pub fn updates(&self) -> u64 {
        self.updates.load(Ordering::Relaxed)
    }

    /// Alert keys the console holds, resolved ones included.
    pub fn keys(&self) -> u64 {
        self.keys.load(Ordering::Relaxed)
    }

    /// Active alerts of an alert severity.
    pub fn active(&self, severity: Severity) -> u32 {
        self.active
            .get(severity.ix())
            .map_or(0, |count| count.load(Ordering::Relaxed))
    }

    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Relaxed))
    }

    /// The fleet node of the hall in the storm.
    pub fn storm_hall(&self) -> Option<u32> {
        match self.storm_hall.load(Ordering::Relaxed) {
            u32::MAX => None,
            id => Some(id),
        }
    }

    /// Simulated time since the storm began.
    pub fn storm_elapsed(&self) -> Duration {
        Duration::from_millis(self.storm_ms.load(Ordering::Relaxed))
    }

    /// The simulated wall clock, `hh:mm:ss`.
    pub fn clock(&self) -> String {
        clock_text(self.clock_ms.load(Ordering::Relaxed))
    }

    pub fn hosts_down(&self) -> u32 {
        self.hosts_down.load(Ordering::Relaxed)
    }
}

/// Alert updates in one simulated second, by alert severity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RateSample {
    /// The simulated wall clock's second.
    pub second: u64,
    pub counts: [u32; 4],
}

/// The last `HISTORY_SECONDS` of alert rates, for the chart.
#[derive(Clone, Debug, Default)]
pub struct RateHistory {
    pub version: u64,
    pub samples: VecDeque<RateSample>,
}

/// The rates the simulator's thread publishes and the chart polls.
#[derive(Clone, Debug, Default)]
pub struct RateFeed(Arc<Mutex<RateHistory>>);

impl RateFeed {
    pub fn snapshot(&self) -> RateHistory {
        self.0.lock().clone()
    }

    pub fn version(&self) -> u64 {
        self.0.lock().version
    }

    fn push(&self, samples: &[RateSample]) {
        if samples.is_empty() {
            return;
        }
        let mut history = self.0.lock();
        for sample in samples {
            if history.samples.len() == HISTORY_SECONDS {
                history.samples.pop_front();
            }
            history.samples.push_back(*sample);
        }
        history.version += 1;
    }
}

/// What one step wrote, for `Feeds::write`.
#[derive(Debug, Default)]
pub struct Out {
    pub alerts: Vec<Arc<Alert>>,
    pub removed_alerts: Vec<AlertKey>,
    pub nodes: Vec<Arc<FleetNode>>,
    pub services: Vec<Arc<ServiceRow>>,
    /// Services that moved into the pinned band: out of the rows.
    pub removed_services: Vec<u16>,
    pub pinned_services: Option<Vec<Arc<ServiceRow>>>,
    pub rates: Vec<RateSample>,
}

impl Out {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The grid stores the simulator writes, and the chart's rates.
pub struct Feeds {
    pub alerts: GridWriter<Alert>,
    pub fleet: GridWriter<FleetNode>,
    pub services: GridWriter<ServiceRow>,
    pub rates: RateFeed,
}

impl Feeds {
    /// The first load: every row as it is now.
    pub fn load(&self, sim: &mut Sim) {
        sim.out.clear();
        self.alerts.replace_all(sim.alert_rows());
        self.fleet.replace_all(sim.fleet_rows());
        let (pinned, rows) = sim.service_rows();
        self.services.batch(|batch| {
            batch.replace_all(rows);
            batch.set_pinned_rows(PinnedEdge::Top, pinned);
        });
        self.rates.push(&sim.rate_history());
    }

    /// Writes what the last steps left in `out`, one batch per store.
    pub fn write(&self, out: &mut Out) {
        if !out.alerts.is_empty() || !out.removed_alerts.is_empty() {
            let (rows, removed) = (
                mem::take(&mut out.alerts),
                mem::take(&mut out.removed_alerts),
            );
            self.alerts.batch(|batch| {
                for row in rows {
                    batch.upsert(row);
                }
                for key in removed {
                    batch.remove(key);
                }
            });
        }
        if !out.nodes.is_empty() {
            let rows = mem::take(&mut out.nodes);
            self.fleet.batch(|batch| {
                for row in rows {
                    batch.upsert(row);
                }
            });
        }
        if let Some(pinned) = out.pinned_services.take() {
            let (rows, removed) = (
                mem::take(&mut out.services),
                mem::take(&mut out.removed_services),
            );
            self.services.batch(|batch| {
                for id in removed {
                    batch.remove(id);
                }
                for row in rows {
                    batch.upsert(row);
                }
                batch.set_pinned_rows(PinnedEdge::Top, pinned);
            });
        }
        self.rates.push(&mem::take(&mut out.rates));
    }

    fn is_closed(&self) -> bool {
        self.alerts.is_closed()
    }
}

/// `sim` in real time, writing `feeds`, a step at a time: whoever runs it
/// calls `step` again after the wait it returns, until the alert store is
/// gone (`is_closed`). Natively a thread of its own does; in the browser,
/// with one thread, the executor's timer between frames.
pub struct Pump {
    sim: Sim,
    feeds: Feeds,
    control: Arc<Control>,
    commands: Receiver<Command>,
    last: Instant,
}

impl Pump {
    /// Warms `sim` up and writes the first load, so the console opens on
    /// the quiet baseline.
    pub fn start(
        mut sim: Sim,
        feeds: Feeds,
        control: Arc<Control>,
        commands: Receiver<Command>,
    ) -> Self {
        sim.warm_up(WARM_UP);
        feeds.load(&mut sim);
        control.publish(&sim);
        Self {
            sim,
            feeds,
            control,
            commands,
            last: Instant::now(),
        }
    }

    /// Applies the console's commands, steps the time since the last step
    /// and writes what it changed. Returns the wait until the next step.
    pub fn step(&mut self) -> Duration {
        for command in self.commands.try_iter() {
            self.sim.apply(command);
        }
        let now = Instant::now();
        self.sim.step((now - self.last).min(MAX_STEP));
        self.last = now;
        self.feeds.write(&mut self.sim.out);
        self.control.publish(&self.sim);
        STEP.saturating_sub(now.elapsed())
    }

    /// The alert store is gone: the console closed.
    pub fn is_closed(&self) -> bool {
        self.feeds.is_closed()
    }
}

/// `hh:mm:ss` of a simulated wall clock in milliseconds.
pub fn clock_text(ms: u64) -> String {
    let seconds = ms / 1_000 % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// A node of the fleet as built.
struct Node {
    parent: Option<u32>,
    level: Level,
    name: Arc<str>,
    /// The node id of its data center; a region's own id.
    dc: u32,
}

struct Hall {
    node: u32,
    racks: std::ops::Range<u32>,
    feed_b: bool,
    /// Cooling capacity, 1 nominal.
    cooling: f64,
    /// Degrees above the hosts' own inlet temperature.
    heat: f64,
}

struct Host {
    node: u32,
    hall: usize,
    service: u16,
    /// Offset of its sampling tick, ms.
    tick: u64,
    base_temp: f64,
    /// How much of the hall's heat reaches it: hot and cold spots.
    exposure: f64,
    /// How far above critical it holds before it goes down.
    margin: f64,
    cpu_base: f64,
    net_base: f64,
    err_base: f64,
    temp: f64,
    cpu: f64,
    mem: f64,
    power: f64,
    net: f64,
    errors: f64,
    throttled: bool,
    down: bool,
    /// When a down host may come back.
    back_at: u64,
    dirty: bool,
}

struct Service {
    name: Arc<str>,
    base_p99: f64,
    base_errors: f64,
    slo: f64,
    budget: f64,
    p99: f64,
    errors: f64,
    hosts: u32,
}

impl Service {
    fn burn(&self) -> f64 {
        self.errors / (100. - self.slo)
    }
}

/// A baseline alert's condition: true until `until`.
struct Trickle {
    key: AlertKey,
    severity: Severity,
    until: u64,
}

/// An alert as the simulator keeps it.
struct Live {
    row: Alert,
    next_fire: u64,
    period: u64,
    resolved_at: Option<u64>,
    /// The step that last saw its condition.
    seen: u64,
    /// Its firings in the last `RATE_WINDOW_MS`, oldest first.
    fired: VecDeque<u64>,
}

impl Live {
    /// Counts a firing at `now` and sets the row's count and rate.
    fn fire(&mut self, now: u64) {
        while self
            .fired
            .front()
            .is_some_and(|at| at + RATE_WINDOW_MS <= now)
        {
            self.fired.pop_front();
        }
        self.fired.push_back(now);
        self.row.rate = self.fired.len() as f64 * 1_000. / RATE_WINDOW_MS as f64;
    }

    /// Resolved: it no longer fires.
    fn resolve(&mut self, now: u64) {
        self.row.state = AlertState::Resolved;
        self.row.rate = 0.;
        self.fired.clear();
        self.resolved_at = Some(now);
    }
}

/// Active alerts of each alert severity per fleet node and per service.
struct Counts {
    nodes: Vec<[u16; 4]>,
    services: Vec<[u16; 4]>,
    /// Nodes whose counts changed this step.
    touched: Vec<u32>,
}

impl Counts {
    fn add(&mut self, subject: Subject, severity: Severity, delta: i16) {
        let Some(ix) = Severity::ALERTS.iter().position(|each| *each == severity) else {
            return;
        };
        let slot = match subject {
            Subject::Node(id) => {
                self.touched.push(id);
                &mut self.nodes[id as usize][ix]
            }
            Subject::Service(id) => &mut self.services[id as usize][ix],
        };
        *slot = slot.saturating_add_signed(delta);
    }

    fn worst(counts: &[u16; 4]) -> Severity {
        Severity::ALERTS
            .iter()
            .zip(counts)
            .find(|(_, count)| **count > 0)
            .map_or(Severity::Ok, |(severity, _)| *severity)
    }
}

struct Storm {
    hall: usize,
    started: u64,
    recovering_since: Option<u64>,
}

/// The simulator: see the module documentation.
pub struct Sim {
    rng: fastrand::Rng,
    /// Simulated milliseconds since the start.
    now: u64,
    /// `hh:mm:ss` of `now`, shared by the alerts that fire this second.
    now_text: Arc<str>,
    now_second: u64,
    nodes: Vec<Node>,
    halls: Vec<Hall>,
    hosts: Vec<Host>,
    /// The node id of the first host.
    first_host: u32,
    services: Vec<Service>,
    pinned: Vec<u16>,
    alerts: FxHashMap<AlertKey, Live>,
    counts: Counts,
    trickles: Vec<Trickle>,
    trickle_due: f64,
    storm: Option<Storm>,
    storm_ms: u64,
    generation: u64,
    updates: u64,
    /// Alert updates of the current second, by severity.
    bucket: [u32; 4],
    history: VecDeque<RateSample>,
    /// The last row written per node, to skip unchanged ones.
    written: Vec<Option<Arc<FleetNode>>>,
    conditions: Vec<(AlertKey, Severity)>,
    out: Out,
}

impl Sim {
    /// A fleet of `shape`, quiet; `storm_seconds` is how long a storm
    /// lasts before it recovers on its own.
    pub fn new(shape: FleetShape, seed: Option<u64>, storm_seconds: u64) -> Self {
        let mut rng = seed.map_or_else(fastrand::Rng::new, fastrand::Rng::with_seed);
        let mut nodes = Vec::new();
        let regions = shape.regions.min(REGIONS.len() as u32);
        for region in 0..regions {
            nodes.push(Node {
                parent: None,
                level: Level::Region,
                name: REGIONS[region as usize].0.into(),
                dc: region,
            });
        }
        let level = |nodes: &mut Vec<Node>, parents: std::ops::Range<u32>, count, level| {
            let start = nodes.len() as u32;
            for parent in parents {
                for ix in 0..count {
                    let parent_node = &nodes[parent as usize];
                    let id = nodes.len() as u32;
                    let (name, dc): (String, u32) = match level {
                        Level::Dc => {
                            let prefix = REGIONS[parent as usize].1;
                            (format!("{prefix}{}", ix + 1), id)
                        }
                        Level::Hall => (format!("{}-h{}", parent_node.name, ix + 1), parent),
                        Level::Rack => (
                            format!("{}-r{:02}", parent_node.name, ix + 1),
                            parent_node.dc,
                        ),
                        _ => (
                            format!("{}-n{:02}", parent_node.name, ix + 1),
                            parent_node.dc,
                        ),
                    };
                    nodes.push(Node {
                        parent: Some(parent),
                        level,
                        name: name.into(),
                        dc,
                    });
                }
            }
            start..nodes.len() as u32
        };
        let dcs = level(&mut nodes, 0..regions, shape.dcs, Level::Dc);
        let halls = level(&mut nodes, dcs, shape.halls, Level::Hall);
        let racks = level(&mut nodes, halls.clone(), shape.racks, Level::Rack);
        let hosts = level(&mut nodes, racks.clone(), shape.hosts, Level::Host);

        // Each hall's racks follow the previous hall's.
        let halls: Vec<Hall> = halls
            .enumerate()
            .map(|(ix, node)| {
                let first = racks.start + ix as u32 * shape.racks;
                Hall {
                    node,
                    racks: first..first + shape.racks,
                    feed_b: true,
                    cooling: 1.,
                    heat: 0.,
                }
            })
            .collect();
        // A hall's racks hold four services in blocks, so a storm in a
        // hall hurts some services and not others.
        let services_per_hall = 4u32;
        let first_host = hosts.start;
        let host_rows: Vec<Host> = hosts
            .clone()
            .map(|node| {
                let ix = node - first_host;
                let rack_ix = ix / shape.hosts;
                let hall_ix = (rack_ix / shape.racks) as usize;
                let rack_in_hall = rack_ix % shape.racks;
                let block = rack_in_hall * services_per_hall / shape.racks;
                let service = ((hall_ix as u32 * 3 + block) % SERVICES.len() as u32) as u16;
                let rack_exposure = 0.75 + 0.6 * f64::from(rack_in_hall % 5) / 4.;
                let cpu_base = 18. + rng.f64() * 45.;
                Host {
                    node,
                    hall: hall_ix,
                    service,
                    tick: u64::from(ix) * 37 % HOST_TICK_MS,
                    base_temp: 21.5 + rng.f64() * 5.,
                    exposure: rack_exposure * (0.9 + rng.f64() * 0.2),
                    margin: rng.f64() * 6.,
                    cpu_base,
                    net_base: 0.4 + rng.f64() * 6.,
                    err_base: rng.f64() * 0.4,
                    temp: 0.,
                    cpu: cpu_base,
                    mem: 35. + rng.f64() * 45.,
                    power: 0.,
                    net: 0.,
                    errors: 0.,
                    throttled: false,
                    down: false,
                    back_at: 0,
                    dirty: true,
                }
            })
            .collect();
        let mut services: Vec<Service> = SERVICES
            .iter()
            .map(|&(name, p99, slo)| Service {
                name: name.into(),
                base_p99: p99,
                // A tenth to half the errors the SLO allows.
                base_errors: (100. - slo) * (0.1 + rng.f64() * 0.4),
                slo,
                budget: 55. + rng.f64() * 43.,
                p99,
                errors: 0.,
                hosts: 0,
            })
            .collect();
        for host in &host_rows {
            services[host.service as usize].hosts += 1;
        }

        let node_count = nodes.len();
        let mut sim = Self {
            rng,
            now: 0,
            now_text: clock_text(START_CLOCK_MS).into(),
            now_second: START_CLOCK_MS / 1_000,
            nodes,
            halls,
            hosts: host_rows,
            first_host,
            services,
            pinned: Vec::new(),
            alerts: FxHashMap::default(),
            counts: Counts {
                nodes: vec![[0; 4]; node_count],
                services: vec![[0; 4]; SERVICES.len()],
                touched: Vec::new(),
            },
            trickles: Vec::new(),
            trickle_due: 0.,
            storm: None,
            storm_ms: storm_seconds.max(1) * 1_000,
            generation: 0,
            updates: 0,
            bucket: [0; 4],
            history: VecDeque::new(),
            written: vec![None; node_count],
            conditions: Vec::new(),
            out: Out::default(),
        };
        for ix in 0..sim.hosts.len() {
            sim.sample_host(ix);
        }
        sim.update_services(0.);
        sim.roll_up();
        sim.out.clear();
        sim
    }

    /// Steps `duration` of simulated time and drops what it wrote: the
    /// state the first load starts from.
    pub fn warm_up(&mut self, duration: Duration) {
        let steps = duration.as_millis() / 100;
        for _ in 0..steps {
            self.step(Duration::from_millis(100));
            self.out.clear();
        }
    }

    pub fn apply(&mut self, command: Command) {
        match command {
            Command::ToggleStorm => self.toggle_storm(),
            Command::Ack(keys) => self.ack(&keys),
        }
    }

    /// Starts a storm in a random hall; during a storm, restores the feed.
    pub fn toggle_storm(&mut self) {
        match &mut self.storm {
            Some(storm) if storm.recovering_since.is_none() => {
                storm.recovering_since = Some(self.now);
                self.halls[storm.hall].feed_b = true;
            }
            Some(storm) => {
                // Recovering: the feed drops again.
                storm.started = self.now;
                storm.recovering_since = None;
                self.halls[storm.hall].feed_b = false;
            }
            None => {
                let hall = self.rng.usize(..self.halls.len());
                self.halls[hall].feed_b = false;
                self.storm = Some(Storm {
                    hall,
                    started: self.now,
                    recovering_since: None,
                });
            }
        }
    }

    /// Acknowledges the firing alerts of `keys`; others are left alone.
    pub fn ack(&mut self, keys: &[AlertKey]) {
        for key in keys {
            if let Some(live) = self.alerts.get_mut(key)
                && live.row.state == AlertState::Firing
            {
                live.row.state = AlertState::Acknowledged;
                self.out.alerts.push(Arc::new(live.row.clone()));
            }
        }
    }

    /// Advances simulated time by `dt` and leaves its writes in `out`.
    pub fn step(&mut self, dt: Duration) {
        let dt_ms = dt.as_millis() as u64;
        if dt_ms == 0 {
            return;
        }
        let before = self.now;
        self.now += dt_ms;
        let dt_s = dt_ms as f64 / 1_000.;
        let second = (START_CLOCK_MS + self.now) / 1_000;
        if second != self.now_second {
            self.close_second(second);
        }
        self.advance_storm();
        self.advance_halls(dt_s);
        for ix in 0..self.hosts.len() {
            let tick = self.hosts[ix].tick;
            if (before + tick) / HOST_TICK_MS != (self.now + tick) / HOST_TICK_MS {
                self.sample_host(ix);
            }
        }
        self.spawn_trickles(dt_s);
        self.reconcile_alerts();
        self.write_hosts();
        if before / SERVICES_EVERY_MS != self.now / SERVICES_EVERY_MS {
            self.update_services(dt_s.max(SERVICES_EVERY_MS as f64 / 1_000.));
        }
        if before / ROLL_UP_EVERY_MS != self.now / ROLL_UP_EVERY_MS {
            self.roll_up();
        }
    }

    /// The writes the last steps left, for `Feeds::write`.
    #[cfg(test)]
    pub fn out_mut(&mut self) -> &mut Out {
        &mut self.out
    }

    pub fn phase(&self) -> Phase {
        match &self.storm {
            None => Phase::Quiet,
            Some(storm) if storm.recovering_since.is_none() => Phase::Incident,
            Some(_) => Phase::Recovering,
        }
    }

    /// The fleet node of the hall in the storm.
    pub fn storm_hall(&self) -> Option<u32> {
        self.storm.as_ref().map(|storm| self.halls[storm.hall].node)
    }

    fn storm_elapsed_ms(&self) -> u64 {
        self.storm
            .as_ref()
            .map_or(0, |storm| self.now - storm.started)
    }

    pub fn clock_ms(&self) -> u64 {
        START_CLOCK_MS + self.now
    }

    /// Alert updates (firings) since the start.
    pub fn updates(&self) -> u64 {
        self.updates
    }

    /// Alert keys held, resolved ones included: the console's rows.
    pub fn alert_keys(&self) -> usize {
        self.alerts.len()
    }

    /// Active alerts by alert severity.
    pub fn active_counts(&self) -> [u32; 4] {
        let mut counts = [0; 4];
        for live in self.alerts.values() {
            if live.row.state.is_active()
                && let Some(ix) = Severity::ALERTS
                    .iter()
                    .position(|severity| *severity == live.row.severity)
            {
                counts[ix] += 1;
            }
        }
        counts
    }

    pub fn hosts_down(&self) -> u32 {
        self.hosts.iter().filter(|host| host.down).count() as u32
    }

    /// The alert of `key`, as last written.
    #[cfg(test)]
    pub fn alert(&self, key: &AlertKey) -> Option<&Alert> {
        self.alerts.get(key).map(|live| &live.row)
    }

    /// Every alert as it is now.
    pub fn alert_rows(&self) -> Vec<Arc<Alert>> {
        self.alerts
            .values()
            .map(|live| Arc::new(live.row.clone()))
            .collect()
    }

    /// Every fleet node as last written.
    pub fn fleet_rows(&self) -> Vec<Arc<FleetNode>> {
        self.written.iter().flatten().cloned().collect()
    }

    /// The pinned services and the others, as they are now.
    pub fn service_rows(&self) -> (Vec<Arc<ServiceRow>>, Vec<Arc<ServiceRow>>) {
        let rows = (0..self.services.len()).map(|ix| Arc::new(self.service_row(ix)));
        let (mut pinned, rows): (Vec<_>, Vec<_>) =
            rows.partition(|row| self.pinned.contains(&row.id));
        pinned.sort_by(|a, b| a.budget.total_cmp(&b.budget));
        (pinned, rows)
    }

    fn rate_history(&self) -> Vec<RateSample> {
        self.history.iter().copied().collect()
    }

    /// The second that just ended goes into the history.
    fn close_second(&mut self, second: u64) {
        let sample = RateSample {
            second: self.now_second,
            counts: mem::take(&mut self.bucket),
        };
        if self.history.len() == HISTORY_SECONDS {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        self.out.rates.push(sample);
        self.now_second = second;
        self.now_text = clock_text(second * 1_000).into();
    }

    fn advance_storm(&mut self) {
        let Some(storm) = &mut self.storm else {
            return;
        };
        match storm.recovering_since {
            None if self.now - storm.started >= self.storm_ms => {
                storm.recovering_since = Some(self.now);
                self.halls[storm.hall].feed_b = true;
            }
            Some(since) if self.now - since > 5_000 => {
                let hall = storm.hall;
                let calm = self.halls[hall].heat < 0.5
                    && self
                        .hosts
                        .iter()
                        .all(|host| host.hall != hall || !host.down);
                if calm {
                    self.storm = None;
                }
            }
            _ => {}
        }
    }

    fn advance_halls(&mut self, dt: f64) {
        for hall in &mut self.halls {
            let (target, lag) = if hall.feed_b {
                (1., 2.)
            } else {
                (COOLING_ON_ONE_FEED, 6.)
            };
            hall.cooling += (target - hall.cooling) * (dt / lag).min(1.);
            let heat = HEAT_SPAN * (1. - hall.cooling);
            hall.heat += (heat - hall.heat) * (dt / 4.).min(1.);
        }
    }

    /// Samples host `ix`'s metrics from its hall.
    fn sample_host(&mut self, ix: usize) {
        let (rng, now) = (&mut self.rng, self.now);
        let host = &mut self.hosts[ix];
        let hall = &self.halls[host.hall];
        host.temp = host.base_temp + hall.heat * host.exposure + (rng.f64() - 0.5) * 0.8;
        if host.down {
            if hall.heat < 4. && now >= host.back_at {
                host.down = false;
            }
        } else if host.temp > TEMP_CRITICAL as f64 + 1. + host.margin {
            host.down = true;
            host.back_at = now + 3_000 + rng.u64(..9_000);
        }
        host.throttled = !host.down
            && (host.temp >= THROTTLE_AT || (host.throttled && host.temp > THROTTLE_CLEAR));
        host.cpu = if host.down {
            0.
        } else if host.throttled {
            (host.cpu_base + 35. + rng.f64() * 6.).min(99.)
        } else {
            (host.cpu_base + (rng.f64() - 0.5) * 8.).clamp(3., 97.)
        };
        host.mem = (host.mem + (rng.f64() - 0.5) * 2.).clamp(20., 95.);
        host.power = if host.down {
            0.05
        } else {
            round2(0.18 + 0.004 * host.cpu + rng.f64() * 0.02)
        };
        host.net = if host.down {
            0.
        } else {
            round2(host.net_base * (0.8 + rng.f64() * 0.4))
        };
        host.errors = if host.down {
            0.
        } else if host.throttled {
            round2(host.err_base + 3. + rng.f64() * 5.)
        } else {
            round2(host.err_base * rng.f64())
        };
        host.dirty = true;
    }

    fn spawn_trickles(&mut self, dt: f64) {
        let now = self.now;
        self.trickles.retain(|trickle| trickle.until > now);
        self.trickle_due -= dt;
        while self.trickle_due <= 0. {
            self.trickle_due += (0.2 + self.rng.f64() * 2.) / TRICKLE_RATE;
            let host = self.first_host + self.rng.u32(..self.hosts.len() as u32);
            let (rule, severity) = match self.rng.u8(..3) {
                0 => (Rule::DiskLatency, Severity::Warning),
                1 => (Rule::MemoryPressure, Severity::Minor),
                _ => (Rule::ClockDrift, Severity::Warning),
            };
            let key = AlertKey {
                rule,
                subject: Subject::Node(host),
            };
            if self.trickles.iter().all(|trickle| trickle.key != key) {
                self.trickles.push(Trickle {
                    key,
                    severity,
                    until: now + 8_000 + self.rng.u64(..37_000),
                });
            }
        }
    }

    /// Every condition that holds now, with its severity.
    fn collect_conditions(&self, out: &mut Vec<(AlertKey, Severity)>) {
        let node = |rule, id| AlertKey {
            rule,
            subject: Subject::Node(id),
        };
        for hall in &self.halls {
            if !hall.feed_b {
                out.push((node(Rule::PowerFeedDown, hall.node), Severity::Critical));
                for rack in hall.racks.clone() {
                    out.push((node(Rule::PduOneFeed, rack), Severity::Major));
                }
            }
            if hall.cooling < 0.85 {
                let severity = if hall.cooling < 0.5 {
                    Severity::Critical
                } else {
                    Severity::Major
                };
                out.push((node(Rule::CoolingDegraded, hall.node), severity));
            }
        }
        for host in &self.hosts {
            if host.down {
                out.push((node(Rule::HostUnreachable, host.node), Severity::Critical));
                continue;
            }
            let temp = host.temp.round() as i64;
            let heat = match temp {
                TEMP_CRITICAL.. => Some(Severity::Critical),
                TEMP_MAJOR.. => Some(Severity::Major),
                TEMP_WARNING.. => Some(Severity::Warning),
                _ => None,
            };
            if let Some(severity) = heat {
                out.push((node(Rule::InletTempHigh, host.node), severity));
            }
            if host.throttled {
                let severity = if temp >= 38 {
                    Severity::Major
                } else {
                    Severity::Minor
                };
                out.push((node(Rule::CpuThrottled, host.node), severity));
            }
            if host.errors >= 2.5 {
                out.push((node(Rule::HostErrors, host.node), Severity::Minor));
            }
        }
        out.extend(
            self.trickles
                .iter()
                .map(|trickle| (trickle.key, trickle.severity)),
        );
        for (id, service) in self.services.iter().enumerate() {
            let key = |rule| AlertKey {
                rule,
                subject: Subject::Service(id as u16),
            };
            if service.errors >= 3. {
                out.push((key(Rule::ErrorRate), Severity::Critical));
            } else if service.errors >= 1. {
                out.push((key(Rule::ErrorRate), Severity::Major));
            }
            let slow = service.p99 / service.base_p99;
            if slow >= 4. {
                out.push((key(Rule::LatencySlo), Severity::Major));
            } else if slow >= 2. {
                out.push((key(Rule::LatencySlo), Severity::Minor));
            }
            let burn = service.burn();
            if service.budget < 15. && burn >= 1. {
                out.push((key(Rule::BudgetBurn), Severity::Critical));
            } else if burn >= 6. {
                out.push((key(Rule::BudgetBurn), Severity::Major));
            } else if burn >= 2. {
                out.push((key(Rule::BudgetBurn), Severity::Warning));
            }
        }
    }

    /// Fires, raises, lowers and resolves the alerts by the conditions
    /// that hold now, and removes resolved alerts after a while.
    fn reconcile_alerts(&mut self) {
        let mut conditions = mem::take(&mut self.conditions);
        conditions.clear();
        self.collect_conditions(&mut conditions);
        self.generation += 1;
        for &(key, severity) in &conditions {
            self.observe(key, severity);
        }
        self.conditions = conditions;

        let (now, generation) = (self.now, self.generation);
        let Self {
            alerts,
            counts,
            out,
            now_text,
            ..
        } = self;
        alerts.retain(|key, live| {
            if live.seen != generation && live.row.state.is_active() {
                counts.add(key.subject, live.row.severity, -1);
                live.resolve(now);
                live.row.last_seen = now_text.clone();
                out.alerts.push(Arc::new(live.row.clone()));
            }
            let expired = live
                .resolved_at
                .is_some_and(|at| now - at >= RESOLVED_LINGER_MS);
            if expired {
                out.removed_alerts.push(*key);
            }
            !expired
        });
        for id in self.counts.touched.drain(..) {
            if let Some(ix) = id.checked_sub(self.first_host)
                && let Some(host) = self.hosts.get_mut(ix as usize)
            {
                host.dirty = true;
            }
        }
    }

    /// The condition of `key` holds at `severity`.
    fn observe(&mut self, key: AlertKey, severity: Severity) {
        let (now, generation) = (self.now, self.generation);
        let period = match key.subject {
            Subject::Node(_) if self.storm.is_some() => 250 + self.rng.u64(..500),
            Subject::Node(_) => 1_500 + self.rng.u64(..3_000),
            Subject::Service(_) => 400 + self.rng.u64(..400),
        };
        let fired = match self.alerts.entry(key) {
            Entry::Vacant(entry) => {
                let (subject, service, dc) = describe(
                    &self.nodes,
                    &self.hosts,
                    self.first_host,
                    &self.services,
                    key.subject,
                );
                let row = Alert {
                    key,
                    severity,
                    state: AlertState::Firing,
                    subject,
                    service,
                    dc,
                    count: 1,
                    rate: 0.,
                    first_seen: self.now_text.clone(),
                    last_seen: self.now_text.clone(),
                };
                self.counts.add(key.subject, severity, 1);
                let live = entry.insert(Live {
                    row,
                    next_fire: now + period,
                    period,
                    resolved_at: None,
                    seen: generation,
                    fired: VecDeque::new(),
                });
                live.fire(now);
                &live.row
            }
            Entry::Occupied(entry) => {
                let live = entry.into_mut();
                live.seen = generation;
                let row = &mut live.row;
                if !row.state.is_active() {
                    // Fired again before it was removed.
                    row.state = AlertState::Firing;
                    row.severity = severity;
                    live.resolved_at = None;
                    self.counts.add(key.subject, severity, 1);
                } else if row.severity != severity {
                    self.counts.add(key.subject, row.severity, -1);
                    self.counts.add(key.subject, severity, 1);
                    // Worse than when acknowledged: it fires again.
                    if severity < row.severity {
                        row.state = AlertState::Firing;
                    }
                    row.severity = severity;
                } else if now < live.next_fire {
                    return;
                }
                live.period = period;
                live.next_fire = now + period;
                row.count += 1;
                row.last_seen = self.now_text.clone();
                live.fire(now);
                &live.row
            }
        };
        self.updates += 1;
        if let Some(ix) = Severity::ALERTS
            .iter()
            .position(|each| *each == fired.severity)
        {
            self.bucket[ix] += 1;
        }
        self.out.alerts.push(Arc::new(fired.clone()));
    }

    /// Writes the hosts whose metrics or alerts changed.
    fn write_hosts(&mut self) {
        for ix in 0..self.hosts.len() {
            if mem::take(&mut self.hosts[ix].dirty) {
                let row = self.host_row(ix);
                self.emit_node(row);
            }
        }
    }

    fn host_row(&self, ix: usize) -> FleetNode {
        let host = &self.hosts[ix];
        let node = &self.nodes[host.node as usize];
        let counts = &self.counts.nodes[host.node as usize];
        FleetNode {
            id: host.node,
            parent: node.parent,
            level: Level::Host,
            name: node.name.clone(),
            status: Counts::worst(counts),
            hosts: 1,
            in_alert: counts.iter().any(|count| *count > 0).into(),
            temp: host.temp.round() as i64,
            power: host.power,
            cpu: host.cpu.round() as i64,
            mem: host.mem.round() as i64,
            net: host.net,
            errors: host.errors,
            service: Some(self.services[host.service as usize].name.clone()),
        }
    }

    /// Writes `row` unless it is the row last written for its node.
    fn emit_node(&mut self, row: FleetNode) {
        let slot = &mut self.written[row.id as usize];
        if slot.as_deref() == Some(&row) {
            return;
        }
        let row = Arc::new(row);
        *slot = Some(row.clone());
        self.out.nodes.push(row);
    }

    /// Writes every rack, hall, data center and region with its hosts'
    /// roll-ups: the host count, the hosts in alert, the hottest host, the
    /// summed power, network and errors, the average CPU and memory, and
    /// the worst status of its own alerts and its children's. Children
    /// come after their parent in id order, so one backward pass adds
    /// every child into its parent after the child is complete.
    pub fn roll_up(&mut self) {
        #[derive(Clone, Copy, Default)]
        struct Sum {
            hosts: u32,
            in_alert: u32,
            temp: Option<i64>,
            power: f64,
            cpu: f64,
            mem: f64,
            net: f64,
            errors: f64,
            status: Option<Severity>,
        }
        let mut sums = vec![Sum::default(); self.nodes.len()];
        for id in (0..self.nodes.len()).rev() {
            let row = if id as u32 >= self.first_host {
                self.host_row(id - self.first_host as usize)
            } else {
                let sum = sums[id];
                let node = &self.nodes[id];
                let own = Counts::worst(&self.counts.nodes[id]);
                let hosts = sum.hosts.max(1);
                let row = FleetNode {
                    id: id as u32,
                    parent: node.parent,
                    level: node.level,
                    name: node.name.clone(),
                    status: own.worst(sum.status.unwrap_or_default()),
                    hosts: sum.hosts,
                    in_alert: sum.in_alert,
                    temp: sum.temp.unwrap_or_default(),
                    power: round2(sum.power),
                    cpu: (sum.cpu / f64::from(hosts)).round() as i64,
                    mem: (sum.mem / f64::from(hosts)).round() as i64,
                    net: round2(sum.net),
                    errors: round2(sum.errors),
                    service: None,
                };
                self.emit_node(row.clone());
                row
            };
            if let Some(parent) = row.parent {
                let host = row.is_host();
                let weight = if host { 1. } else { f64::from(row.hosts) };
                let into = &mut sums[parent as usize];
                into.hosts += row.hosts;
                into.in_alert += row.in_alert;
                into.temp = Some(into.temp.map_or(row.temp, |temp| temp.max(row.temp)));
                into.power += row.power;
                into.cpu += row.cpu as f64 * weight;
                into.mem += row.mem as f64 * weight;
                into.net += row.net;
                into.errors += row.errors;
                into.status = Some(
                    into.status
                        .map_or(row.status, |status| status.worst(row.status)),
                );
            }
        }
    }

    /// Latency, errors and the error budget of every service from the
    /// health of its hosts; then the services written, the lowest budgets
    /// pinned.
    fn update_services(&mut self, dt: f64) {
        let mut impaired = vec![0f64; self.services.len()];
        for host in &self.hosts {
            impaired[host.service as usize] += if host.down {
                1.
            } else if host.throttled {
                0.5
            } else {
                0.
            };
        }
        for (service, impaired) in self.services.iter_mut().zip(impaired) {
            let share = impaired / f64::from(service.hosts.max(1));
            let noise = 0.97 + self.rng.f64() * 0.06;
            service.p99 = service.base_p99 * (1. + 12. * share) * noise;
            service.errors = service.base_errors * noise + 25. * share;
            let burn = service.burn();
            service.budget = (service.budget - burn * BUDGET_DRAIN * dt
                + if burn < 1. { BUDGET_REFILL * dt } else { 0. })
            .clamp(0., 100.);
        }
        let mut order: Vec<u16> = (0..self.services.len() as u16).collect();
        order.sort_by(|a, b| {
            let budget = |id: &u16| self.services[*id as usize].budget;
            budget(a).total_cmp(&budget(b))
        });
        order.truncate(PINNED_SERVICES);
        let removed: Vec<u16> = order
            .iter()
            .filter(|id| !self.pinned.contains(id))
            .copied()
            .collect();
        self.pinned = order;
        let (pinned, rows) = self.service_rows();
        self.out.removed_services = removed;
        self.out.services = rows;
        self.out.pinned_services = Some(pinned);
    }

    fn service_row(&self, ix: usize) -> ServiceRow {
        let service = &self.services[ix];
        let slow = service.p99 / service.base_p99;
        let health = if service.budget < 10. || service.errors >= 3. {
            Severity::Critical
        } else if service.errors >= 1. || slow >= 4. {
            Severity::Major
        } else if service.errors >= 0.3 || slow >= 2. {
            Severity::Minor
        } else if service.budget < 40. {
            Severity::Warning
        } else {
            Severity::Ok
        };
        ServiceRow {
            id: ix as u16,
            name: service.name.clone(),
            status: health.worst(Counts::worst(&self.counts.services[ix])),
            slo: service.slo,
            budget: round2(service.budget),
            p99: service.p99.round() as i64,
            errors: round2(service.errors),
            hosts: service.hosts,
        }
    }
}

/// An alert's subject, service and data center names.
fn describe(
    nodes: &[Node],
    hosts: &[Host],
    first_host: u32,
    services: &[Service],
    subject: Subject,
) -> (Arc<str>, Arc<str>, Arc<str>) {
    match subject {
        Subject::Node(id) => {
            let node = &nodes[id as usize];
            let service = id
                .checked_sub(first_host)
                .and_then(|ix| hosts.get(ix as usize))
                .map_or_else(
                    || Arc::from(FACILITIES),
                    |host| services[host.service as usize].name.clone(),
                );
            (
                node.name.clone(),
                service,
                nodes[node.dc as usize].name.clone(),
            )
        }
        Subject::Service(id) => {
            let name = services[id as usize].name.clone();
            (name.clone(), name, GLOBAL.into())
        }
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.).round() / 100.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(sim: &mut Sim, seconds: u64) {
        for _ in 0..seconds * 10 {
            sim.step(Duration::from_millis(100));
        }
    }

    #[test]
    fn test_shape_sizes_the_fleet() {
        assert_eq!(
            FleetShape::for_hosts(DEFAULT_HOSTS).host_count(),
            DEFAULT_HOSTS
        );
        assert_eq!(FleetShape::for_hosts(1).host_count(), 360);
        let sim = Sim::new(FleetShape::small(), Some(1), 30);
        assert_eq!(sim.hosts.len() as u32, FleetShape::small().host_count());
        // Every node's parent comes before it.
        for (id, node) in sim.nodes.iter().enumerate() {
            assert!(node.parent.is_none_or(|parent| (parent as usize) < id));
        }
    }

    #[test]
    fn test_storm_runs_its_course() {
        let mut sim = Sim::new(FleetShape::small(), Some(3), 20);
        step(&mut sim, 5);
        assert_eq!(sim.phase(), Phase::Quiet);
        sim.toggle_storm();
        step(&mut sim, 15);
        assert_eq!(sim.phase(), Phase::Incident);
        let critical = sim.active_counts()[0];
        assert!(critical > 0, "a storm raises critical alerts");
        // The hall's power alert fires every 250–750 ms: its rate over the
        // last 10 s is the simulator's count of firings in them.
        let feed = AlertKey {
            rule: Rule::PowerFeedDown,
            subject: Subject::Node(sim.storm_hall().expect("a storm")),
        };
        let rate = sim.alert(&feed).expect("the feed alert").rate;
        assert!((1.3..=4.1).contains(&rate), "rate {rate}");
        step(&mut sim, 10);
        assert_eq!(sim.phase(), Phase::Recovering);
        step(&mut sim, 60);
        assert_eq!(sim.phase(), Phase::Quiet);
        assert_eq!(sim.hosts_down(), 0);
        assert!(sim.active_counts()[0] < critical);
    }
}

/// The simulator writing real grid stores, headless, on a few dozen hosts.
#[cfg(all(test, feature = "test-support"))]
mod store_tests {
    use gpui_kit::TestAppContext;
    use gpui_kit_grid::GridStore;

    use super::super::{alerts, fleet, services};
    use super::*;

    struct Stores {
        alerts: GridStore<Alert>,
        fleet: GridStore<FleetNode>,
        feeds: Feeds,
    }

    /// The console's stores, loaded with `sim`.
    fn stores(sim: &mut Sim, cx: &mut TestAppContext) -> Stores {
        let stores = cx.update(|cx| {
            let alerts = GridStore::new(alerts::store_options(), cx);
            let fleet = GridStore::new(fleet::store_options(), cx);
            let services: GridStore<ServiceRow> = GridStore::new(services::store_options(), cx);
            let feeds = Feeds {
                alerts: alerts.writer(),
                fleet: fleet.writer(),
                services: services.writer(),
                rates: RateFeed::default(),
            };
            Stores {
                alerts,
                fleet,
                feeds,
            }
        });
        stores.feeds.load(sim);
        settle(cx);
        stores
    }

    /// Lets the stores publish what was written.
    fn settle(cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
        }
    }

    /// `seconds` of simulated time in 100 ms steps, each step written.
    fn run(sim: &mut Sim, stores: &Stores, seconds: u64, cx: &mut TestAppContext) {
        for _ in 0..seconds * 10 {
            sim.step(Duration::from_millis(100));
            stores.feeds.write(sim.out_mut());
        }
        settle(cx);
    }

    fn storm(cx: &mut TestAppContext) -> (Sim, Stores) {
        let mut sim = Sim::new(FleetShape::small(), Some(11), 60);
        let stores = stores(&mut sim, cx);
        sim.toggle_storm();
        run(&mut sim, &stores, 12, cx);
        (sim, stores)
    }

    #[gpui_kit::test]
    fn test_storm_collapses_repeated_alerts_into_one_row_per_key(cx: &mut TestAppContext) {
        let (mut sim, stores) = storm(cx);
        let before = sim.updates();
        run(&mut sim, &stores, 5, cx);
        let updates = sim.updates() - before;
        let rows = stores.alerts.snapshot();

        // One row per key the simulator holds, with the count and the
        // rate it keeps, however often it fired.
        assert_eq!(rows.len(), sim.alert_keys());
        for (_, row) in rows.iter() {
            let held = sim.alert(&row.key).expect("a held key");
            assert_eq!(
                (row.count, row.state, row.rate),
                (held.count, held.state, held.rate)
            );
        }
        assert!(
            updates > 5 * rows.len() as u64,
            "{updates} updates in 5 s over {} rows",
            rows.len()
        );
        // The hall's power alert fired more than once a second all along.
        let hall = sim.storm_hall().expect("a storm");
        let feed = AlertKey {
            rule: Rule::PowerFeedDown,
            subject: Subject::Node(hall),
        };
        let row = rows.get(&feed).expect("the feed alert");
        assert!(row.count > 17, "count {}", row.count);
        assert_eq!(row.severity, Severity::Critical);
    }

    #[gpui_kit::test]
    fn test_ack_changes_the_state(cx: &mut TestAppContext) {
        let (mut sim, stores) = storm(cx);
        let hall = sim.storm_hall().expect("a storm");
        let feed = AlertKey {
            rule: Rule::PowerFeedDown,
            subject: Subject::Node(hall),
        };
        let row = |stores: &Stores| stores.alerts.snapshot().get(&feed).cloned().expect("row");
        let firing = row(&stores);
        assert_eq!(firing.state, AlertState::Firing);

        sim.apply(Command::Ack(Arc::from([feed])));
        run(&mut sim, &stores, 2, cx);
        let acked = row(&stores);
        assert_eq!(acked.state, AlertState::Acknowledged);
        // Acknowledged, it still counts its firings.
        assert!(acked.count > firing.count);
        assert_eq!(sim.active_counts()[0], {
            let rows = stores.alerts.snapshot();
            rows.iter()
                .filter(|(_, alert)| {
                    alert.state.is_active() && alert.severity == Severity::Critical
                })
                .count() as u32
        });

        // The feed comes back: the alert resolves and stops firing, then
        // leaves.
        sim.toggle_storm();
        run(&mut sim, &stores, 1, cx);
        let resolved = row(&stores);
        assert_eq!((resolved.state, resolved.rate), (AlertState::Resolved, 0.));
        run(&mut sim, &stores, 5, cx);
        assert!(stores.alerts.snapshot().get(&feed).is_none());
    }

    #[gpui_kit::test]
    fn test_tree_roll_ups_match_their_children(cx: &mut TestAppContext) {
        let (mut sim, stores) = storm(cx);
        sim.roll_up();
        stores.feeds.write(sim.out_mut());
        settle(cx);

        let nodes = stores.fleet.snapshot();
        assert_eq!(nodes.len(), sim.nodes.len());
        let mut checked = 0;
        for (_, node) in nodes.iter().filter(|(_, node)| !node.is_host()) {
            let children: Vec<&Arc<FleetNode>> = nodes
                .iter()
                .map(|(_, child)| child)
                .filter(|child| child.parent == Some(node.id))
                .collect();
            assert!(!children.is_empty(), "{}", node.name);
            let hosts: u32 = children.iter().map(|child| child.hosts).sum();
            let in_alert: u32 = children.iter().map(|child| child.in_alert).sum();
            let hottest = children.iter().map(|child| child.temp).max();
            let power: f64 = children.iter().map(|child| child.power).sum();
            let worst = children
                .iter()
                .map(|child| child.status)
                .fold(Severity::Ok, Severity::worst);
            assert_eq!(node.hosts, hosts, "{}", node.name);
            assert_eq!(node.in_alert, in_alert, "{}", node.name);
            assert_eq!(Some(node.temp), hottest, "{}", node.name);
            assert!((node.power - power).abs() < 0.005, "{}", node.name);
            // Its own alerts (power, cooling) can make it worse.
            assert!(node.status <= worst, "{}", node.name);
            checked += 1;
        }
        assert!(checked > 0);
        // The storm shows at the top: hosts in alert, a critical status.
        let hall = nodes
            .get(&sim.storm_hall().expect("a storm"))
            .expect("hall");
        assert!(hall.in_alert > 0);
        assert_eq!(hall.status, Severity::Critical);
        let hosts: u32 = nodes
            .iter()
            .filter(|(_, node)| node.parent.is_none())
            .map(|(_, node)| node.hosts)
            .sum();
        assert_eq!(hosts, FleetShape::small().host_count());
    }
}
