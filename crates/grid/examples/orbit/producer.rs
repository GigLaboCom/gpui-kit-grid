//! The producer: every second it propagates every object to the clock's
//! time, writes one row per object and the tree's roll-ups to the grid
//! store, detects rises and sets, and publishes what the sky plot, the
//! detail and the status bar show (`SkyFrame`). Natively it runs on a
//! thread of its own; in the browser on the executor's timer, its sweep
//! spread over several steps (`chunks`). No GPUI types.
//!
//! `Sky` holds the model's state between sweeps and leaves its writes in
//! `Out`; `Producer` adds the store's writer, the clock and the timing, so
//! tests sweep `Sky` at fixed times.

use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use gpui_kit_grid::{GridWriter, PinnedEdge};
use parking_lot::Mutex;
// `std::time` natively; the browser's clock in wasm, where
// `std::time::{Instant, SystemTime}::now` panic.
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use super::{
    rows::{Body, RollUp, Sighting, SkyKey, SkyRow},
    sky::{Catalog, Constellation, Elements, Epoch, Observer, PassSearch, Position, find_pass},
};

/// Objects pinned on top of the sky table, if the data has them: the ISS
/// (Zarya) and Tiangong (Tianhe).
pub const PINNED: [u32; 2] = [25544, 48274];
/// The object selected at start.
pub const FIRST_SELECTION: u32 = 25544;
/// One sweep of every object a second.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(1);
/// A pass search that found no pass is repeated after this much simulated
/// time.
const NO_PASS_RETRY_MS: f64 = 10. * 60_000.;

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
}

/// Unix ms of the real UTC clock.
pub fn real_now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |since| since.as_secs_f64() * 1e3)
}

/// The simulated UTC clock: the real one, from a fixed start if one is
/// given, running `warp` times as fast.
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
}

/// An object above the horizon, for the sky plot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dot {
    pub norad: u32,
    pub azimuth: f32,
    pub elevation: f32,
    pub sunlit: bool,
    /// A crewed station: drawn larger.
    pub station: bool,
}

/// The selected object: what the detail panel shows.
#[derive(Clone, Debug)]
pub struct Detail {
    pub norad: u32,
    pub name: Arc<str>,
    pub designator: Arc<str>,
    pub constellation: Constellation,
    pub shell: Arc<str>,
    pub elements: Elements,
    /// `None` where SGP4 fails at this time.
    pub position: Option<Position>,
    pub pass: PassSearch,
    /// Passes skipped before the one shown (the Next pass key).
    pub pass_skip: u32,
}

/// What one sweep publishes for the panels beside the table.
#[derive(Clone, Debug, Default)]
pub struct SkyFrame {
    /// The simulated time of the sweep, Unix ms; 0 before the first.
    pub sim_ms: f64,
    pub objects: u32,
    pub up: u32,
    /// Objects SGP4 could not place at this time.
    pub failed: u32,
    pub sweeps: u64,
    /// Rows written a second: the sweep's rows over the real time since
    /// the sweep before it.
    pub rate: u64,
    /// Propagation and the look angles of every object, over the sweep's
    /// steps.
    pub propagate: Duration,
    /// Building the rows and handing them to the store's writer.
    pub write: Duration,
    /// Parsing the data and initialising SGP4, once.
    pub load: Duration,
    pub rises: u32,
    pub sets: u32,
    pub sun_elevation: f64,
    pub median_epoch_ms: f64,
    pub dots: Vec<Dot>,
    pub detail: Option<Detail>,
}

/// What the window and the producer share: the clock, the selection and
/// the last frame.
pub struct Control {
    clock: Mutex<SimClock>,
    /// The selected NORAD number + 1; 0 for none.
    selected: AtomicU64,
    pass_skip: AtomicU32,
    frame: Mutex<Arc<SkyFrame>>,
    version: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Control {
    /// A clock from `start_ms` (Unix ms; the real time if `None`), with the
    /// ISS selected.
    pub fn new(start_ms: Option<f64>) -> Arc<Self> {
        Arc::new(Self {
            clock: Mutex::new(SimClock::new(start_ms)),
            selected: AtomicU64::new(u64::from(FIRST_SELECTION) + 1),
            pass_skip: AtomicU32::new(0),
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

    pub fn selected(&self) -> Option<u32> {
        let value = self.selected.load(Ordering::Relaxed);
        (value > 0).then(|| (value - 1) as u32)
    }

    /// Selects an object for the detail and the sky plot, its next pass
    /// first.
    pub fn select(&self, norad: Option<u32>) {
        let value = norad.map_or(0, |norad| u64::from(norad) + 1);
        if self.selected.swap(value, Ordering::Relaxed) != value {
            self.pass_skip.store(0, Ordering::Relaxed);
        }
    }

    pub fn pass_skip(&self) -> u32 {
        self.pass_skip.load(Ordering::Relaxed)
    }

    /// The detail shows the pass after the one it shows.
    pub fn next_pass(&self) {
        self.pass_skip.fetch_add(1, Ordering::Relaxed);
    }

    /// Back to the first pass, as after a search that found none.
    fn reset_pass(&self) {
        self.pass_skip.store(0, Ordering::Relaxed);
    }

    pub fn frame(&self) -> Arc<SkyFrame> {
        self.frame.lock().clone()
    }

    /// Bumped by every frame published.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Why the data could not be used, if it could not.
    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }

    fn publish(&self, frame: SkyFrame) {
        *self.frame.lock() = Arc::new(frame);
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    fn fail(&self, error: String) {
        *self.error.lock() = Some(error);
        self.version.fetch_add(1, Ordering::Relaxed);
    }
}

/// A branch of the tree: a constellation, or one of its shells.
struct Branch {
    key: SkyKey,
    parent: Option<usize>,
    name: Arc<str>,
    constellation: Constellation,
    shell: Arc<str>,
}

/// What a sweep writes.
#[derive(Default)]
pub struct Out {
    /// Objects that moved, written without highlight.
    pub rows: Vec<Arc<SkyRow>>,
    /// Objects that rose or set since the last sweep, written with
    /// highlight: the row flashes.
    pub flashes: Vec<Arc<SkyRow>>,
    /// Objects SGP4 lost.
    pub removed: Vec<SkyKey>,
    /// Branches whose roll-up changed.
    pub branches: Vec<Arc<SkyRow>>,
    /// The pinned band, once per sweep.
    pub pinned: Option<Vec<Arc<SkyRow>>>,
    pub rises: u32,
    pub sets: u32,
}

impl Out {
    fn clear(&mut self) {
        self.rows.clear();
        self.flashes.clear();
        self.removed.clear();
        self.branches.clear();
        self.pinned = None;
        self.rises = 0;
        self.sets = 0;
    }
}

/// The model between sweeps: every object's last row and the tree.
pub struct Sky {
    catalog: Catalog,
    observer: Observer,
    /// Per object: the row written last.
    rows: Vec<Option<Arc<SkyRow>>>,
    /// Per object: the index of its shell in `branches`.
    shell_of: Vec<usize>,
    branches: Vec<Branch>,
    /// Per branch: the roll-up written last.
    written: Vec<Option<Arc<SkyRow>>>,
    /// Indices of the pinned objects present.
    pinned: Vec<usize>,
}

impl Sky {
    pub fn new(catalog: Catalog, observer: Observer) -> Self {
        let mut branches: Vec<Branch> = Vec::new();
        let mut index: HashMap<SkyKey, usize> = HashMap::new();
        let mut find = |branch: Branch| {
            *index.entry(branch.key).or_insert_with(|| {
                branches.push(branch);
                branches.len() - 1
            })
        };
        let mut shell_of = Vec::with_capacity(catalog.len());
        for satellite in catalog.satellites() {
            let constellation = satellite.constellation();
            let parent = find(Branch {
                key: SkyKey::Constellation(constellation),
                parent: None,
                name: constellation.label().into(),
                constellation,
                shell: "".into(),
            });
            let shell = find(Branch {
                key: SkyKey::Shell(constellation, satellite.shell()),
                parent: Some(parent),
                name: satellite.shell_label().clone(),
                constellation,
                shell: satellite.shell_label().clone(),
            });
            shell_of.push(shell);
        }
        let pinned = PINNED
            .iter()
            .filter_map(|norad| catalog.find(*norad))
            .collect();
        Self {
            rows: vec![None; catalog.len()],
            written: (0..branches.len()).map(|_| None).collect(),
            catalog,
            observer,
            shell_of,
            branches,
            pinned,
        }
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// The objects in `range` at `epoch`: a row each, a flash for each rise
    /// and set since its last sweep (none on its first), a removal for
    /// each object SGP4 lost.
    pub fn sweep(&mut self, range: Range<usize>, epoch: &Epoch, out: &mut Out) {
        for ix in range {
            let satellite = &self.catalog.satellites()[ix];
            let key = SkyKey::Object(satellite.norad());
            let Some(position) = satellite.position(epoch, &self.observer) else {
                if self.rows[ix].take().is_some() {
                    out.removed.push(key);
                }
                continue;
            };
            let sighting = Sighting {
                norad: satellite.norad(),
                azimuth: position.look.azimuth,
                elevation: position.look.elevation,
                range: position.look.range,
                range_rate: position.look.range_rate,
                altitude: position.altitude,
                speed: position.speed,
                sunlit: position.sunlit,
            };
            let row = Arc::new(SkyRow {
                key,
                parent: Some(self.branches[self.shell_of[ix]].key),
                name: satellite.name().clone(),
                constellation: satellite.constellation(),
                shell: satellite.shell_label().clone(),
                body: Body::Object(sighting),
            });
            let was_up = self.rows[ix].as_ref().map(|row| row.is_up());
            match was_up {
                Some(was_up) if was_up != sighting.is_up() => {
                    if sighting.is_up() {
                        out.rises += 1;
                    } else {
                        out.sets += 1;
                    }
                    out.flashes.push(row.clone());
                }
                _ => out.rows.push(row.clone()),
            }
            self.rows[ix] = Some(row);
        }
    }

    /// Every branch's roll-up over its objects' last rows; those that
    /// changed go to `out`.
    pub fn roll_up(&mut self, out: &mut Out) {
        let mut roll_ups = vec![RollUp::default(); self.branches.len()];
        for (ix, row) in self.rows.iter().enumerate() {
            let Some(sighting) = row.as_ref().and_then(|row| row.sighting()) else {
                continue;
            };
            let shell = self.shell_of[ix];
            for branch in [Some(shell), self.branches[shell].parent]
                .into_iter()
                .flatten()
            {
                let roll_up = &mut roll_ups[branch];
                roll_up.objects += 1;
                roll_up.highest = Some(roll_up.highest.map_or(sighting.elevation, |highest| {
                    highest.max(sighting.elevation)
                }));
                if sighting.is_up() {
                    roll_up.up += 1;
                    roll_up.nearest = Some(
                        roll_up
                            .nearest
                            .map_or(sighting.range, |nearest| nearest.min(sighting.range)),
                    );
                }
            }
        }
        for (ix, roll_up) in roll_ups.into_iter().enumerate() {
            if self.written[ix]
                .as_ref()
                .is_some_and(|row| row.roll_up() == Some(&roll_up))
            {
                continue;
            }
            let branch = &self.branches[ix];
            let row = Arc::new(SkyRow {
                key: branch.key,
                parent: branch.parent.map(|parent| self.branches[parent].key),
                name: branch.name.clone(),
                constellation: branch.constellation,
                shell: branch.shell.clone(),
                body: Body::Branch(roll_up),
            });
            self.written[ix] = Some(row.clone());
            out.branches.push(row);
        }
    }

    /// The pinned objects' last rows.
    pub fn pinned_rows(&self) -> Vec<Arc<SkyRow>> {
        self.pinned
            .iter()
            .filter_map(|ix| self.rows[*ix].clone())
            .collect()
    }

    /// Every object above the horizon, for the sky plot.
    pub fn dots(&self) -> Vec<Dot> {
        self.rows
            .iter()
            .flatten()
            .filter_map(|row| {
                let sighting = row.sighting()?;
                sighting.is_up().then(|| Dot {
                    norad: sighting.norad,
                    azimuth: sighting.azimuth as f32,
                    elevation: sighting.elevation as f32,
                    sunlit: sighting.sunlit,
                    station: row.constellation == Constellation::Stations,
                })
            })
            .collect()
    }

    /// Objects with a row, and those above the horizon.
    pub fn counts(&self) -> (u32, u32) {
        self.rows
            .iter()
            .flatten()
            .fold((0, 0), |(objects, up), row| {
                (objects + 1, up + u32::from(row.is_up()))
            })
    }
}

/// The pass shown for the selected object, kept until it is over.
struct PassCache {
    norad: u32,
    skip: u32,
    search: PassSearch,
    /// Simulated time after which it is searched again.
    valid_until: f64,
}

/// The data, its parsed model, the store's writer and the clock: one step
/// is one chunk of a sweep.
pub struct Producer {
    data: Option<Vec<u8>>,
    sky: Option<Sky>,
    observer: Observer,
    writer: GridWriter<SkyRow>,
    control: Arc<Control>,
    chunks: usize,
    /// The next chunk of the sweep.
    chunk: usize,
    out: Out,
    pass: Option<PassCache>,
    sweeps: u64,
    load: Duration,
    propagate: Duration,
    write: Duration,
    rises: u32,
    sets: u32,
    /// The instant of the sweep under way.
    epoch: Epoch,
    /// Rows written in this sweep, and when the last one ended.
    written: u64,
    last_sweep: Option<Instant>,
}

impl Producer {
    /// A producer of `data` (OMM JSON) for `observer`, writing `writer`,
    /// its sweep spread over `chunks` steps.
    pub fn new(
        data: Vec<u8>,
        observer: Observer,
        writer: GridWriter<SkyRow>,
        control: Arc<Control>,
        chunks: usize,
    ) -> Self {
        Self {
            data: Some(data),
            sky: None,
            observer,
            writer,
            control,
            chunks: chunks.max(1),
            chunk: 0,
            out: Out::default(),
            pass: None,
            sweeps: 0,
            load: Duration::ZERO,
            propagate: Duration::ZERO,
            write: Duration::ZERO,
            rises: 0,
            sets: 0,
            epoch: Epoch::at(0.),
            written: 0,
            last_sweep: None,
        }
    }

    /// The time between two steps: a sweep a second.
    pub fn interval(&self) -> Duration {
        SWEEP_INTERVAL / self.chunks as u32
    }

    /// One chunk of a sweep: parses the data first. The rows of a sweep are
    /// written at its last step, together; the first sweep runs whole.
    /// Returns false once the store is gone or the data cannot be used,
    /// and the work is over.
    pub fn step(&mut self) -> bool {
        if self.writer.is_closed() {
            return false;
        }
        if self.sky.is_none() {
            let started = Instant::now();
            let data = self.data.take().unwrap_or_default();
            match Catalog::parse(&data) {
                Ok(catalog) => {
                    out!(
                        "orbit: {} objects, {} without SGP4 constants, loaded in {:.0} ms",
                        catalog.len(),
                        catalog.skipped(),
                        started.elapsed().as_secs_f64() * 1e3
                    );
                    self.sky = Some(Sky::new(catalog, self.observer));
                }
                Err(error) => {
                    self.control.fail(error);
                    return false;
                }
            }
            self.load = started.elapsed();
        }
        let Some(sky) = self.sky.as_mut() else {
            return false;
        };
        // A sweep places every object at one instant, the time of its
        // first step.
        if self.chunk == 0 {
            self.epoch = Epoch::at(self.control.now());
            self.out.clear();
        }
        let epoch = self.epoch;
        let count = sky.catalog().len();
        // The first sweep runs whole, so the first rows come at once.
        let chunks = if self.sweeps == 0 { 1 } else { self.chunks };
        let per_chunk = count.div_ceil(chunks);
        let range = (self.chunk * per_chunk).min(count)..((self.chunk + 1) * per_chunk).min(count);
        let last = self.chunk + 1 >= chunks;
        self.chunk = if last { 0 } else { self.chunk + 1 };

        let started = Instant::now();
        sky.sweep(range, &epoch, &mut self.out);
        self.propagate += started.elapsed();
        if !last {
            return true;
        }
        // Written once a sweep: one rebuild of the table, not one a step.
        let started = Instant::now();
        sky.roll_up(&mut self.out);
        self.out.pinned = Some(sky.pinned_rows());
        let out = &mut self.out;
        let written = out.rows.len() + out.flashes.len() + out.branches.len();
        self.writer.batch(|batch| {
            for row in out.rows.drain(..) {
                batch.upsert_silent(row);
            }
            for row in out.flashes.drain(..) {
                batch.upsert(row);
            }
            for row in out.branches.drain(..) {
                batch.upsert_silent(row);
            }
            for key in out.removed.drain(..) {
                batch.remove(key);
            }
            if let Some(pinned) = out.pinned.take() {
                batch.set_pinned_rows(PinnedEdge::Top, pinned);
            }
        });
        self.write += started.elapsed();
        self.written += written as u64;
        self.rises += self.out.rises;
        self.sets += self.out.sets;
        self.finish_sweep(&epoch);
        true
    }

    /// Publishes the frame of a finished sweep and starts the next one.
    fn finish_sweep(&mut self, epoch: &Epoch) {
        self.sweeps += 1;
        let now = Instant::now();
        let rate = self.last_sweep.map_or(0, |last| {
            (self.written as f64 / (now - last).as_secs_f64().max(1e-3)).round() as u64
        });
        self.last_sweep = Some(now);
        let detail = self.detail(epoch);
        let Some(sky) = self.sky.as_ref() else {
            return;
        };
        let (objects, up) = sky.counts();
        let frame = SkyFrame {
            sim_ms: epoch.unix_ms(),
            objects,
            up,
            failed: sky.catalog().len() as u32 - objects,
            sweeps: self.sweeps,
            rate,
            propagate: self.propagate,
            write: self.write,
            load: self.load,
            rises: self.rises,
            sets: self.sets,
            sun_elevation: epoch.sun_elevation(sky.observer()),
            median_epoch_ms: sky.catalog().median_epoch_ms(),
            dots: sky.dots(),
            detail,
        };
        self.control.publish(frame);
        self.propagate = Duration::ZERO;
        self.write = Duration::ZERO;
        self.written = 0;
        self.rises = 0;
        self.sets = 0;
    }

    /// The selected object's detail, its pass searched again when the
    /// selection or the pass asked for changed, or the pass is over.
    fn detail(&mut self, epoch: &Epoch) -> Option<Detail> {
        let sky = self.sky.as_ref()?;
        let norad = self.control.selected()?;
        let ix = sky.catalog().find(norad)?;
        let satellite = &sky.catalog().satellites()[ix];
        let now = epoch.unix_ms();
        let skip = self.control.pass_skip();
        let current = self
            .pass
            .as_ref()
            .is_some_and(|pass| pass.norad == norad && pass.skip == skip && now < pass.valid_until);
        if !current {
            let search = find_pass(satellite, sky.observer(), now, skip);
            let valid_until = match &search {
                PassSearch::Pass(pass) => pass.set.unix_ms,
                PassSearch::AlwaysUp | PassSearch::NoPass => now + NO_PASS_RETRY_MS,
            };
            // Past the last pass in the window: back to the first.
            if skip > 0 && search == PassSearch::NoPass {
                self.control.reset_pass();
            }
            self.pass = Some(PassCache {
                norad,
                skip,
                search,
                valid_until,
            });
        }
        let pass = self.pass.as_ref()?;
        Some(Detail {
            norad,
            name: satellite.name().clone(),
            designator: satellite.designator().clone(),
            constellation: satellite.constellation(),
            shell: satellite.shell_label().clone(),
            elements: satellite.elements().clone(),
            position: satellite.position(epoch, sky.observer()),
            pass: pass.search.clone(),
            pass_skip: pass.skip,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::sky::{FIXTURE, parse_utc};
    use super::*;

    fn t0() -> f64 {
        parse_utc("2026-11-06T00:00:00Z").expect("a time")
    }

    /// The fixture's sky for an observer under the ISS at `t0` + 1 h.
    fn sky_under_the_iss() -> (Sky, f64) {
        let catalog = Catalog::parse(FIXTURE.as_bytes()).expect("the fixture");
        let overhead = t0() + 3_600_000.;
        let position = catalog.satellites()[0]
            .position(&Epoch::at(overhead), &Observer::greenwich())
            .expect("a position");
        let observer = Observer::new(position.latitude, position.longitude, 0.);
        (Sky::new(catalog, observer), overhead)
    }

    fn sweep(sky: &mut Sky, unix_ms: f64) -> Out {
        let mut out = Out::default();
        let count = sky.catalog().len();
        sky.sweep(0..count, &Epoch::at(unix_ms), &mut out);
        sky.roll_up(&mut out);
        out
    }

    fn iss(rows: &[Arc<SkyRow>]) -> Option<&Arc<SkyRow>> {
        rows.iter().find(|row| row.key == SkyKey::Object(25544))
    }

    #[test]
    fn test_rise_and_set_flash_once() {
        let (mut sky, overhead) = sky_under_the_iss();
        let minute = 60_000.;
        // The first sweep only learns where everything is.
        let first = sweep(&mut sky, overhead - 15. * minute);
        assert!(first.flashes.is_empty());
        assert!(!iss(&first.rows).expect("the ISS").is_up());

        let rise = sweep(&mut sky, overhead);
        let row = iss(&rise.flashes).expect("the ISS rose");
        assert!(row.sighting().expect("an object").elevation > 89.);
        assert!(rise.rises >= 1);
        assert!(iss(&rise.rows).is_none(), "a flash is not written twice");

        // Still up a second later: no flash.
        let still = sweep(&mut sky, overhead + 1_000.);
        assert!(iss(&still.flashes).is_none());

        let set = sweep(&mut sky, overhead + 15. * minute);
        let row = iss(&set.flashes).expect("the ISS set");
        assert!(!row.is_up());
        assert!(set.sets >= 1);
    }

    #[test]
    fn test_tree_of_constellations_and_shells() {
        let (mut sky, overhead) = sky_under_the_iss();
        let out = sweep(&mut sky, overhead);
        let rows: Vec<&Arc<SkyRow>> = out.rows.iter().chain(&out.flashes).collect();
        assert_eq!(rows.len(), 4);
        // Stations, Starlink and GPS, each with one shell here but the
        // stations, which have two.
        let constellations = out
            .branches
            .iter()
            .filter(|row| matches!(row.key, SkyKey::Constellation(_)))
            .count();
        assert_eq!(constellations, 3);
        for row in rows {
            let shell = out
                .branches
                .iter()
                .find(|branch| Some(branch.key) == row.parent)
                .expect("an object hangs from its shell");
            let constellation = out
                .branches
                .iter()
                .find(|branch| Some(branch.key) == shell.parent)
                .expect("a shell hangs from its constellation");
            assert_eq!(constellation.parent, None);
            assert_eq!(shell.constellation, row.constellation);
        }
        // A second sweep at the same time changes no roll-up.
        assert!(sweep(&mut sky, overhead).branches.is_empty());
        assert_eq!(sky.pinned_rows().len(), 2);
    }

    #[test]
    fn test_warp_rebases_the_clock() {
        let control = Control::new(Some(t0()));
        let before = control.now();
        assert!((before - t0()).abs() < 1_000.);
        control.set_warp(Warp::Sixty);
        assert_eq!(control.warp(), Warp::Sixty);
        // Rebased: no jump at the switch.
        assert!((control.now() - before).abs() < 1_000.);
        assert_eq!(Warp::Sixty.next(), Warp::RealTime);
    }

    #[test]
    fn test_selection_resets_the_pass() {
        let control = Control::new(None);
        assert_eq!(control.selected(), Some(FIRST_SELECTION));
        control.next_pass();
        control.next_pass();
        assert_eq!(control.pass_skip(), 2);
        control.select(Some(48274));
        assert_eq!(control.pass_skip(), 0);
        control.select(None);
        assert_eq!(control.selected(), None);
    }
}

/// The producer writing a real grid store, headless, on the fixture.
#[cfg(all(test, feature = "test-support"))]
mod store_tests {
    use gpui_kit::TestAppContext;
    use gpui_kit_grid::{GridStore, StoreSnapshot};

    use super::super::{sky::FIXTURE, table};
    use super::*;

    fn settle(cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
        }
    }

    /// A store fed by a producer of the fixture, `sweeps` sweeps written.
    fn run(sweeps: usize, chunks: usize, cx: &mut TestAppContext) -> Arc<StoreSnapshot<SkyRow>> {
        let store = cx.update(|cx| GridStore::new(table::store_options(), cx));
        let control = Control::new(Some(
            super::super::sky::parse_utc("2026-11-06T00:00:00").unwrap(),
        ));
        let mut producer = Producer::new(
            FIXTURE.as_bytes().to_vec(),
            Observer::greenwich(),
            store.writer(),
            control.clone(),
            chunks,
        );
        // The first sweep in one step, the others in `chunks` each.
        for _ in 0..1 + (sweeps - 1) * chunks {
            assert!(producer.step());
        }
        settle(cx);
        assert_eq!(control.frame().sweeps, sweeps as u64);
        store.snapshot()
    }

    #[gpui_kit::test]
    fn test_rows_collapse_by_key(cx: &mut TestAppContext) {
        let rows = run(5, 2, cx);
        // Four objects, three constellations and four shells (the two
        // stations orbit at different heights), however many sweeps.
        let objects = rows.iter().filter(|(_, row)| row.is_object()).count();
        assert_eq!(objects, 4);
        assert_eq!(rows.len(), 4 + 3 + 4);
        for norad in [25544, 48274, 44714, 24876] {
            assert!(rows.get(&SkyKey::Object(norad)).is_some(), "{norad}");
        }
        let pinned = rows.pinned_rows(PinnedEdge::Top);
        let keys: Vec<SkyKey> = pinned.iter().map(|row| row.key).collect();
        assert_eq!(keys, [SkyKey::Object(25544), SkyKey::Object(48274)]);
    }

    #[gpui_kit::test]
    fn test_roll_ups_match_their_children(cx: &mut TestAppContext) {
        let rows = run(2, 1, cx);
        let mut checked = 0;
        for (_, branch) in rows.iter() {
            let Some(roll_up) = branch.roll_up() else {
                continue;
            };
            let children: Vec<&Arc<SkyRow>> = rows
                .iter()
                .map(|(_, row)| row)
                .filter(|row| row.parent == Some(branch.key))
                .collect();
            assert!(!children.is_empty(), "{}", branch.name);
            let (mut objects, mut up) = (0, 0);
            let mut highest = f64::NEG_INFINITY;
            for child in &children {
                match &child.body {
                    Body::Object(sighting) => {
                        objects += 1;
                        up += u32::from(sighting.is_up());
                        highest = highest.max(sighting.elevation);
                    }
                    Body::Branch(child) => {
                        objects += child.objects;
                        up += child.up;
                        highest = highest.max(child.highest.unwrap_or(f64::NEG_INFINITY));
                    }
                }
            }
            assert_eq!(roll_up.objects, objects, "{}", branch.name);
            assert_eq!(roll_up.up, up, "{}", branch.name);
            assert_eq!(roll_up.highest, Some(highest), "{}", branch.name);
            checked += 1;
        }
        assert_eq!(checked, 7);
    }
}
