//! The pump: four times a second it takes what the source delivered, runs
//! it through the aggregation (`pulse.rs`), writes the changed rows to the
//! grid store in one batch and publishes what the pulse panel, the detail
//! and the status bar show (`PulseFrame`). Natively it runs on a thread of
//! its own; in the browser on the executor's timer, on the page's only
//! thread. No GPUI types besides the store's writer and clock.
//!
//! The view talks to it through [`Control`]: it swaps the source (live,
//! replay, none), pauses it, selects a wiki and reads the last frame.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use gpui_kit_grid::{Clock, GridWriter};
use parking_lot::Mutex;
// Timing the step only: the windows run on the source's clock.
use web_time::Instant;

use super::{
    edit::{Edit, TitleGlyphs},
    pulse::{Lane, Pulse, Tick, WikiDetail},
    rows::WikiRow,
    source::{Counters, EditSource, FeedState},
};

/// A step every 250 ms.
pub const STEP: Duration = Duration::from_millis(250);

/// What the view shows after a step.
#[derive(Clone, Debug, Default)]
pub struct PulseFrame {
    /// The source's clock at the step, ms.
    pub now_ms: u64,
    /// The edits of the last minute, oldest first.
    pub ticks: Vec<Tick>,
    pub lanes: Vec<Lane>,
    pub wikis: usize,
    pub per_minute: usize,
    pub events_per_second: f64,
    pub edits: u64,
    pub events: u64,
    /// `None` before a source runs.
    pub feed: Option<FeedState>,
    pub counters: Counters,
    pub error: Option<String>,
    pub detail: Option<WikiDetail>,
    /// Rows written since the start, and a second over the last second.
    pub rows_written: u64,
    pub rows_per_second: u64,
    /// The last step: taking, aggregating and writing.
    pub step: Duration,
    pub steps: u64,
}

/// A new source for the pump; `reset` empties the table first.
struct Swap {
    source: Option<Box<dyn EditSource>>,
    reset: bool,
}

/// What the view and the pump share.
pub struct Control {
    frame: Mutex<Arc<PulseFrame>>,
    version: AtomicU64,
    swap: Mutex<Option<Swap>>,
    paused: AtomicBool,
    closed: AtomicBool,
    selected: Mutex<Option<Arc<str>>>,
    speed: Arc<AtomicU32>,
}

impl Control {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            frame: Mutex::new(Arc::default()),
            version: AtomicU64::new(0),
            swap: Mutex::new(None),
            paused: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            selected: Mutex::new(None),
            speed: Arc::new(AtomicU32::new(1)),
        })
    }

    /// The pump takes `source` at its next step, after emptying the table
    /// if `reset`; `None` stops reading.
    pub fn set_source(&self, source: Option<Box<dyn EditSource>>, reset: bool) {
        let mut swap = self.swap.lock();
        let reset = reset || swap.as_ref().is_some_and(|swap| swap.reset);
        *swap = Some(Swap { source, reset });
        self.paused.store(false, Ordering::Relaxed);
    }

    /// Holds the table and the panel as they are, or lets them go on.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// The replay's speed, shared with it.
    pub fn speed(&self) -> &Arc<AtomicU32> {
        &self.speed
    }

    pub fn select(&self, wiki: Option<Arc<str>>) {
        *self.selected.lock() = wiki;
    }

    pub fn selected(&self) -> Option<Arc<str>> {
        self.selected.lock().clone()
    }

    /// The view is gone: the pump stops.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }

    pub fn frame(&self) -> Arc<PulseFrame> {
        self.frame.lock().clone()
    }

    /// Changes with every frame published.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    fn publish(&self, frame: PulseFrame) {
        *self.frame.lock() = Arc::new(frame);
        self.version.fetch_add(1, Ordering::Relaxed);
    }
}

/// The pump's state between steps.
pub struct Pump {
    control: Arc<Control>,
    writer: GridWriter<WikiRow>,
    clock: Arc<dyn Clock>,
    glyphs: TitleGlyphs,
    source: Option<Box<dyn EditSource>>,
    pulse: Pulse,
    edits: Vec<Edit>,
    rows: Vec<WikiRow>,
    rows_written: u64,
    /// Rows written in each of the last steps, for the rate.
    recent_writes: [u64; 4],
    steps: u64,
}

impl Pump {
    pub fn new(
        control: Arc<Control>,
        writer: GridWriter<WikiRow>,
        clock: Arc<dyn Clock>,
        glyphs: TitleGlyphs,
    ) -> Self {
        Self {
            control,
            writer,
            clock,
            pulse: Pulse::new(glyphs.clone()),
            glyphs,
            source: None,
            edits: Vec::new(),
            rows: Vec::new(),
            rows_written: 0,
            recent_writes: [0; 4],
            steps: 0,
        }
    }

    /// One step; false once the view or the store is gone.
    pub fn step(&mut self) -> bool {
        if self.control.closed.load(Ordering::Relaxed) || self.writer.is_closed() {
            return false;
        }
        let swap = self.control.swap.lock().take();
        if let Some(swap) = swap {
            if swap.reset {
                self.pulse = Pulse::new(self.glyphs.clone());
                self.writer.replace_all([]);
                self.rows_written = 0;
                self.control.publish(PulseFrame::default());
            }
            self.source = swap.source;
        }
        if self.control.is_paused() {
            return true;
        }
        let Some(source) = self.source.as_mut() else {
            return true;
        };
        let started = Instant::now();
        let status = source.take(self.clock.now(), &mut self.edits);
        for edit in self.edits.drain(..) {
            self.pulse.ingest(&edit);
        }
        self.pulse.advance(status.now_ms);
        self.pulse.changed_rows(&mut self.rows);
        let written = self.rows.len() as u64;
        if !self.rows.is_empty() {
            let rows = &mut self.rows;
            self.writer.batch(|batch| {
                for row in rows.drain(..) {
                    batch.upsert(row);
                }
            });
        }
        self.rows_written += written;
        self.recent_writes.rotate_left(1);
        self.recent_writes[3] = written;
        self.steps += 1;
        let detail = self
            .control
            .selected()
            .and_then(|wiki| self.pulse.detail(&wiki));
        let pulse = &self.pulse;
        self.control.publish(PulseFrame {
            now_ms: pulse.now_ms(),
            ticks: pulse.ticks().copied().collect(),
            lanes: pulse.lanes(),
            wikis: pulse.wikis(),
            per_minute: pulse.per_minute(),
            events_per_second: pulse.events_per_second(),
            edits: pulse.edits(),
            events: pulse.events(),
            feed: Some(status.state),
            counters: status.counters,
            error: status.error,
            detail,
            rows_written: self.rows_written,
            rows_per_second: self.recent_writes.iter().sum(),
            step: started.elapsed(),
            steps: self.steps,
        });
        true
    }
}
