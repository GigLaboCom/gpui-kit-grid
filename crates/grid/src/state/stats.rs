//! Timing knobs, profiling counters and the profiling clock.

use std::time::Duration;

use web_time::Instant;

use crate::store::SNAPSHOT_INTERVAL;

/// Timing knobs of a grid.
#[derive(Clone, Copy, Debug)]
pub struct GridTuning {
    pub(super) min_rebuild_interval: Duration,
    pub(super) pull_interval: Duration,
}

impl Default for GridTuning {
    fn default() -> Self {
        Self {
            min_rebuild_interval: Duration::from_millis(100),
            pull_interval: SNAPSHOT_INTERVAL,
        }
    }
}

impl GridTuning {
    /// Rebuilds caused by data start at most this often (user actions
    /// rebuild at once). Default 100 ms, like LiveGrid.
    pub fn with_min_rebuild_interval(mut self, interval: Duration) -> Self {
        self.min_rebuild_interval = interval;
        self
    }

    /// Snapshots are pulled from the store at most this often. Default 8 ms,
    /// the store's default publish interval
    /// ([`StoreOptions::with_publish_interval`](crate::StoreOptions::with_publish_interval)).
    pub fn with_pull_interval(mut self, interval: Duration) -> Self {
        self.pull_interval = interval;
        self
    }
}

/// Counters for profiling a grid.
#[derive(Clone, Copy, Debug, Default)]
pub struct GridStats {
    pub(super) frames: u64,
    pub(super) rebuilds: u64,
    pub(super) last_rebuild: Duration,
    pub(super) max_rebuild: Duration,
    pub(super) last_prepaint: Duration,
    pub(super) last_header_render: Duration,
    pub(super) last_pull: Duration,
    pub(super) fade_wakeups: u64,
    pub(super) visible_cells: usize,
    pub(super) icons: usize,
    pub(super) snapshots: u64,
    pub(super) compactions: u64,
    pub(super) tombstones: usize,
    pub(super) fading_lines: usize,
    pub(super) painted_captions: usize,
    pub(super) a11y_rows: usize,
    pub(super) a11y_cells: usize,
    pub(super) last_a11y_plan: Duration,
    // The tree.
    pub(super) tree_cycles: usize,
    // Elements in the body's rows.
    pub(super) cell_hosts: usize,
    pub(super) last_hosts_prepaint: Duration,
    // Images in cells.
    pub(super) images: usize,
    // The drop target of the last drag move.
    pub(super) last_drop_target: Duration,
}

impl GridStats {
    /// Frames painted.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Projection rebuilds finished.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// CPU time of the last rebuild (background thread).
    pub fn last_rebuild(&self) -> Duration {
        self.last_rebuild
    }

    /// CPU time of the slowest rebuild.
    pub fn max_rebuild(&self) -> Duration {
        self.max_rebuild
    }

    /// CPU time of the last frame plan (prepaint of the body).
    pub fn last_prepaint(&self) -> Duration {
        self.last_prepaint
    }

    /// CPU time of the last header build (its elements, in `render`).
    pub fn last_header_render(&self) -> Duration {
        self.last_header_render
    }

    /// CPU time of applying the last store snapshot on the UI thread: the
    /// pull, with what removals do to the selection and the focus.
    pub fn last_pull(&self) -> Duration {
        self.last_pull
    }

    /// Repaints caused only by the highlight fade timer.
    pub fn fade_wakeups(&self) -> u64 {
        self.fade_wakeups
    }

    /// Cells planned in the last frame.
    pub fn visible_cells(&self) -> usize {
        self.visible_cells
    }

    /// Icons painted in the body in the last frame: change arrows
    /// (`CellStyle::set_trend`), icons at the start of cells and icons of
    /// `CellValue::Icon` cells.
    pub fn icons(&self) -> usize {
        self.icons
    }

    /// Images painted in the body in the last frame: the `CellValue::Image`
    /// cells whose image was ready.
    pub fn images(&self) -> usize {
        self.images
    }

    /// Store snapshots pulled.
    pub fn snapshots(&self) -> u64 {
        self.snapshots
    }

    /// Projections compacted after removals (no sorting).
    pub fn compactions(&self) -> u64 {
        self.compactions
    }

    /// Removed rows skipped while painting the last frame, before the
    /// projection caught up: rows without a removal fade, and fading rows
    /// whose fade ended.
    pub fn tombstones(&self) -> usize {
        self.tombstones
    }

    /// Removed rows painted fading out in the last frame
    /// ([`HighlightPolicy::with_removal_fade`]).
    ///
    /// [`HighlightPolicy::with_removal_fade`]: crate::HighlightPolicy::with_removal_fade
    pub fn fading_lines(&self) -> usize {
        self.fading_lines
    }

    /// Group captions painted in the last frame.
    pub fn painted_captions(&self) -> usize {
        self.painted_captions
    }

    /// Row nodes of the accessibility tree planned in the last frame: the
    /// painted data and pinned rows, and group captions. Zero while
    /// accessibility is off.
    pub fn a11y_rows(&self) -> usize {
        self.a11y_rows
    }

    /// Synthetic cell nodes planned in the last frame: the painted cells
    /// but the focused one. Zero while accessibility is off.
    pub fn a11y_cells(&self) -> usize {
        self.a11y_cells
    }

    /// CPU time of the last frame's accessibility plan, after the cell walk
    /// gathered its texts. Zero while accessibility is off. With
    /// `LiveGridState::set_a11y_forced` in a window whose accessibility is
    /// off (headless tests and benchmarks), it also builds the nodes GPUI
    /// would build from the plan, so it times both.
    pub fn last_a11y_plan(&self) -> Duration {
        self.last_a11y_plan
    }

    // The tree's cycles.

    /// Rows of the tree shown made roots because their parents form a cycle
    /// (`Projection::tree_cycles`); 0 for a flat or grouped view. A row
    /// that is its own parent is a root but no cycle.
    pub fn tree_cycles(&self) -> usize {
        self.tree_cycles
    }

    // Elements in the body's rows.

    /// Cell hosts planned in the last frame: real elements in cells, such
    /// as the elements of `Element` and `Control` columns and an open
    /// editor.
    pub fn cell_hosts(&self) -> usize {
        self.cell_hosts
    }

    /// CPU time of the layout and prepaint of the last frame's row
    /// elements, when it had cell hosts; zero otherwise.
    pub fn last_hosts_prepaint(&self) -> Duration {
        self.last_hosts_prepaint
    }

    /// Main-thread time the last move of a drag over the rows took to place
    /// it: the hit test and the drop policy (S38).
    pub fn last_drop_target(&self) -> Duration {
        self.last_drop_target
    }

    /// These counters with `tree_cycles` of the projection shown.
    pub(super) fn with_tree_cycles(mut self, tree_cycles: usize) -> Self {
        self.tree_cycles = tree_cycles;
        self
    }
}

/// Profiling clock for job and frame durations: `std::time::Instant` on
/// native targets and `performance.now()` in the browser (`web_time`), where
/// `std::time::Instant::now` panics.
pub(super) fn profile_start() -> Option<Instant> {
    Some(Instant::now())
}

pub(super) fn profile_elapsed(started: Option<Instant>) -> Duration {
    started.map_or(Duration::ZERO, |started| started.elapsed())
}
