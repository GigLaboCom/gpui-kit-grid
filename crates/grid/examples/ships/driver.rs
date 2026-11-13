//! The driver: every few tens of milliseconds it polls the vessel source,
//! applies what arrived to the fleet, writes the changes to the grid store
//! in one batch, and four times a second publishes what the map, the rate
//! chart, the vessel card and the status bar show ([`FleetFrame`]).
//! Natively it runs on a thread of its own; in the browser on the page's
//! timer, between frames. No GPUI types but the task it runs in.

use std::{
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
// `std::time::Instant::now` panics. Durations only.
use web_time::Instant;

use super::{
    ais::Metadata,
    fleet::{Dot, Fleet, FleetCounts, RateMeter, StaleRule, Trail, Writes},
    rows::VesselRow,
    source::{Connection, MaybeSend, Recording, ReplaySource, SourceEvent, VesselSource},
    speed::SpeedRule,
};

/// How often the driver polls the source.
pub const TICK: Duration = Duration::from_millis(50);
/// How often it publishes a frame for the panels.
const PUBLISH_EVERY: Duration = Duration::from_millis(250);
/// How often it looks for vessels gone quiet.
const SWEEP_EVERY: Duration = Duration::from_secs(5);
/// The rate the status bar shows: messages a second over this many
/// seconds.
const RATE_SECONDS: usize = 5;

/// The recording to fall back to when the live source cannot reach
/// Digitraffic: natively the default recording file, read at once; in the
/// browser the page's, which the page fetches only when asked.
pub trait FallbackRecording: MaybeSend {
    /// The recording, or why there is none; `None` while it is on its way
    /// (asked again on the next tick).
    fn recording(&mut self) -> Option<Result<Recording, String>>;
}

/// A [`FallbackRecording`], boxed.
pub type Fallback = Box<dyn FallbackRecording>;

/// Where the events go besides the fleet: natively a recording file.
pub trait EventSink: MaybeSend {
    fn record(&mut self, events: &[SourceEvent], clock_ms: f64);
}

/// The selected vessel, for its card and the map.
#[derive(Clone, Debug)]
pub struct VesselDetail {
    pub row: Arc<VesselRow>,
    pub metadata: Option<Arc<Metadata>>,
    pub trail: Vec<(f32, f32)>,
    pub seen_ms: f64,
}

/// What one publish shows beside the table.
#[derive(Clone, Debug)]
pub struct FleetFrame {
    /// The source's clock, Unix ms.
    pub clock_ms: f64,
    pub counts: FleetCounts,
    /// Messages a second, recently.
    pub rate: f32,
    /// Locations and metadata a second, oldest first.
    pub history: Vec<(u32, u32)>,
    pub messages: u64,
    pub dots: Vec<Dot>,
    pub trails: Vec<Trail>,
    pub selected: Option<VesselDetail>,
    pub connection: Connection,
    /// Why the source was replaced by a recording, if it was.
    pub notice: Option<Arc<str>>,
    /// Applying the last tick's events and writing them.
    pub apply: Duration,
    /// When the first rows were written, since the driver started.
    pub first_rows: Option<Duration>,
}

impl Default for FleetFrame {
    fn default() -> Self {
        Self {
            clock_ms: 0.,
            counts: FleetCounts::default(),
            rate: 0.,
            history: Vec::new(),
            messages: 0,
            dots: Vec::new(),
            trails: Vec::new(),
            selected: None,
            connection: Connection::Connecting,
            notice: None,
            apply: Duration::ZERO,
            first_rows: None,
        }
    }
}

/// What the view and the driver share: the last frame and the selection.
#[derive(Default)]
pub struct Control {
    frame: Mutex<Arc<FleetFrame>>,
    version: AtomicU64,
    /// The selected MMSI + 1; 0 for none.
    selected: AtomicU64,
}

impl Control {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    pub fn frame(&self) -> Arc<FleetFrame> {
        self.frame.lock().clone()
    }

    /// Bumped by every frame published.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    pub fn selected(&self) -> Option<u32> {
        let value = self.selected.load(Ordering::Relaxed);
        (value > 0).then(|| (value - 1) as u32)
    }

    pub fn select(&self, mmsi: Option<u32>) {
        let value = mmsi.map_or(0, |mmsi| u64::from(mmsi) + 1);
        self.selected.store(value, Ordering::Relaxed);
    }

    fn publish(&self, frame: FleetFrame) {
        *self.frame.lock() = Arc::new(frame);
        self.version.fetch_add(1, Ordering::Relaxed);
    }
}

/// The source, the fleet and the store's writer.
pub struct Driver {
    source: Box<dyn VesselSource>,
    fallback: Option<Fallback>,
    sink: Option<Box<dyn EventSink>>,
    fleet: Fleet,
    writer: GridWriter<VesselRow>,
    control: Arc<Control>,
    rate: RateMeter,
    events: Vec<SourceEvent>,
    writes: Writes,
    started: Instant,
    last_publish: Option<Duration>,
    last_sweep: Duration,
    messages: u64,
    apply: Duration,
    first_rows: Option<Duration>,
    notice: Option<Arc<str>>,
}

impl Driver {
    pub fn new(
        source: Box<dyn VesselSource>,
        writer: GridWriter<VesselRow>,
        control: Arc<Control>,
    ) -> Self {
        Self {
            source,
            fallback: None,
            sink: None,
            fleet: Fleet::new(StaleRule::default()),
            writer,
            control,
            rate: RateMeter::default(),
            events: Vec::new(),
            writes: Writes::default(),
            started: Instant::now(),
            last_publish: None,
            last_sweep: Duration::ZERO,
            messages: 0,
            apply: Duration::ZERO,
            first_rows: None,
            notice: None,
        }
    }

    /// Replays `fallback` if the source turns out unreachable.
    pub fn with_fallback(mut self, fallback: Option<Fallback>) -> Self {
        self.fallback = fallback;
        self
    }

    /// Believes the speeds `speeds` says are plausible (`speed.rs`).
    pub fn with_speed_rule(mut self, speeds: Arc<dyn SpeedRule>) -> Self {
        self.fleet = std::mem::take(&mut self.fleet).with_speed_rule(speeds);
        self
    }

    /// Hands every event to `sink` too.
    pub fn with_sink(mut self, sink: Option<Box<dyn EventSink>>) -> Self {
        self.sink = sink;
        self
    }

    /// Says why the source is what it is (a replay instead of live).
    pub fn with_notice(mut self, notice: Option<Arc<str>>) -> Self {
        self.notice = notice;
        self
    }

    /// One tick. Returns false once the store is gone and the work is over.
    pub fn step(&mut self) -> bool {
        if self.writer.is_closed() {
            self.source.close(self.started.elapsed());
            return false;
        }
        let now = self.started.elapsed();
        self.events.clear();
        self.source.poll(now, &mut self.events);
        let applied = Instant::now();
        let clock_ms = self.source.clock_ms(now);
        if let Some(sink) = self.sink.as_mut()
            && !self.events.is_empty()
        {
            sink.record(&self.events, clock_ms);
        }
        for event in &self.events {
            self.rate.count(event, now);
            self.messages += u64::from(!event.snapshot);
            self.fleet.apply(event, clock_ms, &mut self.writes);
        }
        self.rate.advance(now);
        if now >= self.last_sweep + SWEEP_EVERY {
            self.last_sweep = now;
            self.fleet.sweep(clock_ms, &mut self.writes);
        }
        if !self.writes.is_empty() {
            let writes = &mut self.writes;
            self.writer.batch(|batch| {
                for row in writes.upserts.drain(..) {
                    batch.upsert(row);
                }
                for row in writes.silent.drain(..) {
                    batch.upsert_silent(row);
                }
                for mmsi in writes.removed.drain(..) {
                    batch.remove(mmsi);
                }
            });
            self.first_rows
                .get_or_insert_with(|| self.started.elapsed());
        }
        if !self.events.is_empty() {
            self.apply = applied.elapsed();
        }
        self.fall_back(now);
        if self
            .last_publish
            .is_none_or(|last| now >= last + PUBLISH_EVERY)
        {
            self.last_publish = Some(now);
            self.publish(clock_ms);
        }
        true
    }

    /// Swaps an unreachable live source for the recording, once.
    fn fall_back(&mut self, now: Duration) {
        let Connection::Unreachable { reason } = self.source.connection() else {
            return;
        };
        let Some(recording) = self
            .fallback
            .as_mut()
            .and_then(|fallback| fallback.recording())
        else {
            return;
        };
        self.fallback = None;
        match recording {
            Ok(recording) => {
                err!("ships: Digitraffic unreachable ({reason}); replaying the recording");
                self.source.close(now);
                self.source = Box::new(ReplaySource::new(Arc::new(recording), 1.));
                self.notice =
                    Some(format!("Digitraffic unreachable ({reason}): a recording replays").into());
            }
            Err(error) => err!("ships: Digitraffic unreachable, and no recording ({error})"),
        }
    }

    fn publish(&mut self, clock_ms: f64) {
        let selected = self.control.selected().and_then(|mmsi| {
            let vessel = self.fleet.vessel(mmsi)?;
            Some(VesselDetail {
                row: vessel.row().clone(),
                metadata: vessel.metadata().cloned(),
                trail: vessel.trail().collect(),
                seen_ms: vessel.seen_ms(),
            })
        });
        self.control.publish(FleetFrame {
            clock_ms,
            counts: self.fleet.counts(),
            rate: self.rate.rate(RATE_SECONDS),
            history: self.rate.history(),
            messages: self.messages,
            dots: self.fleet.dots(),
            trails: self.fleet.trails(),
            selected,
            connection: self.source.connection(),
            notice: self.notice.clone(),
            apply: self.apply,
            first_rows: self.first_rows,
        });
    }
}

/// Runs `driver` until its store is gone. Returns the task to keep while
/// it runs: in the browser it runs the driver, natively it is done at once
/// and the driver runs on a thread of its own.
pub fn start(mut driver: Driver, cx: &App) -> Task<()> {
    #[cfg(not(target_family = "wasm"))]
    {
        let _ = cx;
        let spawned = std::thread::Builder::new()
            .name("ships-driver".into())
            .spawn(move || {
                while driver.step() {
                    std::thread::sleep(TICK);
                }
            });
        if let Err(error) = spawned {
            err!("ships: no driver thread ({error})");
        }
        Task::ready(())
    }
    #[cfg(target_family = "wasm")]
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        while driver.step() {
            executor.timer(TICK).await;
        }
    })
}
