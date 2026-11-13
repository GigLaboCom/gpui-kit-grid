//! Row storage: a single-writer ingest task that owns the rows, the pinned
//! rows and the progress, and publishes immutable snapshots.
//!
//! `Ingest` and `StoreSnapshot` have no GPUI types. `GridStore` only uses the
//! GPUI background executor to run the ingest loop and to read the clock.

mod queue;

use std::{collections::VecDeque, future::Future, sync::Arc, time::Duration};

use gpui_kit::{App, BackgroundExecutor, Task};
use parking_lot::Mutex;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use web_time::Instant;

pub use queue::WriteCoalescing;

use crate::{
    highlight::{HighlightPolicy, RowChange, RowStamps, SparseStamps, Stamp},
    model::{ColumnIx, ColumnSet, GridColumns, GridRow},
};
use queue::{QueueCounters, WriteQueue};

/// Rows per page. Publishing a snapshot copies only the pages changed since
/// the previous snapshot, and only while that snapshot is still held.
pub const PAGE_SIZE: usize = 1024;

/// The default interval between published snapshots and between a grid's
/// pulls of them: the store publishes no more often than a grid pulls.
pub(crate) const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(8);

/// The edge a pinned band sits at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PinnedEdge {
    /// Under the header, above the data rows.
    Top,
    /// At the bottom of the body, below the data rows.
    Bottom,
}

impl PinnedEdge {
    const fn ix(self) -> usize {
        match self {
            Self::Top => 0,
            Self::Bottom => 1,
        }
    }
}

/// Progress of work the grid waits for: a load, a reload, a recalculation.
///
/// Written with [`GridWriter::set_progress`], in order with the data, and
/// read from [`StoreSnapshot::progress`].
#[derive(Clone, Debug, PartialEq)]
pub struct GridProgress {
    fraction: Option<f32>,
    label: Option<Arc<str>>,
}

impl GridProgress {
    /// Work of unknown length.
    pub fn indeterminate() -> Self {
        Self {
            fraction: None,
            label: None,
        }
    }

    /// Work that is `fraction` done, clamped to `0.0..=1.0`; NaN is
    /// indeterminate.
    pub fn new(fraction: f32) -> Self {
        Self {
            fraction: (!fraction.is_nan()).then(|| fraction.clamp(0.0, 1.0)),
            label: None,
        }
    }

    /// Sets the text that describes the work, e.g. "Loading trades".
    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The done fraction in `0.0..=1.0`, or `None` for work of unknown length.
    pub fn fraction(&self) -> Option<f32> {
        self.fraction
    }

    /// The text that describes the work.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Returns true if the length of the work is unknown.
    pub fn is_indeterminate(&self) -> bool {
        self.fraction.is_none()
    }
}

/// The storage slot of a row plus its generation. A `RowId` held by a stale
/// projection stops resolving once its row is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RowId {
    ix: u32,
    generation: u32,
}

impl RowId {
    /// The slot index.
    pub fn ix(self) -> usize {
        self.ix as usize
    }
}

struct Slot<R> {
    row: Option<Arc<R>>,
    generation: u32,
    seq: u64,
    stamps: Option<SparseStamps>,
}

impl<R> Clone for Slot<R> {
    fn clone(&self) -> Self {
        Self {
            row: self.row.clone(),
            generation: self.generation,
            seq: self.seq,
            stamps: self.stamps.clone(),
        }
    }
}

impl<R> Default for Slot<R> {
    fn default() -> Self {
        Self {
            row: None,
            generation: 0,
            seq: 0,
            stamps: None,
        }
    }
}

struct Page<R> {
    slots: Vec<Slot<R>>,
}

impl<R> Clone for Page<R> {
    fn clone(&self) -> Self {
        Self {
            slots: self.slots.clone(),
        }
    }
}

/// The rows pinned at one edge and their change stamps, index for index.
/// Bands are short and replaced whole, so a snapshot shares them by `Arc`.
struct PinnedBand<R> {
    rows: Vec<Arc<R>>,
    stamps: Vec<Option<SparseStamps>>,
}

impl<R> Clone for PinnedBand<R> {
    fn clone(&self) -> Self {
        Self {
            rows: self.rows.clone(),
            stamps: self.stamps.clone(),
        }
    }
}

impl<R> Default for PinnedBand<R> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            stamps: Vec::new(),
        }
    }
}

/// The clock change stamps and highlight ages are measured with. The store
/// and every view over it read the same clock.
pub trait Clock: Send + Sync + 'static {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// GPUI's executor clock: real time in applications, a fake clock in tests,
/// `web_time` on the web.
impl Clock for BackgroundExecutor {
    fn now(&self) -> Instant {
        BackgroundExecutor::now(self)
    }
}

/// Resolves a reload: a fresh snapshot from the server (`replace_all`)
/// against live operations producers wrote while it was loading.
///
/// The default is [`ReplayLive`]. A policy that knows row versions can drop
/// live operations older than the snapshot instead. The live operations
/// include [`Op::SetPinnedRows`]; [`Op::SetProgress`] is never buffered.
pub trait ReloadPolicy<R: GridRow>: Send + 'static {
    /// Returns the operations to apply, in order. Runs on the ingest task.
    fn resolve(&mut self, snapshot: Vec<Arc<R>>, live: Vec<Op<R>>) -> Vec<Op<R>>;
}

/// Replaces every row with the snapshot, then replays the live operations
/// written meanwhile, so nothing newer than the snapshot is lost. Pinned rows
/// written during the reload are replayed the same way.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReplayLive;

impl<R: GridRow> ReloadPolicy<R> for ReplayLive {
    fn resolve(&mut self, snapshot: Vec<Arc<R>>, mut live: Vec<Op<R>>) -> Vec<Op<R>> {
        let mut ops = Vec::with_capacity(live.len() + 1);
        ops.push(Op::ReplaceAll(snapshot));
        ops.append(&mut live);
        ops
    }
}

/// An operation written by a producer.
#[non_exhaustive]
pub enum Op<R: GridRow> {
    /// Insert or replace by key, highlighting changed columns.
    Upsert(Arc<R>),
    /// Insert or replace by key without highlight (initial load).
    UpsertSilent(Arc<R>),
    /// Highlight columns of an existing row without replacing it.
    MarkChanged(R::Key, ColumnSet),
    /// Remove by key.
    Remove(R::Key),
    /// Atomically replace every row, without highlight.
    ReplaceAll(Vec<Arc<R>>),
    /// Remove every row.
    Clear,
    /// A reload finished: resolved by the store's [`ReloadPolicy`].
    FinishReload {
        /// The rows the reload returned.
        snapshot: Vec<Arc<R>>,
        /// Operations producers wrote while the reload was running.
        live: Vec<Op<R>>,
    },
    /// Replace the rows pinned at `edge`, highlighting changes by key. Data
    /// rows, `ReplaceAll` and `Clear` never touch a band.
    SetPinnedRows {
        /// The band to replace.
        edge: PinnedEdge,
        /// The new rows, in order. A repeated key keeps its last row.
        rows: Vec<Arc<R>>,
    },
    /// Show (`Some`) or clear (`None`) the store's progress. A running
    /// reload never buffers it, so a [`ReloadPolicy`] does not see it.
    SetProgress(Option<GridProgress>),
}

/// The key index published with every snapshot: a persistent hash map, so
/// publishing is O(1) and a lookup is O(log32 n), about four hops at 100k
/// rows. The ingest's write path uses its own `FxHashMap` (O(1)); this index
/// changes only on inserts and removals, never on updates.
type KeyIndex<K> =
    imbl::GenericHashMap<K, RowId, FxBuildHasher, imbl::shared_ptr::DefaultSharedPtr>;

/// A removed row while it fades out ([`HighlightPolicy::with_removal_fade`]):
/// its last version, its removal stamp and its insertion sequence.
struct FadingRow<R> {
    row: Arc<R>,
    removed: Stamp,
    seq: u64,
}

impl<R> Clone for FadingRow<R> {
    fn clone(&self) -> Self {
        Self {
            row: self.row.clone(),
            removed: self.removed,
            seq: self.seq,
        }
    }
}

/// The fading rows published with every snapshot, by their id before
/// removal: a persistent hash map, so publishing is O(1). It holds at most
/// the removals of one fade.
type FadingMap<R> =
    imbl::GenericHashMap<RowId, FadingRow<R>, FxBuildHasher, imbl::shared_ptr::DefaultSharedPtr>;

/// What changed between two snapshots, accumulated per subscriber.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    inserted: usize,
    removed: Vec<RowId>,
    faded: Vec<RowId>,
    updated: Vec<RowId>,
    columns: ColumnSet,
    reset: bool,
    parent: bool,
    pinned: bool,
    progress: bool,
}

impl ChangeSet {
    /// Merges `other` into this set.
    pub fn merge(&mut self, other: &ChangeSet) {
        self.inserted += other.inserted;
        self.removed.extend_from_slice(&other.removed);
        self.faded.extend_from_slice(&other.faded);
        self.updated.extend_from_slice(&other.updated);
        self.columns.union_with(other.columns);
        self.reset |= other.reset;
        self.parent |= other.parent;
        self.pinned |= other.pinned;
        self.progress |= other.progress;
    }

    /// Returns true if rows were inserted, removed or replaced wholesale.
    pub fn has_structure_change(&self) -> bool {
        self.inserted > 0 || !self.removed.is_empty() || self.reset
    }

    /// Returns true if rows were inserted or every row was replaced: the
    /// display order has to be rebuilt.
    pub fn has_insertions_or_reset(&self) -> bool {
        self.inserted > 0 || self.reset
    }

    /// Returns true if every row was replaced or removed (`replace_all`,
    /// `clear`): a new data set, not a stream of changes.
    pub fn is_reset(&self) -> bool {
        self.reset
    }

    /// The number of inserted rows.
    pub fn inserted(&self) -> usize {
        self.inserted
    }

    /// The ids of removed rows (empty after a reset, which removes all).
    ///
    /// A row that fades out ([`HighlightPolicy::with_removal_fade`]) is here
    /// at its removal, so the selection lets go of its key at once, and in
    /// [`Self::faded`] when its fade ends.
    pub fn removed(&self) -> &[RowId] {
        &self.removed
    }

    /// The ids of fading rows the store dropped from its snapshots: their
    /// fade ended, their key was written again, or a reset (`replace_all`,
    /// `clear`) dropped every fading row. A view that shows fading rows
    /// drops them from its projection. [`Self::removed`] reported them
    /// already, at removal, so they are not a structure change.
    pub fn faded(&self) -> &[RowId] {
        &self.faded
    }

    /// Returns true if a replaced row has another
    /// [`GridRow::parent_key`] than its previous version: a tree view
    /// rebuilds on it. Reported even when no column changed; such a
    /// replacement is in [`Self::updated`], without highlight.
    pub fn has_parent_change(&self) -> bool {
        self.parent
    }

    /// The columns whose values changed in data rows. Pinned rows do not add
    /// to it: they are not in the display order. Neither does a replacement
    /// that changes no column.
    pub fn columns(&self) -> ColumnSet {
        self.columns
    }

    /// The ids of rows replaced by a new row or marked changed, so a view
    /// can tell whether any of them is on screen.
    ///
    /// A replacement that changes no column is here too, without highlight
    /// and without [`Self::columns`]: data the columns do not show, such as
    /// a tooltip text or a style input, may have changed, so a view repaints
    /// the row if it is on screen, and the display order needs no work. The
    /// same `Arc` written again is not a replacement.
    pub fn updated(&self) -> &[RowId] {
        &self.updated
    }

    /// Returns true if a band of pinned rows changed: rows, order or values.
    /// A row replaced by a new `Arc` is a change even when no column
    /// changed; a band of the same `Arc`s in the same order is not. The
    /// display order needs no work for it, only a repaint.
    pub fn has_pinned_change(&self) -> bool {
        self.pinned
    }

    /// Returns true if the progress written through
    /// [`GridWriter::set_progress`] changed.
    pub fn has_progress_change(&self) -> bool {
        self.progress
    }

    /// Returns true if nothing changed.
    pub fn is_empty(&self) -> bool {
        self.inserted == 0
            && self.removed.is_empty()
            && self.faded.is_empty()
            && self.updated.is_empty()
            && self.columns.is_empty()
            && !self.reset
            && !self.parent
            && !self.pinned
            && !self.progress
    }
}

/// An immutable view of every row at one store version.
pub struct StoreSnapshot<R: GridRow> {
    version: u64,
    pages: Vec<Arc<Page<R>>>,
    index: KeyIndex<R::Key>,
    capacity: usize,
    len: usize,
    epoch: Instant,
    pinned: [Arc<PinnedBand<R>>; 2],
    progress: Option<GridProgress>,
    fading: FadingMap<R>,
}

impl<R: GridRow> StoreSnapshot<R> {
    /// The store version this snapshot was published at.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The number of live rows.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if there are no rows.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of slots, live or free. Row indices are below this.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The instant stamps are measured from.
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    fn slot(&self, ix: usize) -> Option<&Slot<R>> {
        if ix >= self.capacity {
            return None;
        }
        self.pages
            .get(ix / PAGE_SIZE)
            .and_then(|page| page.slots.get(ix % PAGE_SIZE))
    }

    fn live_slot(&self, id: RowId) -> Option<&Slot<R>> {
        self.slot(id.ix())
            .filter(|slot| slot.generation == id.generation && slot.row.is_some())
    }

    /// The row with this id, or `None` if it was removed since.
    pub fn row(&self, id: RowId) -> Option<&Arc<R>> {
        self.live_slot(id).and_then(|slot| slot.row.as_ref())
    }

    /// The id of the row with this key in this snapshot.
    pub fn row_id(&self, key: &R::Key) -> Option<RowId> {
        self.index.get(key).copied()
    }

    /// The row with this key in this snapshot.
    pub fn get(&self, key: &R::Key) -> Option<&Arc<R>> {
        self.row_id(key).and_then(|id| self.row(id))
    }

    /// The change stamps of the row's cells, if any is hot. Read a cell's
    /// with [`RowStamps::get`].
    pub fn stamps(&self, id: RowId) -> Option<RowStamps<'_>> {
        self.live_slot(id)
            .and_then(|slot| slot.stamps.as_ref())
            .map(SparseStamps::view)
    }

    /// The insertion sequence of the row: the stable tie-breaker for sorting.
    pub fn seq(&self, id: RowId) -> u64 {
        self.live_slot(id).map_or(0, |slot| slot.seq)
    }

    /// Every live row with its id, in storage order.
    pub fn iter(&self) -> impl Iterator<Item = (RowId, &Arc<R>)> {
        self.pages.iter().enumerate().flat_map(|(page_ix, page)| {
            page.slots
                .iter()
                .enumerate()
                .filter_map(move |(slot_ix, slot)| {
                    slot.row.as_ref().map(|row| {
                        let id = RowId {
                            ix: (page_ix * PAGE_SIZE + slot_ix) as u32,
                            generation: slot.generation,
                        };
                        (id, row)
                    })
                })
        })
    }

    /// The rows pinned at `edge`, in order. Their keys are a space of their
    /// own: a pinned row may share its key with a data row or with a row of
    /// the other band.
    pub fn pinned_rows(&self, edge: PinnedEdge) -> &[Arc<R>] {
        &self.pinned[edge.ix()].rows
    }

    /// The change stamps of the cells of the `ix`-th row pinned at `edge`,
    /// if any is hot.
    pub fn pinned_stamps(&self, edge: PinnedEdge, ix: usize) -> Option<RowStamps<'_>> {
        self.pinned[edge.ix()]
            .stamps
            .get(ix)?
            .as_ref()
            .map(SparseStamps::view)
    }

    /// The progress written through [`GridWriter::set_progress`].
    pub fn progress(&self) -> Option<&GridProgress> {
        self.progress.as_ref()
    }

    /// A removed row while it fades out
    /// ([`HighlightPolicy::with_removal_fade`]): its last version and its
    /// removal stamp. `id` is the row's id before removal (its old
    /// generation), so it never resolves to a live row.
    pub fn fading_row(&self, id: RowId) -> Option<(&Arc<R>, Stamp)> {
        self.fading
            .get(&id)
            .map(|fading| (&fading.row, fading.removed))
    }

    /// Every fading row with its id, removal stamp and insertion sequence
    /// (the tie-breaker for sorting, as [`Self::seq`] of a live row), in no
    /// particular order.
    pub fn fading_rows(&self) -> impl Iterator<Item = (RowId, &Arc<R>, Stamp, u64)> {
        self.fading
            .iter()
            .map(|(id, fading)| (*id, &fading.row, fading.removed, fading.seq))
    }

    /// Bytes held by the change stamps of data rows, their allocations'
    /// reference counts included (for the spike's memory measurement).
    pub fn stamp_bytes(&self) -> usize {
        self.pages
            .iter()
            .flat_map(|page| page.slots.iter())
            .filter_map(|slot| slot.stamps.as_ref())
            .map(SparseStamps::heap_bytes)
            .sum()
    }
}

/// The single writer of the rows. Applies operations in order, diffs every
/// replacement, writes change stamps and publishes snapshots.
pub struct Ingest<R: GridRow> {
    pages: Vec<Arc<Page<R>>>,
    capacity: usize,
    /// Write path: O(1) key lookup for every operation.
    index: FxHashMap<R::Key, RowId>,
    /// Read path: published with snapshots; changes on inserts and removals.
    published_index: KeyIndex<R::Key>,
    free: Vec<u32>,
    next_seq: u64,
    version: u64,
    hot: FxHashSet<u32>,
    epoch: Instant,
    policy: HighlightPolicy,
    reload: Box<dyn ReloadPolicy<R>>,
    /// Every column of `R`: the cells an insert flashes.
    every: ColumnSet,
    last_sweep_ms: u32,
    pinned: [Arc<PinnedBand<R>>; 2],
    progress: Option<GridProgress>,
    /// Read path: removed rows while they fade, published with snapshots.
    fading: FadingMap<R>,
    /// Write path: the fading row of a removed key, so that a new row of
    /// the key drops it at once.
    fading_keys: FxHashMap<R::Key, RowId>,
    /// Fading rows with their removal stamps, in removal order: the front
    /// ends its fade first, so a batch checks one entry while no fade has
    /// ended. A row dropped earlier stays here until it reaches the front.
    fade_queue: VecDeque<(RowId, Stamp)>,
}

impl<R: GridRow> Ingest<R> {
    /// Creates an empty store whose stamps are measured from `epoch`.
    pub fn new(policy: HighlightPolicy, epoch: Instant) -> Self {
        Self::with_reload_policy(policy, epoch, Box::new(ReplayLive))
    }

    /// Creates an empty store with a custom [`ReloadPolicy`].
    pub fn with_reload_policy(
        policy: HighlightPolicy,
        epoch: Instant,
        reload: Box<dyn ReloadPolicy<R>>,
    ) -> Self {
        Self {
            pages: Vec::new(),
            capacity: 0,
            index: FxHashMap::default(),
            published_index: KeyIndex::default(),
            free: Vec::new(),
            next_seq: 0,
            version: 0,
            hot: FxHashSet::default(),
            epoch,
            policy,
            reload,
            every: ColumnSet::first(R::Column::all().len()),
            last_sweep_ms: 0,
            pinned: Default::default(),
            progress: None,
            fading: FadingMap::default(),
            fading_keys: FxHashMap::default(),
            fade_queue: VecDeque::new(),
        }
    }

    /// The number of live rows.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Returns true if there are no rows.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// The number of rows that currently own change stamps.
    pub fn hot_rows(&self) -> usize {
        self.hot.len()
    }

    fn now_ms(&self, now: Instant) -> u32 {
        now.saturating_duration_since(self.epoch).as_millis() as u32
    }

    fn slot_mut(&mut self, ix: usize) -> &mut Slot<R> {
        let page = Arc::make_mut(&mut self.pages[ix / PAGE_SIZE]);
        &mut page.slots[ix % PAGE_SIZE]
    }

    fn allocate(&mut self) -> usize {
        if let Some(ix) = self.free.pop() {
            return ix as usize;
        }
        let ix = self.capacity;
        if ix % PAGE_SIZE == 0 {
            let mut slots = Vec::with_capacity(PAGE_SIZE);
            slots.resize_with(PAGE_SIZE, Slot::default);
            self.pages.push(Arc::new(Page { slots }));
        }
        self.capacity += 1;
        ix
    }

    /// Applies `ops` in order at `now` and returns what changed. Then drops
    /// the fading rows whose fade ended at `now` ([`ChangeSet::faded`]),
    /// even when `ops` is empty.
    pub fn apply(&mut self, ops: impl IntoIterator<Item = Op<R>>, now: Instant) -> ChangeSet {
        let now_ms = self.now_ms(now);
        let mut changes = ChangeSet::default();
        for op in ops {
            self.apply_op(op, now_ms, &mut changes);
        }
        self.finish_batch(now_ms, changes)
    }

    /// Drops the fading rows whose fade ended and counts the version.
    fn finish_batch(&mut self, now_ms: u32, mut changes: ChangeSet) -> ChangeSet {
        self.drop_faded(now_ms, &mut changes);
        if !changes.is_empty() {
            self.version += 1;
        }
        changes
    }

    fn apply_op(&mut self, op: Op<R>, now_ms: u32, changes: &mut ChangeSet) {
        {
            match op {
                Op::Upsert(row) => self.upsert(row, now_ms, true, changes),
                Op::UpsertSilent(row) => self.upsert(row, now_ms, false, changes),
                Op::MarkChanged(key, columns) => {
                    if let Some(&id) = self.index.get(&key) {
                        let stamp = Stamp::new(now_ms, RowChange::Updated);
                        self.stamp(id.ix(), columns, now_ms, |_| stamp);
                        changes.columns.union_with(columns);
                        changes.updated.push(id);
                    }
                }
                Op::Remove(key) => self.remove(key, now_ms, changes),
                Op::ReplaceAll(rows) => {
                    self.reset(changes);
                    for row in rows {
                        self.upsert(row, now_ms, false, changes);
                    }
                }
                Op::Clear => self.reset(changes),
                Op::FinishReload { snapshot, live } => {
                    for op in self.reload.resolve(snapshot, live) {
                        // A policy cannot nest reloads.
                        if !matches!(op, Op::FinishReload { .. }) {
                            self.apply_op(op, now_ms, changes);
                        }
                    }
                }
                Op::SetPinnedRows { edge, rows } => {
                    self.set_pinned_rows(edge, rows, now_ms, changes);
                }
                Op::SetProgress(progress) => {
                    if self.progress != progress {
                        self.progress = progress;
                        changes.progress = true;
                    }
                }
            }
        }
    }

    /// Replaces a band, diffing it against the previous one by key: changed
    /// columns of a kept key are stamped `Updated` with their direction, a
    /// new key flashes `Inserted`, and a kept key carries its stamps over.
    /// The first fill of an empty band is not highlighted, like
    /// `replace_all`. Costs O(band rows × columns); bands are short.
    fn set_pinned_rows(
        &mut self,
        edge: PinnedEdge,
        rows: Vec<Arc<R>>,
        now_ms: u32,
        changes: &mut ChangeSet,
    ) {
        let written = rows.len();
        let rows = keep_last_of_each_key(rows);
        if cfg!(debug_assertions) && rows.len() < written {
            log::warn!(
                "gpui-kit-grid: {} rows pinned at {edge:?} repeat a key; the last row of each key is kept",
                written - rows.len()
            );
        }
        let previous = self.pinned[edge.ix()].clone();
        let previous_ix: FxHashMap<&R::Key, usize> = previous
            .rows
            .iter()
            .enumerate()
            .map(|(ix, row)| (row.key(), ix))
            .collect();
        let first_fill = previous.rows.is_empty();
        let mut changed = rows.len() != previous.rows.len();
        let (policy, every) = (self.policy, self.every);
        let columns_of = R::Column::all();
        let stamps = rows
            .iter()
            .enumerate()
            .map(|(ix, row)| {
                let Some(&old_ix) = previous_ix.get(row.key()) else {
                    changed = true;
                    let flash = Stamp::new(now_ms, RowChange::Inserted);
                    return (!first_fill).then(|| SparseStamps::whole_row(flash));
                };
                let old = &previous.rows[old_ix];
                let mut columns = ColumnSet::EMPTY;
                // A new row repaints the band even when no column changed,
                // as a replaced data row does.
                let replaced = !Arc::ptr_eq(old, row);
                if replaced {
                    row.changed_columns(old, &mut columns);
                }
                changed |= replaced || old_ix != ix;
                let carried = &previous.stamps[old_ix];
                if columns.is_empty() {
                    return carried.clone();
                }
                SparseStamps::written(
                    carried.as_ref(),
                    columns,
                    every,
                    |stamp| !policy.is_highlight_over(stamp, now_ms),
                    |column| updated::<R>(now_ms, row, old, columns_of[column.get()]),
                )
            })
            .collect();
        // A band of the same rows is replaced too, but publishes nothing,
        // like an upsert of the same row.
        self.pinned[edge.ix()] = Arc::new(PinnedBand { rows, stamps });
        changes.pinned |= changed;
    }

    /// Removes the row of `key`. With a removal fade, its last version
    /// fades out: it stays in snapshots by its old id until the fade ends.
    fn remove(&mut self, key: R::Key, now_ms: u32, changes: &mut ChangeSet) {
        let Some(id) = self.index.remove(&key) else {
            return;
        };
        self.published_index.remove(&key);
        let slot = self.slot_mut(id.ix());
        let row = slot.row.take();
        let seq = slot.seq;
        slot.stamps = None;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(id.ix);
        self.hot.remove(&id.ix);
        changes.removed.push(id);
        if let Some(row) = row
            && self.policy.has_removal_fade()
        {
            let removed = Stamp::removal(now_ms);
            self.fading.insert(id, FadingRow { row, removed, seq });
            self.fading_keys.insert(key, id);
            self.fade_queue.push_back((id, removed));
        }
    }

    /// Drops the fading row of `key` at once, if there is one: a new row of
    /// the key shows without a ghost beside it.
    fn drop_fading_key(&mut self, key: &R::Key, changes: &mut ChangeSet) {
        if let Some(id) = self.fading_keys.remove(key) {
            self.fading.remove(&id);
            changes.faded.push(id);
        }
    }

    /// Drops the fading rows whose fade ended at `now_ms`, earliest first.
    /// O(1) while the earliest fade has not ended.
    fn drop_faded(&mut self, now_ms: u32, changes: &mut ChangeSet) {
        while let Some(&(id, removed)) = self.fade_queue.front() {
            if !self.policy.is_removal_over(removed, now_ms) {
                break;
            }
            self.fade_queue.pop_front();
            // Not there if its key was written again since.
            if let Some(fading) = self.fading.remove(&id) {
                self.fading_keys.remove(fading.row.key());
                changes.faded.push(id);
            }
        }
    }

    /// Frees every slot and drops every fading row: a new data set does not
    /// fade. Slots are kept and their generations bumped, so a `RowId` from
    /// an older projection never resolves to a new row.
    fn reset(&mut self, changes: &mut ChangeSet) {
        changes.reset = true;
        if !self.fading.is_empty() {
            changes.faded.extend(self.fading.keys().copied());
            self.fading = FadingMap::default();
            self.fading_keys.clear();
        }
        self.fade_queue.clear();
        for ix in 0..self.capacity {
            let slot = self.slot_mut(ix);
            if slot.row.is_some() {
                slot.generation = slot.generation.wrapping_add(1);
            }
            slot.row = None;
            slot.stamps = None;
        }
        self.free = (0..self.capacity as u32).rev().collect();
        self.index.clear();
        self.published_index = KeyIndex::default();
        self.hot.clear();
    }

    fn upsert(&mut self, row: Arc<R>, now_ms: u32, highlight: bool, changes: &mut ChangeSet) {
        if let Some(&id) = self.index.get(row.key()) {
            let ix = id.ix();
            let current = self.pages[ix / PAGE_SIZE].slots[ix % PAGE_SIZE]
                .row
                .as_ref();
            // The same row again: nothing to store, and no page to copy.
            if current.is_some_and(|current| Arc::ptr_eq(current, &row)) {
                return;
            }
            let slot = self.slot_mut(ix);
            let Some(previous) = slot.row.replace(row.clone()) else {
                return;
            };
            let mut columns = ColumnSet::EMPTY;
            row.changed_columns(&previous, &mut columns);
            // A tree moves the row; its columns may all be the same.
            changes.parent |= row.parent_key() != previous.parent_key();
            // A new row is published even when no column changed: hooks may
            // read data the columns do not show. Without changed columns it
            // has no highlight and needs no work for the display order.
            changes.updated.push(id);
            if columns.is_empty() {
                return;
            }
            changes.columns.union_with(columns);
            if highlight {
                // Each changed cell against the previous version in this
                // very operation, so a chain A → B → C in one batch stamps
                // the direction of B → C.
                let all = R::Column::all();
                self.stamp(ix, columns, now_ms, |column| {
                    updated::<R>(now_ms, &row, &previous, all[column.get()])
                });
            }
            return;
        }

        if !self.fading_keys.is_empty() {
            self.drop_fading_key(row.key(), changes);
        }
        let ix = self.allocate();
        let seq = self.next_seq;
        self.next_seq += 1;
        let key = row.key().clone();
        let slot = self.slot_mut(ix);
        slot.row = Some(row);
        slot.seq = seq;
        slot.stamps = None;
        let id = RowId {
            ix: ix as u32,
            generation: slot.generation,
        };
        self.published_index.insert(key.clone(), id);
        self.index.insert(key, id);
        changes.inserted += 1;
        if highlight {
            let flash = SparseStamps::whole_row(Stamp::new(now_ms, RowChange::Inserted));
            self.slot_mut(ix).stamps = Some(flash);
            self.hot.insert(ix as u32);
        }
    }

    /// Stamps `columns` of a row with `stamp(column)` and drops the row's
    /// stamps that no longer highlight.
    fn stamp(
        &mut self,
        ix: usize,
        columns: ColumnSet,
        now_ms: u32,
        stamp: impl FnMut(ColumnIx) -> Stamp,
    ) {
        let (policy, every) = (self.policy, self.every);
        let slot = self.slot_mut(ix);
        slot.stamps = SparseStamps::written(
            slot.stamps.as_ref(),
            columns,
            every,
            |stamp| !policy.is_highlight_over(stamp, now_ms),
            stamp,
        );
        if slot.stamps.is_some() {
            self.hot.insert(ix as u32);
        }
    }

    /// Frees the stamps of rows whose highlight is over. Runs at most twice
    /// per highlight duration and costs O(hot rows). A row with some hot
    /// stamps keeps its expired ones until it is written again.
    pub fn sweep(&mut self, now: Instant) {
        let now_ms = self.now_ms(now);
        if now_ms.saturating_sub(self.last_sweep_ms) < self.policy.duration_ms() / 2 {
            return;
        }
        self.last_sweep_ms = now_ms;
        let policy = self.policy;
        let is_hot = |stamp| !policy.is_highlight_over(stamp, now_ms);
        let expired: Vec<u32> = self
            .hot
            .iter()
            .copied()
            .filter(|&ix| {
                let page = &self.pages[ix as usize / PAGE_SIZE];
                page.slots[ix as usize % PAGE_SIZE]
                    .stamps
                    .as_ref()
                    .is_none_or(|stamps| stamps.is_cold(is_hot))
            })
            .collect();
        for ix in expired {
            self.slot_mut(ix as usize).stamps = None;
            self.hot.remove(&ix);
        }
        // Pinned rows are not in `hot`: bands are short, so they are scanned.
        let expired = |stamps: &Option<SparseStamps>| {
            stamps.as_ref().is_some_and(|stamps| stamps.is_cold(is_hot))
        };
        for band in &mut self.pinned {
            if band.stamps.iter().any(expired) {
                for stamps in &mut Arc::make_mut(band).stamps {
                    if expired(stamps) {
                        *stamps = None;
                    }
                }
            }
        }
    }

    /// Publishes the current rows. Cheap: clones one `Arc` per page and per
    /// pinned band.
    pub fn snapshot(&self) -> Arc<StoreSnapshot<R>> {
        Arc::new(StoreSnapshot {
            version: self.version,
            pages: self.pages.clone(),
            index: self.published_index.clone(),
            capacity: self.capacity,
            len: self.index.len(),
            epoch: self.epoch,
            pinned: self.pinned.clone(),
            progress: self.progress.clone(),
            fading: self.fading.clone(),
        })
    }
}

/// The stamp of a changed `column` of `row`, replacing `previous`: `Updated`
/// with the row's own [`GridRow::change_direction`].
#[inline]
fn updated<R: GridRow>(now_ms: u32, row: &R, previous: &R, column: R::Column) -> Stamp {
    let direction = row.change_direction(previous, column);
    Stamp::with_direction(now_ms, RowChange::Updated, direction)
}

/// Drops every row whose key repeats later in the band: the last one wins,
/// in its own position.
fn keep_last_of_each_key<R: GridRow>(rows: Vec<Arc<R>>) -> Vec<Arc<R>> {
    if rows.len() < 2 {
        return rows;
    }
    let mut last_ix: FxHashMap<&R::Key, usize> =
        FxHashMap::with_capacity_and_hasher(rows.len(), FxBuildHasher);
    for (ix, row) in rows.iter().enumerate() {
        last_ix.insert(row.key(), ix);
    }
    if last_ix.len() == rows.len() {
        return rows;
    }
    rows.iter()
        .enumerate()
        .filter(|(ix, row)| last_ix[row.key()] == *ix)
        .map(|(_, row)| row.clone())
        .collect()
}

struct Shared<R: GridRow> {
    pending: Mutex<Pending<R>>,
    wake: async_channel::Sender<()>,
}

impl<R: GridRow> Shared<R> {
    fn wake(&self) {
        let _ = self.wake.try_send(());
    }
}

struct Pending<R: GridRow> {
    queue: WriteQueue<R>,
    /// While a reload runs, live operations wait here.
    reload: Option<ReloadBuffer<R>>,
    next_reload: u64,
    /// `GridWriter::flush` calls waiting for the next batch. When a flush
    /// registers, every operation written before it is in `queue` or already
    /// taken by the ingest. The ingest takes `queue` and the flushes under one
    /// lock and resolves them after publishing, so the batch that takes a
    /// flush covers its operations. No counter is needed.
    flushes: Vec<async_channel::Sender<u64>>,
    /// The ingest task is gone: flushes resolve to `None` at once.
    closed: bool,
}

struct ReloadBuffer<R: GridRow> {
    id: u64,
    live: Vec<Op<R>>,
}

impl<R: GridRow> Pending<R> {
    fn new(coalescing: WriteCoalescing) -> Self {
        Self {
            queue: WriteQueue::new(coalescing),
            reload: None,
            next_reload: 0,
            flushes: Vec::new(),
            closed: false,
        }
    }

    /// Queues `op` for the ingest, or buffers it while a reload runs; the
    /// progress is never buffered. Returns true if `op` was queued: the
    /// ingest has to wake.
    fn write(&mut self, op: Op<R>) -> bool {
        match &mut self.reload {
            Some(reload) if !matches!(op, Op::SetProgress(_)) => {
                self.queue.count_buffered();
                reload.live.push(op);
                false
            }
            _ => {
                self.queue.push(op);
                true
            }
        }
    }

    /// Ends the reload `id` with `snapshot`: queues both with the operations
    /// it buffered. Returns false if a newer reload superseded it.
    fn finish_reload(&mut self, id: u64, snapshot: Vec<Arc<R>>) -> bool {
        let Some(reload) = self.reload.take_if(|reload| reload.id == id) else {
            return false;
        };
        self.queue.push_finished_reload(snapshot, reload.live);
        true
    }
}

/// Lives inside the ingest future: once the task is dropped with the store,
/// it resolves the pending flushes to `None` and refuses new ones.
struct IngestGuard<R: GridRow>(Arc<Shared<R>>);

impl<R: GridRow> Drop for IngestGuard<R> {
    fn drop(&mut self) {
        let flushes = {
            let mut pending = self.0.pending.lock();
            pending.closed = true;
            std::mem::take(&mut pending.flushes)
        };
        // A dropped sender resolves its flush to `None`.
        drop(flushes);
    }
}

/// Writes rows into a [`GridStore`] from any thread.
///
/// Operations are applied in order by the store's ingest task. Writing is a
/// short lock and a non-blocking wake-up; it never waits for the UI.
/// Operations written one by one may be published in different snapshots;
/// [`GridWriter::batch`] publishes several in one.
pub struct GridWriter<R: GridRow> {
    shared: Arc<Shared<R>>,
    hub: Arc<Mutex<Hub<R>>>,
}

impl<R: GridRow> Clone for GridWriter<R> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            hub: self.hub.clone(),
        }
    }
}

impl<R: GridRow> GridWriter<R> {
    fn push(&self, op: Op<R>) {
        let queued = self.shared.pending.lock().write(op);
        if queued {
            self.shared.wake();
        }
    }

    /// Writes the operations of `write` as one batch: the store applies them
    /// in order and publishes them in one snapshot, so no snapshot has some
    /// of them without the others. Other writes may share that snapshot.
    /// Nothing is written until `write` returns; then the batch costs one
    /// lock and at most one wake-up.
    ///
    /// A running reload buffers the batch's writes until it finishes, as it
    /// buffers single ones, but not the batch's progress, which is never
    /// buffered. [`WriteBatch::finish_reload`] finishes a reload inside the
    /// batch: the reloaded rows and the batch's writes after it, such as
    /// clearing the progress, are published together.
    ///
    /// ```ignore
    /// writer.batch(|batch| {
    ///     batch.finish_reload(reload, rows);
    ///     batch.set_progress(None);
    /// });
    /// ```
    pub fn batch(&self, write: impl FnOnce(&mut WriteBatch<R>)) {
        let mut batch = WriteBatch {
            shared: self.shared.clone(),
            writes: Vec::new(),
        };
        write(&mut batch);
        if batch.writes.is_empty() {
            return;
        }
        let mut pending = self.shared.pending.lock();
        let mut queued = false;
        for write in batch.writes {
            queued |= match write {
                BatchWrite::Op(op) => pending.write(op),
                BatchWrite::FinishReload(mut reload, snapshot) => {
                    reload.finished = true;
                    pending.finish_reload(reload.id, snapshot)
                }
            };
        }
        drop(pending);
        if queued {
            self.shared.wake();
        }
    }

    /// Starts a reload: until [`Reload::finish`], operations from every
    /// writer are buffered instead of applied. `finish(snapshot)` hands the
    /// snapshot and the buffered operations to the store's [`ReloadPolicy`]
    /// (by default the snapshot replaces every row and the buffered operations
    /// are replayed on top). Dropping the guard without `finish` cancels the
    /// reload and applies the buffered operations as usual. A newer reload
    /// supersedes an older one: the older `finish` is then ignored.
    pub fn begin_reload(&self) -> Reload<R> {
        let mut pending = self.shared.pending.lock();
        let id = pending.next_reload;
        pending.next_reload += 1;
        let live = pending
            .reload
            .take()
            .map(|reload| reload.live)
            .unwrap_or_default();
        pending.reload = Some(ReloadBuffer { id, live });
        Reload {
            shared: self.shared.clone(),
            id,
            finished: false,
        }
    }

    /// Inserts or replaces a row by key and highlights the changed columns,
    /// each with its [`GridRow::change_direction`].
    ///
    /// A new row that changes no column is published all the same, without
    /// highlight, so hooks that read other data of the row see it. Writing
    /// the same `Arc` again changes nothing.
    pub fn upsert(&self, row: impl Into<Arc<R>>) {
        self.push(Op::Upsert(row.into()));
    }

    /// Inserts or replaces a row by key without highlight.
    pub fn upsert_silent(&self, row: impl Into<Arc<R>>) {
        self.push(Op::UpsertSilent(row.into()));
    }

    /// Highlights columns of an existing row without replacing it, without
    /// a direction: the values did not change.
    pub fn mark_changed(&self, key: R::Key, columns: ColumnSet) {
        self.push(Op::MarkChanged(key, columns));
    }

    /// Removes a row by key. With a removal fade
    /// ([`HighlightPolicy::with_removal_fade`]), its last version stays in
    /// snapshots as a fading row ([`StoreSnapshot::fading_row`]) until the
    /// fade ends or the key is written again.
    pub fn remove(&self, key: R::Key) {
        self.push(Op::Remove(key));
    }

    /// Atomically replaces every row, without highlight.
    pub fn replace_all(&self, rows: impl IntoIterator<Item = Arc<R>>) {
        self.push(Op::ReplaceAll(rows.into_iter().collect()));
    }

    /// Removes every row.
    pub fn clear(&self) {
        self.push(Op::Clear);
    }

    /// Replaces the rows pinned at `edge` (an empty `rows` clears the band).
    ///
    /// Applied in order with other writes; buffered by a running reload like
    /// them. Rows are diffed by key against the previous band: changed
    /// columns are highlighted and a new key flashes, except on the first
    /// fill of an empty band. A key repeated within `rows` keeps its last
    /// row. Pinned keys are a space of their own, apart from the data rows
    /// and the other band. `replace_all` and `clear` leave bands alone.
    pub fn set_pinned_rows(&self, edge: PinnedEdge, rows: impl IntoIterator<Item = Arc<R>>) {
        self.push(Op::SetPinnedRows {
            edge,
            rows: rows.into_iter().collect(),
        });
    }

    /// Shows (`Some`) or clears (`None`) the store's progress.
    ///
    /// Applied in order with other writes, but never buffered by a reload:
    /// progress written during a reload shows while the reload runs, and
    /// progress cleared after [`Reload::finish`] is never cleared before the
    /// snapshot that applies the reload. It may be cleared in a later one;
    /// to clear it in that very snapshot, write both in one
    /// [`GridWriter::batch`] with [`WriteBatch::finish_reload`].
    pub fn set_progress(&self, progress: Option<GridProgress>) {
        self.push(Op::SetProgress(progress));
    }

    /// Resolves once every operation written before the call is applied and
    /// published: with that store version (the version of the published
    /// snapshot that has them), or `None` if the store is gone.
    ///
    /// The operations waited for are fixed by the call, not by the first
    /// poll. Operations buffered by an unfinished reload are not waited for:
    /// they are not written to the store until [`Reload::finish`]. Each call
    /// wakes the ingest task once; an idle store resolves with its current
    /// version.
    pub fn flush(&self) -> impl Future<Output = Option<u64>> + Send + 'static {
        let (done, resolved) = async_channel::bounded(1);
        let mut pending = self.shared.pending.lock();
        if !pending.closed {
            pending.flushes.push(done);
        }
        drop(pending);
        self.shared.wake();
        async move { resolved.recv().await.ok() }
    }

    /// Returns true once the store is gone; producers can stop writing.
    pub fn is_closed(&self) -> bool {
        self.shared.wake.is_closed()
    }

    /// The latest published snapshot, for producers that need the current
    /// rows (e.g. to patch a row by key). Operations still queued are not in it.
    pub fn snapshot(&self) -> Arc<StoreSnapshot<R>> {
        self.hub.lock().snapshot.clone()
    }
}

/// A running reload, from [`GridWriter::begin_reload`].
pub struct Reload<R: GridRow> {
    shared: Arc<Shared<R>>,
    id: u64,
    finished: bool,
}

impl<R: GridRow> Reload<R> {
    /// Finishes the reload with the rows it returned. To publish other
    /// writes in the same snapshot, finish it in a [`GridWriter::batch`]
    /// with [`WriteBatch::finish_reload`] instead.
    pub fn finish(mut self, snapshot: impl IntoIterator<Item = Arc<R>>) {
        self.finished = true;
        let snapshot = snapshot.into_iter().collect();
        // Ignored if a newer reload superseded this one.
        let queued = self.shared.pending.lock().finish_reload(self.id, snapshot);
        if queued {
            self.shared.wake();
        }
    }
}

impl<R: GridRow> Drop for Reload<R> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut pending = self.shared.pending.lock();
        if pending
            .reload
            .as_ref()
            .is_some_and(|reload| reload.id == self.id)
        {
            let live = pending
                .reload
                .take()
                .map(|reload| reload.live)
                .unwrap_or_default();
            pending.queue.push_cancelled_reload(live);
            drop(pending);
            self.shared.wake();
        }
    }
}

/// Operations collected by [`GridWriter::batch`], written when its closure
/// returns and published in one snapshot. Each method writes what the
/// [`GridWriter`] method of the same name writes.
pub struct WriteBatch<R: GridRow> {
    shared: Arc<Shared<R>>,
    writes: Vec<BatchWrite<R>>,
}

/// One write of a [`WriteBatch`], in the order written.
enum BatchWrite<R: GridRow> {
    Op(Op<R>),
    /// Holds the guard: a batch dropped unwritten cancels the reload, like
    /// a dropped [`Reload`].
    FinishReload(Reload<R>, Vec<Arc<R>>),
}

impl<R: GridRow> WriteBatch<R> {
    fn push(&mut self, op: Op<R>) {
        self.writes.push(BatchWrite::Op(op));
    }

    /// Inserts or replaces a row by key and highlights the changed columns.
    pub fn upsert(&mut self, row: impl Into<Arc<R>>) {
        self.push(Op::Upsert(row.into()));
    }

    /// Inserts or replaces a row by key without highlight.
    pub fn upsert_silent(&mut self, row: impl Into<Arc<R>>) {
        self.push(Op::UpsertSilent(row.into()));
    }

    /// Highlights columns of an existing row without replacing it.
    pub fn mark_changed(&mut self, key: R::Key, columns: ColumnSet) {
        self.push(Op::MarkChanged(key, columns));
    }

    /// Removes a row by key.
    pub fn remove(&mut self, key: R::Key) {
        self.push(Op::Remove(key));
    }

    /// Replaces every row, without highlight.
    pub fn replace_all(&mut self, rows: impl IntoIterator<Item = Arc<R>>) {
        self.push(Op::ReplaceAll(rows.into_iter().collect()));
    }

    /// Removes every row.
    pub fn clear(&mut self) {
        self.push(Op::Clear);
    }

    /// Replaces the rows pinned at `edge`; see [`GridWriter::set_pinned_rows`].
    pub fn set_pinned_rows(&mut self, edge: PinnedEdge, rows: impl IntoIterator<Item = Arc<R>>) {
        self.push(Op::SetPinnedRows {
            edge,
            rows: rows.into_iter().collect(),
        });
    }

    /// Shows (`Some`) or clears (`None`) the store's progress; never
    /// buffered by a running reload.
    pub fn set_progress(&mut self, progress: Option<GridProgress>) {
        self.push(Op::SetProgress(progress));
    }

    /// Finishes `reload` with the rows it returned, at this point of the
    /// batch: writes before it in the batch are buffered by the reload, and
    /// writes after it are published with the reloaded rows. Ignored if a
    /// newer reload superseded it, like [`Reload::finish`]. A reload of
    /// another store is finished there at once, outside the batch.
    pub fn finish_reload(&mut self, reload: Reload<R>, snapshot: impl IntoIterator<Item = Arc<R>>) {
        if !Arc::ptr_eq(&reload.shared, &self.shared) {
            reload.finish(snapshot);
            return;
        }
        let snapshot = snapshot.into_iter().collect();
        self.writes.push(BatchWrite::FinishReload(reload, snapshot));
    }
}

struct Subscriber {
    id: u64,
    changes: ChangeSet,
    wake: async_channel::Sender<()>,
}

struct Hub<R: GridRow> {
    snapshot: Arc<StoreSnapshot<R>>,
    subscribers: Vec<Subscriber>,
    next_subscriber: u64,
    batches: BatchCounters,
}

/// The ingest's batches that published a snapshot, and their times.
#[derive(Clone, Copy, Debug, Default)]
struct BatchCounters {
    count: u64,
    last: Duration,
    max: Duration,
}

/// Counters of a store's write queue and ingest, for profiling
/// ([`GridStore::stats`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct StoreStats {
    queue: QueueCounters,
    batches: BatchCounters,
}

impl StoreStats {
    /// Operations written so far, by every writer: each upsert, removal,
    /// mark, replacement, band or progress, merged or buffered by a reload.
    pub fn writes(&self) -> u64 {
        self.queue.writes
    }

    /// Writes merged into a waiting upsert of the same key instead of
    /// queued ([`WriteCoalescing::ByKey`]).
    pub fn coalesced_writes(&self) -> u64 {
        self.queue.coalesced
    }

    /// Writes the ingest has taken, merged ones included. A reload's
    /// writes count when the reload ends.
    pub fn applied_writes(&self) -> u64 {
        self.queue.applied
    }

    /// Entries waiting in the queue now.
    pub fn pending(&self) -> usize {
        self.queue.pending
    }

    /// The most entries the queue has held at once.
    pub fn peak_pending(&self) -> usize {
        self.queue.peak_pending
    }

    /// Ingest batches that published a snapshot.
    pub fn batches(&self) -> u64 {
        self.batches.count
    }

    /// CPU time of the last batch that published a snapshot: applying its
    /// writes, freeing expired stamps and making the snapshot.
    pub fn last_batch(&self) -> Duration {
        self.batches.last
    }

    /// CPU time of the slowest batch that published a snapshot.
    pub fn max_batch(&self) -> Duration {
        self.batches.max
    }
}

/// Options of a [`GridStore`].
#[derive(Clone, Copy, Debug)]
pub struct StoreOptions {
    highlight: HighlightPolicy,
    publish_interval: Duration,
    write_coalescing: WriteCoalescing,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            highlight: HighlightPolicy::default(),
            publish_interval: SNAPSHOT_INTERVAL,
            write_coalescing: WriteCoalescing::default(),
        }
    }
}

impl StoreOptions {
    /// Sets the highlight policy, removal fade included. It belongs to the
    /// store, so every view over the store fades the same way.
    pub fn with_highlight(mut self, highlight: HighlightPolicy) -> Self {
        self.highlight = highlight;
        self
    }

    /// Snapshots are published at most this often (default 8 ms, the same
    /// as a grid's default pull interval,
    /// [`GridTuning::with_pull_interval`](crate::GridTuning::with_pull_interval)),
    /// so writes arriving one by one are applied and published in batches.
    ///
    /// A published snapshot shares every page with the store, so the next
    /// batch copies each page it writes to, about 15 µs per page of 1024
    /// rows. Under a fast stream a batch writes to every page, and each
    /// publish costs a copy of the whole store: publishing more often than
    /// the grid pulls copies pages no grid reads. A shorter interval shows
    /// writes sooner in [`GridWriter::snapshot`] and [`GridWriter::flush`];
    /// a longer one halves the ingest at 100k updates/s (16 ms against 8)
    /// and delays a write on screen by up to the interval.
    pub fn with_publish_interval(mut self, interval: Duration) -> Self {
        self.publish_interval = interval;
        self
    }

    /// How writes wait for the ingest (default [`WriteCoalescing::ByKey`]:
    /// an upsert takes the place of a waiting upsert of its key, so the
    /// queue grows with the keys written, not with the writes).
    pub fn with_write_coalescing(mut self, coalescing: WriteCoalescing) -> Self {
        self.write_coalescing = coalescing;
        self
    }

    /// The highlight policy.
    pub fn highlight(&self) -> HighlightPolicy {
        self.highlight
    }

    /// How writes wait for the ingest.
    pub fn write_coalescing(&self) -> WriteCoalescing {
        self.write_coalescing
    }

    /// The minimum interval between published snapshots.
    pub fn publish_interval(&self) -> Duration {
        self.publish_interval
    }
}

struct StoreInner<R: GridRow> {
    shared: Arc<Shared<R>>,
    hub: Arc<Mutex<Hub<R>>>,
    options: StoreOptions,
    clock: Arc<dyn Clock>,
    _ingest: Task<()>,
}

impl<R: GridRow> Drop for StoreInner<R> {
    fn drop(&mut self) {
        // Close every subscription: their `changed()` returns false.
        self.hub.lock().subscribers.clear();
    }
}

/// The rows of one or more grids. Cloning shares the store.
pub struct GridStore<R: GridRow> {
    inner: Arc<StoreInner<R>>,
}

impl<R: GridRow> Clone for GridStore<R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<R: GridRow> GridStore<R> {
    /// Creates a store and starts its ingest task on the background executor.
    pub fn new(options: StoreOptions, cx: &App) -> Self {
        Self::builder(options).build(cx)
    }

    /// A store with custom strategies: clock and reload policy.
    pub fn builder(options: StoreOptions) -> GridStoreBuilder<R> {
        GridStoreBuilder {
            options,
            clock: None,
            reload: Box::new(ReplayLive),
        }
    }

    /// The current time of the store clock (stamps are measured with it).
    pub fn now(&self) -> Instant {
        self.inner.clock.now()
    }

    /// A writer for producers on any thread.
    pub fn writer(&self) -> GridWriter<R> {
        GridWriter {
            shared: self.inner.shared.clone(),
            hub: self.inner.hub.clone(),
        }
    }

    /// The latest published snapshot.
    pub fn snapshot(&self) -> Arc<StoreSnapshot<R>> {
        self.inner.hub.lock().snapshot.clone()
    }

    /// The store options.
    pub fn options(&self) -> StoreOptions {
        self.inner.options
    }

    /// Counters of the write queue and the ingest, for profiling.
    pub fn stats(&self) -> StoreStats {
        let queue = self.inner.shared.pending.lock().queue.counters();
        let batches = self.inner.hub.lock().batches;
        StoreStats { queue, batches }
    }

    /// Subscribes to new snapshots. Changes accumulate until taken.
    pub fn subscribe(&self) -> StoreSubscription<R> {
        let (wake, rx) = async_channel::bounded(1);
        let mut hub = self.inner.hub.lock();
        let id = hub.next_subscriber;
        hub.next_subscriber += 1;
        hub.subscribers.push(Subscriber {
            id,
            changes: ChangeSet::default(),
            wake,
        });
        StoreSubscription {
            hub: self.inner.hub.clone(),
            id,
            rx,
        }
    }
}

/// Builds a [`GridStore`] with custom strategies.
pub struct GridStoreBuilder<R: GridRow> {
    options: StoreOptions,
    clock: Option<Arc<dyn Clock>>,
    reload: Box<dyn ReloadPolicy<R>>,
}

impl<R: GridRow> GridStoreBuilder<R> {
    /// Measures stamps with `clock` instead of GPUI's executor clock.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Resolves reloads with `policy` instead of [`ReplayLive`].
    pub fn with_reload_policy(mut self, policy: impl ReloadPolicy<R>) -> Self {
        self.reload = Box::new(policy);
        self
    }

    /// Starts the store on the app's background executor.
    pub fn build(self, cx: &App) -> GridStore<R> {
        self.build_on(cx.background_executor().clone())
    }

    /// Starts the store on `executor`.
    pub fn build_on(self, executor: BackgroundExecutor) -> GridStore<R> {
        let Self {
            options,
            clock,
            reload,
        } = self;
        let clock: Arc<dyn Clock> = clock.unwrap_or_else(|| Arc::new(executor.clone()));
        let (wake, wake_rx) = async_channel::bounded(1);
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending::new(options.write_coalescing)),
            wake,
        });
        let mut ingest = Ingest::<R>::with_reload_policy(options.highlight, clock.now(), reload);
        let hub = Arc::new(Mutex::new(Hub {
            snapshot: ingest.snapshot(),
            subscribers: Vec::new(),
            next_subscriber: 0,
            batches: BatchCounters::default(),
        }));
        let task = executor.spawn({
            let shared = shared.clone();
            let hub = hub.clone();
            let clock = clock.clone();
            let timer = executor.clone();
            // Part of the future even before its first poll, so dropping the
            // task with the store always releases the flushes.
            let guard = IngestGuard(shared.clone());
            async move {
                let _guard = guard;
                while wake_rx.recv().await.is_ok() {
                    // Every operation written before a taken flush is in the
                    // taken writes or in an earlier batch.
                    let (writes, flushes) = {
                        let mut pending = shared.pending.lock();
                        let writes = pending.queue.take();
                        (writes, std::mem::take(&mut pending.flushes))
                    };
                    // Profiling only: the store clock may be a fake one.
                    let started = Instant::now();
                    let now = clock.now();
                    let changes = ingest.apply_writes(writes, now);
                    ingest.sweep(now);
                    let publish = !changes.is_empty();
                    if publish {
                        let snapshot = ingest.snapshot();
                        let elapsed = started.elapsed();
                        let previous = {
                            let mut hub = hub.lock();
                            let batches = &mut hub.batches;
                            batches.count += 1;
                            batches.last = elapsed;
                            batches.max = batches.max.max(elapsed);
                            let previous = std::mem::replace(&mut hub.snapshot, snapshot);
                            for subscriber in &mut hub.subscribers {
                                subscriber.changes.merge(&changes);
                                let _ = subscriber.wake.try_send(());
                            }
                            previous
                        };
                        // Free retired pages here, not under the lock.
                        drop(previous);
                    }
                    // After the publish: the version is the published one, or
                    // the current one if the batch changed nothing.
                    for flush in flushes {
                        let _ = flush.try_send(ingest.version);
                    }
                    if publish {
                        // Batch writes that arrive one by one: everything
                        // written during this pause is applied and published
                        // together.
                        timer.timer(options.publish_interval).await;
                    }
                }
            }
        });
        GridStore {
            inner: Arc::new(StoreInner {
                shared,
                hub,
                options,
                clock,
                _ingest: task,
            }),
        }
    }
}

/// A subscription to a [`GridStore`]'s snapshots.
pub struct StoreSubscription<R: GridRow> {
    hub: Arc<Mutex<Hub<R>>>,
    id: u64,
    rx: async_channel::Receiver<()>,
}

impl<R: GridRow> StoreSubscription<R> {
    /// Waits for the next snapshot. Returns false once the store is gone.
    pub async fn changed(&self) -> bool {
        self.rx.recv().await.is_ok()
    }

    /// Takes the latest snapshot and every change since the last take.
    pub fn take(&self) -> (Arc<StoreSnapshot<R>>, ChangeSet) {
        let mut hub = self.hub.lock();
        let snapshot = hub.snapshot.clone();
        let changes = hub
            .subscribers
            .iter_mut()
            .find(|subscriber| subscriber.id == self.id)
            .map(|subscriber| std::mem::take(&mut subscriber.changes))
            .unwrap_or_default();
        (snapshot, changes)
    }
}

impl<R: GridRow> Drop for StoreSubscription<R> {
    fn drop(&mut self) {
        let id = self.id;
        self.hub
            .lock()
            .subscribers
            .retain(|subscriber| subscriber.id != id);
    }
}
