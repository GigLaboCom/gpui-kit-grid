//! The write queue between producers and the ingest: operations wait here
//! in order until the ingest takes them all. With [`WriteCoalescing::ByKey`]
//! an upsert of a key whose upsert still waits takes that one's place, so
//! the queue holds at most one entry per pending key between barriers,
//! however fast producers write.

use std::{hash::BuildHasher as _, mem, sync::Arc};

use rustc_hash::{FxBuildHasher, FxHashMap};

use super::{ChangeSet, Ingest, Op};
use crate::{
    highlight::{ChangeDirection, RowChange, Stamp},
    model::{ColumnIx, ColumnSet, GridColumns as _, GridRow},
};

/// How a store queues writes that arrive while its ingest is busy.
///
/// For the same writes in a batch, both publish the same rows, change
/// highlights and their directions, fading removals, pinned rows and
/// progress, and the same [`ChangeSet`], except that
/// [`ChangeSet::updated`] names a row once for writes merged into one entry
/// instead of once per write. They differ in what a stream costs: the
/// queue's memory and the ingest's work grow with the number of keys
/// written ([`Self::ByKey`]) or with the number of writes
/// ([`Self::KeepEveryWrite`]) between two batches. Neither pushes back on
/// producers: a stream of new keys, or an ingest that falls behind, still
/// grows the queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteCoalescing {
    /// An upsert of a key whose upsert still waits for the ingest takes that
    /// one's place in the queue, so a stream that outruns the ingest queues
    /// one entry per key, not per write. The writer diffs the new row
    /// against the waiting one ([`GridRow::changed_columns`] and
    /// [`GridRow::change_direction`]), so the ingest highlights each change
    /// as if it had applied every write. `remove`, `mark_changed`,
    /// `replace_all`, `clear` and the end of a reload are barriers: an
    /// upsert after one never merges into a write before it.
    #[default]
    ByKey,
    /// Every write waits in the queue in turn: under a stream faster than
    /// the ingest the queue grows with the event rate.
    KeepEveryWrite,
}

/// One entry of the queue.
pub(super) enum Queued<R: GridRow> {
    /// An operation as written.
    Op(Op<R>),
    /// Upserts of one key merged into the place of the first: an index into
    /// [`TakenWrites::merges`].
    Merged(u32),
}

/// Upserts of one key merged into one entry: the first as written, the
/// latest row, and what the writes after the first changed.
pub(super) struct MergedUpsert<R: GridRow> {
    first: Arc<R>,
    /// The first was an `upsert`, not an `upsert_silent`.
    highlight: bool,
    latest: Arc<R>,
    changes: MergedChanges,
}

/// What the writes after the first of a merged entry changed, each against
/// the one before it, as the ingest would have found applying them in turn.
#[derive(Default)]
struct MergedChanges {
    /// Columns any of them changed ([`ChangeSet::columns`]).
    columns: ColumnSet,
    /// Columns a highlighted one changed: stamped `Updated`.
    stamped: ColumnSet,
    /// Of `stamped`, the columns whose last change rose, and fell.
    up: ColumnSet,
    down: ColumnSet,
    /// One of them moved the row to another parent.
    parent: bool,
}

impl MergedChanges {
    /// Records a replacement of `previous` by `row`.
    fn record<R: GridRow>(&mut self, row: &R, previous: &R, highlight: bool) {
        let mut columns = ColumnSet::EMPTY;
        row.changed_columns(previous, &mut columns);
        self.parent |= row.parent_key() != previous.parent_key();
        self.columns.union_with(columns);
        if !highlight {
            return;
        }
        let all = R::Column::all();
        // Only the row's own columns are stamped (`SparseStamps::written`).
        let columns = columns.intersection(ColumnSet::first(all.len()));
        self.stamped.union_with(columns);
        for column in columns.iter() {
            let direction = row.change_direction(previous, all[column.get()]);
            self.up.remove(column);
            self.down.remove(column);
            match direction {
                Some(ChangeDirection::Up) => self.up.insert(column),
                Some(ChangeDirection::Down) => self.down.insert(column),
                None => {}
            }
        }
    }

    fn direction(&self, column: ColumnIx) -> Option<ChangeDirection> {
        if self.up.contains(column) {
            Some(ChangeDirection::Up)
        } else if self.down.contains(column) {
            Some(ChangeDirection::Down)
        } else {
            None
        }
    }
}

/// The queue's counters ([`StoreStats`](super::StoreStats)).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct QueueCounters {
    pub(super) writes: u64,
    pub(super) coalesced: u64,
    pub(super) applied: u64,
    pub(super) pending: usize,
    pub(super) peak_pending: usize,
}

/// The operations waiting for the ingest, in order.
pub(super) struct WriteQueue<R: GridRow> {
    coalescing: WriteCoalescing,
    entries: Vec<Queued<R>>,
    merges: Vec<MergedUpsert<R>>,
    /// The entry of the latest waiting upsert of each key, by the key's
    /// hash: the key is compared with the entry's row, so a collision only
    /// keeps two entries apart. No key is cloned.
    upserts: FxHashMap<u64, u32>,
    /// Entries before this one are behind a barrier: nothing merges into
    /// them.
    floor: u32,
    /// Writes in `entries`, merged ones and a reload's included.
    queued: u64,
    counters: QueueCounters,
}

/// The entries the ingest took, with the merged upserts they refer to.
pub(super) struct TakenWrites<R: GridRow> {
    entries: Vec<Queued<R>>,
    merges: Vec<MergedUpsert<R>>,
}

impl<R: GridRow> WriteQueue<R> {
    pub(super) fn new(coalescing: WriteCoalescing) -> Self {
        Self {
            coalescing,
            entries: Vec::new(),
            merges: Vec::new(),
            upserts: FxHashMap::default(),
            floor: 0,
            queued: 0,
            counters: QueueCounters::default(),
        }
    }

    pub(super) fn counters(&self) -> QueueCounters {
        QueueCounters {
            pending: self.entries.len(),
            ..self.counters
        }
    }

    /// Counts a write a running reload buffers instead.
    pub(super) fn count_buffered(&mut self) {
        self.counters.writes += 1;
    }

    /// Queues a written operation, merging an upsert into the waiting
    /// upsert of its key.
    pub(super) fn push(&mut self, op: Op<R>) {
        self.counters.writes += 1;
        self.queued += 1;
        match op {
            Op::Upsert(row) => self.push_upsert(row, true),
            Op::UpsertSilent(row) => self.push_upsert(row, false),
            Op::Remove(key) => {
                self.forget(&key);
                self.push_entry(Op::Remove(key));
            }
            Op::MarkChanged(key, columns) => {
                self.forget(&key);
                self.push_entry(Op::MarkChanged(key, columns));
            }
            op @ (Op::SetPinnedRows { .. } | Op::SetProgress(_)) => self.push_entry(op),
            op => {
                self.barrier();
                self.push_entry(op);
            }
        }
    }

    /// Queues the end of a reload: a barrier, then the reload with the
    /// operations it buffered (written already, counted now).
    pub(super) fn push_finished_reload(&mut self, snapshot: Vec<Arc<R>>, live: Vec<Op<R>>) {
        self.queued += live.len() as u64;
        self.barrier();
        self.push_entry(Op::FinishReload { snapshot, live });
    }

    /// Queues what a cancelled reload buffered, as written: a barrier
    /// first, so nothing written before merges past them.
    pub(super) fn push_cancelled_reload(&mut self, live: Vec<Op<R>>) {
        self.queued += live.len() as u64;
        self.barrier();
        self.entries.extend(live.into_iter().map(Queued::Op));
        self.note_depth();
    }

    /// Takes every entry for the ingest.
    pub(super) fn take(&mut self) -> TakenWrites<R> {
        self.upserts.clear();
        self.floor = 0;
        self.counters.applied += mem::take(&mut self.queued);
        TakenWrites {
            entries: mem::take(&mut self.entries),
            merges: mem::take(&mut self.merges),
        }
    }

    fn push_entry(&mut self, op: Op<R>) {
        self.entries.push(Queued::Op(op));
        self.note_depth();
    }

    fn note_depth(&mut self) {
        self.counters.peak_pending = self.counters.peak_pending.max(self.entries.len());
    }

    /// Nothing written so far merges with what comes next.
    fn barrier(&mut self) {
        self.floor = self.entries.len() as u32;
    }

    /// No later upsert of `key` merges into an earlier one.
    fn forget(&mut self, key: &R::Key) {
        if self.coalescing == WriteCoalescing::ByKey {
            self.upserts.remove(&FxBuildHasher.hash_one(key));
        }
    }

    fn push_upsert(&mut self, row: Arc<R>, highlight: bool) {
        let hash = match self.coalescing {
            WriteCoalescing::ByKey => FxBuildHasher.hash_one(row.key()),
            WriteCoalescing::KeepEveryWrite => {
                self.push_entry(upsert(row, highlight));
                return;
            }
        };
        let row = match self.upserts.get(&hash) {
            Some(&ix) if ix >= self.floor => match self.merge(ix as usize, row, highlight) {
                Ok(()) => {
                    self.counters.coalesced += 1;
                    return;
                }
                Err(row) => row,
            },
            _ => row,
        };
        self.upserts.insert(hash, self.entries.len() as u32);
        self.push_entry(upsert(row, highlight));
    }

    /// Merges `row` into the upsert at `ix` if it is one of the same key;
    /// gives the row back otherwise.
    fn merge(&mut self, ix: usize, row: Arc<R>, highlight: bool) -> Result<(), Arc<R>> {
        let (first, first_highlight) = match &self.entries[ix] {
            Queued::Merged(merge_ix) => {
                let merge = &mut self.merges[*merge_ix as usize];
                if merge.latest.key() != row.key() {
                    return Err(row);
                }
                // The same row again changes nothing, as in the ingest.
                if !Arc::ptr_eq(&merge.latest, &row) {
                    merge.changes.record(&*row, &*merge.latest, highlight);
                    merge.latest = row;
                }
                return Ok(());
            }
            Queued::Op(Op::Upsert(first)) => (first, true),
            Queued::Op(Op::UpsertSilent(first)) => (first, false),
            Queued::Op(_) => return Err(row),
        };
        if first.key() != row.key() {
            return Err(row);
        }
        if Arc::ptr_eq(first, &row) {
            return Ok(());
        }
        let mut changes = MergedChanges::default();
        changes.record(&*row, &**first, highlight);
        let first = first.clone();
        self.entries[ix] = Queued::Merged(self.merges.len() as u32);
        self.merges.push(MergedUpsert {
            first,
            highlight: first_highlight,
            latest: row,
            changes,
        });
        Ok(())
    }
}

fn upsert<R: GridRow>(row: Arc<R>, highlight: bool) -> Op<R> {
    if highlight {
        Op::Upsert(row)
    } else {
        Op::UpsertSilent(row)
    }
}

impl<R: GridRow> Ingest<R> {
    /// Applies the entries the ingest took, in order: each merged upsert in
    /// the place of its first write.
    pub(super) fn apply_writes(
        &mut self,
        writes: TakenWrites<R>,
        now: web_time::Instant,
    ) -> ChangeSet {
        let now_ms = self.now_ms(now);
        let mut changes = ChangeSet::default();
        let TakenWrites { entries, merges } = writes;
        for entry in entries {
            match entry {
                Queued::Op(op) => self.apply_op(op, now_ms, &mut changes),
                Queued::Merged(ix) => self.apply_merged(&merges[ix as usize], now_ms, &mut changes),
            }
        }
        self.finish_batch(now_ms, changes)
    }

    /// The first write as written, against the stored row or as an insert;
    /// then the latest row in its place with what the later writes changed,
    /// each against the one before: the stamps, directions and changes of
    /// applying them in turn.
    fn apply_merged(&mut self, merge: &MergedUpsert<R>, now_ms: u32, changes: &mut ChangeSet) {
        self.upsert(merge.first.clone(), now_ms, merge.highlight, changes);
        let Some(&id) = self.index.get(merge.latest.key()) else {
            return;
        };
        let ix = id.ix();
        self.slot_mut(ix).row = Some(merge.latest.clone());
        let merged = &merge.changes;
        changes.parent |= merged.parent;
        changes.updated.push(id);
        changes.columns.union_with(merged.columns);
        if !merged.stamped.is_empty() {
            self.stamp(ix, merged.stamped, now_ms, |column| {
                Stamp::with_direction(now_ms, RowChange::Updated, merged.direction(column))
            });
        }
    }
}
