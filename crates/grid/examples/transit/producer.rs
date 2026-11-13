//! The producer: asks its [`VehicleSource`] for every vehicle under way at
//! the clock's time, writes one row per vehicle to the grid store (a new
//! run flashes, a departure flashes its leg, a finished run fades out) and
//! publishes what the map and the status bar show ([`TransitFrame`]).
//! With a station chosen it also writes that station's board, the next
//! departures by the timetable (`board.rs`). Natively it runs on a thread
//! of its own; in the browser on the executor's timer. No GPUI types but
//! the task it runs in.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use gpui_kit::{App, Task};
use gpui_kit_grid::GridWriter;
use parking_lot::Mutex;
// `std::time` natively; the browser's clock in wasm, where
// `std::time::{Instant, SystemTime}::now` panic.
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use super::{
    board::{BOARD_ROWS, BoardRow},
    rows::VehicleRow,
    source::{
        Departure, FeedStatus, GeoPoint, Line, LiveState, Mode, SourceClock, SourceInfo, Vehicle,
        VehicleKey, VehicleSource,
    },
};

/// How often the producer looks at the clock; a sweep runs when its
/// interval (`Warp::sweep_interval`) has passed.
pub const STEP_INTERVAL: Duration = Duration::from_millis(250);

/// How fast simulated time runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Warp {
    #[default]
    RealTime,
    Ten,
    Sixty,
}

impl Warp {
    pub const ALL: [Self; 3] = [Self::RealTime, Self::Ten, Self::Sixty];

    pub fn factor(self) -> f64 {
        match self {
            Self::RealTime => 1.,
            Self::Ten => 10.,
            Self::Sixty => 60.,
        }
    }

    /// `×1`, `×10`, `×60`.
    pub fn label(self) -> &'static str {
        match self {
            Self::RealTime => "×1",
            Self::Ten => "×10",
            Self::Sixty => "×60",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::RealTime => Self::Ten,
            Self::Ten => Self::Sixty,
            Self::Sixty => Self::RealTime,
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    /// A sweep a second in real time, where the countdowns tick by one;
    /// four a second while time runs fast, so vehicles glide.
    pub fn sweep_interval(self) -> Duration {
        match self {
            Self::RealTime => Duration::from_secs(1),
            Self::Ten | Self::Sixty => STEP_INTERVAL,
        }
    }
}

/// Unix ms of the real clock.
pub fn real_now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |since| since.as_secs_f64() * 1e3)
}

/// The simulated clock: the real one, from a fixed start if one is given,
/// running `warp` times as fast.
#[derive(Clone, Copy, Debug)]
struct SimClock {
    real_origin: f64,
    sim_origin: f64,
    warp: Warp,
}

impl SimClock {
    fn new(start_ms: Option<f64>) -> Self {
        let now = real_now_ms();
        Self {
            real_origin: now,
            sim_origin: start_ms.unwrap_or(now),
            warp: Warp::RealTime,
        }
    }

    fn at(&self, real_ms: f64) -> f64 {
        self.sim_origin + (real_ms - self.real_origin) * self.warp.factor()
    }

    fn set_warp(&mut self, warp: Warp) {
        let now = real_now_ms();
        self.sim_origin = self.at(now);
        self.real_origin = now;
        self.warp = warp;
    }

    fn jump_to(&mut self, unix_ms: f64) {
        self.real_origin = real_now_ms();
        self.sim_origin = unix_ms;
    }
}

/// A vehicle on the map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dot {
    pub key: VehicleKey,
    pub at: GeoPoint,
    /// Its line, by rank: an index into the network's lines (a live line
    /// the network does not know ranks after them).
    pub line: u32,
    pub mode: Mode,
}

/// The selected vehicle, for the map: its run's stations and where it is.
#[derive(Clone, Debug)]
pub struct Selected {
    pub line: Arc<Line>,
    pub headsign: Arc<str>,
    pub leg: Arc<str>,
    pub at: GeoPoint,
    pub path: Arc<[GeoPoint]>,
    pub leg_ix: u16,
    pub next_arrival_ms: f64,
    /// What a live vehicle reports.
    pub live: Option<LiveState>,
}

/// What one sweep publishes for the map and the status bar.
#[derive(Clone, Debug, Default)]
pub struct TransitFrame {
    /// The simulated time of the sweep, Unix ms; 0 before the first.
    pub sim_ms: f64,
    pub sweeps: u64,
    pub under_way: u32,
    /// Under way, by `Mode::ix`.
    pub by_mode: [u32; Mode::COUNT],
    /// The trips of the service day, if the source knows them.
    pub trips_today: Option<u32>,
    /// In the last sweep: runs that started, legs begun, runs that ended.
    pub started: u32,
    pub departures: u32,
    pub finished: u32,
    /// Rows written a second: the sweep's rows over the real time since
    /// the sweep before it.
    pub rate: u64,
    /// Asking the source for the vehicles.
    pub compute: Duration,
    /// Building the rows and handing them to the store's writer.
    pub write: Duration,
    /// Loading the source, once.
    pub load: Duration,
    pub dots: Vec<Dot>,
    pub selected: Option<Selected>,
    pub info: Option<Arc<SourceInfo>>,
    /// The station whose board is written, and its departures shown.
    pub station: Option<u32>,
    pub board: u32,
    /// How a live feed is doing.
    pub feed: Option<FeedStatus>,
}

/// What the window and the producer share: the clock, the selection and
/// the last frame.
pub struct Control {
    clock: Mutex<SimClock>,
    /// The selected vehicle's key + 1; 0 for none.
    selected: AtomicU64,
    /// The station of the board + 1; 0 for none.
    station: AtomicU64,
    frame: Mutex<Arc<TransitFrame>>,
    version: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Control {
    /// A clock from `start_ms` (Unix ms; the real time if `None`).
    pub fn new(start_ms: Option<f64>) -> Arc<Self> {
        Arc::new(Self {
            clock: Mutex::new(SimClock::new(start_ms)),
            selected: AtomicU64::new(0),
            station: AtomicU64::new(0),
            frame: Mutex::new(Arc::default()),
            version: AtomicU64::new(0),
            error: Mutex::new(None),
        })
    }

    /// The simulated time now, Unix ms.
    pub fn now(&self) -> f64 {
        self.clock.lock().at(real_now_ms())
    }

    pub fn warp(&self) -> Warp {
        self.clock.lock().warp
    }

    /// Runs time `warp` times as fast from now on.
    pub fn set_warp(&self, warp: Warp) {
        self.clock.lock().set_warp(warp);
    }

    /// Moves the clock to `unix_ms`; it runs on from there.
    pub fn jump_to(&self, unix_ms: f64) {
        self.clock.lock().jump_to(unix_ms);
    }

    pub fn selected(&self) -> Option<VehicleKey> {
        let value = self.selected.load(Ordering::Relaxed);
        (value > 0).then(|| VehicleKey::new(value - 1))
    }

    /// Selects a vehicle for the map.
    pub fn select(&self, key: Option<VehicleKey>) {
        let value = key.map_or(0, |key| key.get().wrapping_add(1));
        self.selected.store(value, Ordering::Relaxed);
    }

    /// The station whose board is written.
    pub fn station(&self) -> Option<u32> {
        let value = self.station.load(Ordering::Relaxed);
        (value > 0).then(|| (value - 1) as u32)
    }

    /// Chooses the station of the board, or none.
    pub fn set_station(&self, station: Option<u32>) {
        let value = station.map_or(0, |station| u64::from(station) + 1);
        self.station.store(value, Ordering::Relaxed);
    }

    pub fn frame(&self) -> Arc<TransitFrame> {
        self.frame.lock().clone()
    }

    /// Bumped by every frame published.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Why the source has no vehicles, if it has none.
    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }

    fn publish(&self, frame: TransitFrame) {
        *self.frame.lock() = Arc::new(frame);
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    fn fail(&self, error: String) {
        *self.error.lock() = Some(error);
        self.version.fetch_add(1, Ordering::Relaxed);
    }
}

/// What a sweep did to the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepCounts {
    pub started: u32,
    pub departures: u32,
    pub finished: u32,
    pub written: u32,
}

/// The source, the stores' writers, the clock and the rows written last.
pub struct Producer {
    source: Box<dyn VehicleSource>,
    info: Option<Arc<SourceInfo>>,
    writer: GridWriter<VehicleRow>,
    control: Arc<Control>,
    /// Each run's row written last, with the sweep that saw it.
    rows: HashMap<VehicleKey, (Arc<VehicleRow>, u64)>,
    vehicles: Vec<Vehicle>,
    /// The board's store, its rows written last and their station.
    board: Option<GridWriter<BoardRow>>,
    board_rows: HashMap<VehicleKey, Arc<BoardRow>>,
    board_station: Option<u32>,
    departures: Vec<Departure>,
    sweeps: u64,
    load: Duration,
    started: Instant,
    last_sweep: Option<Instant>,
}

impl Producer {
    /// A producer of `source`'s vehicles, writing `writer`.
    pub fn new(
        source: Box<dyn VehicleSource>,
        writer: GridWriter<VehicleRow>,
        control: Arc<Control>,
    ) -> Self {
        Self {
            source,
            info: None,
            writer,
            control,
            rows: HashMap::new(),
            vehicles: Vec::new(),
            board: None,
            board_rows: HashMap::new(),
            board_station: None,
            departures: Vec::new(),
            sweeps: 0,
            load: Duration::ZERO,
            started: Instant::now(),
            last_sweep: None,
        }
    }

    /// Writes the board of the chosen station (`Control::set_station`) to
    /// `writer`, if the source knows departures.
    pub fn with_board(mut self, writer: GridWriter<BoardRow>) -> Self {
        self.board = Some(writer);
        self
    }

    /// Loads the source first; then a sweep whenever the warp's interval
    /// has passed. Returns false once the store is gone or the source
    /// failed, and the work is over.
    pub fn step(&mut self) -> bool {
        if self.writer.is_closed() {
            self.source.close();
            return false;
        }
        if self.info.is_none() {
            let started = Instant::now();
            match self.source.load() {
                Ok(info) => {
                    if info.clock == SourceClock::LiveOnly {
                        self.control
                            .jump_to(info.clock_start_ms.unwrap_or_else(real_now_ms));
                        self.control.set_warp(Warp::RealTime);
                    }
                    self.info = Some(Arc::new(info));
                }
                Err(error) => {
                    self.control.fail(error);
                    return false;
                }
            }
            self.load = started.elapsed();
            out!(
                "transit: source loaded in {:.0} ms",
                self.load.as_secs_f64() * 1e3
            );
        }
        self.source.poll(self.started.elapsed());
        let interval = self.control.warp().sweep_interval();
        // A step comes a little early or late: a sweep is due a tenth of
        // its interval before the interval is over.
        if let Some(last) = self.last_sweep
            && last.elapsed() < interval.mul_f32(0.9)
        {
            return true;
        }
        self.sweep_at(self.control.now());
        true
    }

    /// One sweep at `unix_ms`: the rows written, the frame published.
    pub fn sweep_at(&mut self, unix_ms: f64) -> SweepCounts {
        let started = Instant::now();
        self.vehicles.clear();
        self.source.vehicles(unix_ms, &mut self.vehicles);
        let compute = started.elapsed();

        let started = Instant::now();
        self.sweeps += 1;
        let sweep = self.sweeps;
        let mut counts = SweepCounts::default();
        let rows = &mut self.rows;
        let vehicles = &self.vehicles;
        self.writer.batch(|batch| {
            for vehicle in vehicles {
                let row = VehicleRow::new(vehicle, unix_ms);
                match rows.get_mut(&row.key) {
                    None => {
                        counts.started += 1;
                        let row = Arc::new(row);
                        batch.upsert(row.clone());
                        rows.insert(row.key, (row, sweep));
                    }
                    Some((previous, seen)) => {
                        *seen = sweep;
                        if **previous == row {
                            continue;
                        }
                        let departed = row.departed_since(previous);
                        let row = Arc::new(row);
                        if departed {
                            counts.departures += 1;
                            batch.upsert(row.clone());
                        } else {
                            batch.upsert_silent(row.clone());
                        }
                        *previous = row;
                    }
                }
                counts.written += 1;
            }
            rows.retain(|key, (_, seen)| {
                let current = *seen == sweep;
                if !current {
                    counts.finished += 1;
                    batch.remove(*key);
                }
                current
            });
        });
        self.sweep_board(unix_ms);
        let write = started.elapsed();
        self.publish(unix_ms, counts, compute, write);
        counts
    }

    /// The board of the chosen station at `unix_ms`: a departure new to it
    /// flashes, its countdown ticks silently, a departed one (or the whole
    /// board, for another station) fades out.
    fn sweep_board(&mut self, unix_ms: f64) {
        let Some(board) = &self.board else {
            return;
        };
        let has_board = self.info.as_ref().is_some_and(|info| info.has_board);
        let station = self.control.station().filter(|_| has_board);
        self.departures.clear();
        if let Some(station) = station {
            self.source
                .departures(station, unix_ms, BOARD_ROWS, &mut self.departures);
        }
        let changed_station = station != self.board_station;
        self.board_station = station;
        let rows = &mut self.board_rows;
        let departures = &self.departures;
        board.batch(|batch| {
            if changed_station {
                for key in rows.keys() {
                    batch.remove(*key);
                }
                rows.clear();
            }
            let mut seen = std::collections::HashSet::with_capacity(departures.len());
            for departure in departures {
                let row = BoardRow::new(departure, unix_ms);
                seen.insert(row.key);
                match rows.get_mut(&row.key) {
                    Some(previous) if **previous == row => {}
                    Some(previous) => {
                        let row = Arc::new(row);
                        batch.upsert_silent(row.clone());
                        *previous = row;
                    }
                    None => {
                        let row = Arc::new(row);
                        batch.upsert(row.clone());
                        rows.insert(row.key, row);
                    }
                }
            }
            rows.retain(|key, _| {
                let kept = seen.contains(key);
                if !kept {
                    batch.remove(*key);
                }
                kept
            });
        });
    }

    fn publish(&mut self, unix_ms: f64, counts: SweepCounts, compute: Duration, write: Duration) {
        let now = Instant::now();
        let rate = self.last_sweep.map_or(0, |last| {
            (f64::from(counts.written) / (now - last).as_secs_f64().max(1e-3)).round() as u64
        });
        self.last_sweep = Some(now);
        let mut by_mode = [0; Mode::COUNT];
        for vehicle in &self.vehicles {
            by_mode[vehicle.line.mode.ix()] += 1;
        }
        let selected_key = self.control.selected();
        let selected = selected_key.and_then(|key| {
            let vehicle = self.vehicles.iter().find(|vehicle| vehicle.key == key)?;
            Some(Selected {
                line: vehicle.line.clone(),
                headsign: vehicle.headsign.clone(),
                leg: vehicle.leg.clone(),
                at: vehicle.at,
                path: vehicle.path.clone(),
                leg_ix: vehicle.leg_ix,
                next_arrival_ms: vehicle.next_arrival_ms,
                live: vehicle.live,
            })
        });
        let frame = TransitFrame {
            sim_ms: unix_ms,
            sweeps: self.sweeps,
            under_way: self.vehicles.len() as u32,
            by_mode,
            trips_today: self.source.trips_on_day(unix_ms),
            started: counts.started,
            departures: counts.departures,
            finished: counts.finished,
            rate,
            compute,
            write,
            load: self.load,
            dots: self
                .vehicles
                .iter()
                .map(|vehicle| Dot {
                    key: vehicle.key,
                    at: vehicle.at,
                    line: vehicle.line.rank,
                    mode: vehicle.line.mode,
                })
                .collect(),
            selected,
            info: self.info.clone(),
            station: self.board_station,
            board: self.board_rows.len() as u32,
            feed: self.source.status(),
        };
        self.control.publish(frame);
    }
}

/// The modes under way, in the order the frame counts them.
pub fn mode_counts(frame: &TransitFrame) -> impl Iterator<Item = (Mode, u32)> + '_ {
    Mode::ALL
        .into_iter()
        .map(|mode| (mode, frame.by_mode[mode.ix()]))
        .filter(|(_, count)| *count > 0)
}

/// Runs `producer` until its store is gone, a step every
/// [`STEP_INTERVAL`]. Returns the task to keep while it runs: in the
/// browser it runs the producer (a live source's socket lives on the
/// page's one thread), natively it is done at once and the producer runs
/// on a thread of its own.
pub fn start(mut producer: Producer, cx: &App) -> Task<()> {
    #[cfg(not(target_family = "wasm"))]
    {
        let _ = cx;
        let spawned = std::thread::Builder::new()
            .name("transit-producer".into())
            .spawn(move || {
                while producer.step() {
                    std::thread::sleep(STEP_INTERVAL);
                }
            });
        if let Err(error) = spawned {
            err!("transit: no producer thread ({error})");
        }
        Task::ready(())
    }
    #[cfg(target_family = "wasm")]
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        while producer.step() {
            executor.timer(STEP_INTERVAL).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warp_rebases_the_clock() {
        let start = 1_800_000_000_000.;
        let control = Control::new(Some(start));
        let before = control.now();
        assert!((before - start).abs() < 1_000.);
        control.set_warp(Warp::Sixty);
        assert_eq!(control.warp(), Warp::Sixty);
        // Rebased: no jump at the switch.
        assert!((control.now() - before).abs() < 1_000.);
        control.jump_to(start + 3_600_000.);
        assert!((control.now() - start - 3_600_000.).abs() < 1_000.);
        assert_eq!(Warp::Sixty.next(), Warp::RealTime);
        assert_eq!(Warp::RealTime.sweep_interval(), Duration::from_secs(1));
    }

    #[test]
    fn test_selection_round_trips() {
        let control = Control::new(None);
        assert_eq!(control.selected(), None);
        control.select(Some(VehicleKey::new(0)));
        assert_eq!(control.selected(), Some(VehicleKey::new(0)));
        control.select(None);
        assert_eq!(control.selected(), None);
    }
}

/// The producer writing a real grid store, headless, on the test
/// timetable: rows appear as runs start and go as they end.
#[cfg(all(test, feature = "test-support"))]
mod store_tests {
    use gpui_kit::TestAppContext;
    use gpui_kit_grid::{GridStore, StoreSnapshot};

    use super::super::{
        schedule::TimetableSource,
        table,
        timetable::FIXTURE,
        zone::{self, Day},
    };
    use super::*;

    fn settle(cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
        }
    }

    fn at(clock: &str) -> f64 {
        let day = Day::from_ymd(2026, 11, 10);
        zone::parse_munich(&format!("{}T{clock}", day.iso()), day).expect("a time")
    }

    fn producer(cx: &mut TestAppContext) -> (Producer, GridStore<VehicleRow>, Arc<Control>) {
        let store = cx.update(|cx| GridStore::new(table::store_options(), cx));
        // Before the first trip: the first sweep writes nothing.
        let control = Control::new(Some(at("05:00:00")));
        let source = Box::new(TimetableSource::new(FIXTURE.as_bytes().to_vec()));
        let mut producer = Producer::new(source, store.writer(), control.clone());
        assert!(producer.step(), "loads and sweeps");
        (producer, store, control)
    }

    fn rows(
        store: &GridStore<VehicleRow>,
        cx: &mut TestAppContext,
    ) -> Arc<StoreSnapshot<VehicleRow>> {
        settle(cx);
        store.snapshot()
    }

    #[gpui_kit::test]
    fn test_rows_appear_and_disappear_with_their_runs(cx: &mut TestAppContext) {
        let (mut producer, store, control) = producer(cx);
        assert!(control.error().is_none());
        // 06:01: the first U9 trip (06:00) is under way.
        let counts = producer.sweep_at(at("06:01:00"));
        assert_eq!(counts.started, 1);
        assert_eq!(rows(&store, cx).len(), 1);
        // 06:02:15: it has left Alpha, so its leg changes and flashes.
        let counts = producer.sweep_at(at("06:02:15"));
        assert_eq!(
            (counts.started, counts.departures, counts.finished),
            (0, 1, 0)
        );
        let snapshot = rows(&store, cx);
        let (_, row) = snapshot.iter().next().expect("a row");
        assert_eq!(row.leg.as_ref(), "Beta → Gamma");
        // 06:11: the first trip has ended, the second one runs.
        let counts = producer.sweep_at(at("06:11:00"));
        assert_eq!((counts.started, counts.finished), (1, 1));
        let snapshot = rows(&store, cx);
        assert_eq!(snapshot.len(), 1);
        assert!(
            snapshot
                .iter()
                .all(|(_, row)| row.key.get() & 0xFFFF_FFFF == 1)
        );
        // 07:00: nothing runs.
        let counts = producer.sweep_at(at("07:00:00"));
        assert_eq!(counts.finished, 1);
        assert_eq!(rows(&store, cx).len(), 0);
        let frame = control.frame();
        assert_eq!(frame.under_way, 0);
        assert_eq!(frame.trips_today, Some(3));
        assert!(frame.info.is_some());
    }

    #[gpui_kit::test]
    fn test_board_lists_departures_and_drops_them_as_they_leave(cx: &mut TestAppContext) {
        let store = cx.update(|cx| GridStore::new(table::store_options(), cx));
        let board = cx.update(|cx| GridStore::new(super::super::board::store_options(), cx));
        let control = Control::new(Some(at("05:00:00")));
        let source = Box::new(TimetableSource::new(FIXTURE.as_bytes().to_vec()));
        let mut producer =
            Producer::new(source, store.writer(), control.clone()).with_board(board.writer());
        assert!(producer.step(), "loads and sweeps");
        // No station: no board.
        producer.sweep_at(at("05:55:00"));
        settle(cx);
        assert_eq!(board.snapshot().len(), 0);
        // Alpha: the 06:00 and the 06:10.
        control.set_station(Some(0));
        producer.sweep_at(at("05:55:00"));
        settle(cx);
        let snapshot = board.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(
            snapshot
                .iter()
                .all(|(_, row)| row.platform.as_ref() == "1" && row.next_stop.as_ref() == "Beta")
        );
        assert_eq!(control.frame().station, Some(0));
        assert_eq!(control.frame().board, 2);
        // The 06:00 has left.
        producer.sweep_at(at("06:00:30"));
        settle(cx);
        let snapshot = board.snapshot();
        assert_eq!(snapshot.len(), 1);
        let (_, row) = snapshot.iter().next().expect("a row");
        assert_eq!(row.departs.seconds(), 570);
        // Another station replaces the board; none clears it.
        control.set_station(Some(1));
        producer.sweep_at(at("06:00:30"));
        settle(cx);
        assert_eq!(board.snapshot().len(), 2);
        control.set_station(None);
        producer.sweep_at(at("06:00:40"));
        settle(cx);
        assert_eq!(board.snapshot().len(), 0);
    }

    #[gpui_kit::test]
    fn test_selected_vehicle_reaches_the_frame(cx: &mut TestAppContext) {
        let (mut producer, _store, control) = producer(cx);
        producer.sweep_at(at("06:01:00"));
        let key = control.frame().dots[0].key;
        control.select(Some(key));
        producer.sweep_at(at("06:01:30"));
        let frame = control.frame();
        let selected = frame.selected.as_ref().expect("the selected vehicle");
        assert_eq!(selected.headsign.as_ref(), "Gamma");
        assert_eq!(selected.path.len(), 3);
        assert_eq!(frame.by_mode[Mode::UBahn.ix()], 1);
    }
}
