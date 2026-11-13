//! Commits waiting for their answer or for the store: the optimistic value
//! a cell shows meanwhile, the error mark of a refused commit, and the same
//! values for a copy and an export in the background.
//!
//! A commit (`LiveGridEvent::EditCommit`) is pending until the application
//! answers it with `LiveGridState::resolve_edit`. `Ok` makes it accepted:
//! the cell keeps showing the value until the store has it, or the store
//! says otherwise, or `EditPolicy::settle_timeout` passes. `Err` drops it
//! and marks the cell with the error. Sorting, filtering, grouping and
//! aggregates read the store, never these values.

use std::{hash::Hash, sync::Arc, time::Duration};

use gpui_kit::{Context, Task};
use rustc_hash::FxHashMap;
use web_time::Instant;

use super::{
    ColumnOf, KeyOf, LiveGridDelegate, LiveGridState,
    edit::{EditError, EditId, EditState},
};
use crate::{
    model::{GridRow, OwnedValue},
    store::StoreSnapshot,
};

/// The optimistic values of a grid at one moment, by row key: what a cell
/// shows while its commit waits. Immutable, so a background copy or export
/// reads the values of the moment its command ran.
pub(crate) struct PendingValues<K, C> {
    rows: FxHashMap<K, Box<[(C, OwnedValue)]>>,
}

impl<K, C> Default for PendingValues<K, C> {
    fn default() -> Self {
        Self {
            rows: FxHashMap::default(),
        }
    }
}

impl<K: Eq + Hash, C> PendingValues<K, C> {
    /// Returns true if no cell waits: O(1).
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The optimistic values of the row of `key`, by column.
    pub(crate) fn get(&self, key: &K) -> Option<&[(C, OwnedValue)]> {
        self.rows.get(key).map(|values| &**values)
    }
}

/// One commit of a cell that waits.
struct PendingEdit {
    id: EditId,
    value: OwnedValue,
    /// `Pending` or `Accepted`.
    state: EditState,
    /// When the commit was accepted, and the store's value then.
    accepted: Option<(Instant, OwnedValue)>,
}

/// The commits of one cell that wait, oldest first, and the mark of its
/// latest refused commit.
struct CellEdits<C> {
    column: C,
    edits: Vec<PendingEdit>,
    /// `Failed`, and when the mark goes by itself.
    failed: Option<(EditState, Option<Instant>)>,
}

impl<C> CellEdits<C> {
    fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.failed.is_none()
    }

    /// The newest commit that waits: what the cell shows.
    fn newest(&self) -> Option<&PendingEdit> {
        self.edits.last()
    }
}

/// What a cell's commits mark it with in a frame.
#[derive(Clone, Copy)]
pub(super) struct CellEditMarks<'a> {
    /// The value the newest waiting commit shows.
    pub(super) value: Option<&'a OwnedValue>,
    /// The state the cell reads as ([`LiveGridState::edit_state`]).
    pub(super) state: Option<&'a EditState>,
    /// The newest commit still waits for an answer: its value is muted.
    pub(super) pending: bool,
    /// A commit of the cell was refused: the error mark shows.
    pub(super) failed: bool,
}

/// The commits of a grid that wait for `LiveGridState::resolve_edit` or for
/// the store, and the cells' error marks.
pub(super) struct PendingEdits<K, C> {
    rows: FxHashMap<K, Vec<CellEdits<C>>>,
    /// Where each waiting commit is.
    ids: FxHashMap<EditId, (K, C)>,
    /// The values of the newest waiting commits, for a copy or an export;
    /// made again when they change, never per frame.
    values: Arc<PendingValues<K, C>>,
    /// The deadline the timer waits for, and the timer.
    timer: Option<(Instant, Task<()>)>,
}

impl<K, C> Default for PendingEdits<K, C> {
    fn default() -> Self {
        Self {
            rows: FxHashMap::default(),
            ids: FxHashMap::default(),
            values: Arc::new(PendingValues::default()),
            timer: None,
        }
    }
}

impl<K: Clone + Eq + Hash, C: PartialEq + Copy> PendingEdits<K, C> {
    /// Returns true if no cell has a commit or a mark: a frame then checks
    /// no cell.
    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn cell(&self, key: &K, column: C) -> Option<&CellEdits<C>> {
        self.rows
            .get(key)?
            .iter()
            .find(|cell| cell.column == column)
    }

    fn cell_mut(&mut self, key: &K, column: C) -> &mut CellEdits<C> {
        let cells = self.rows.entry(key.clone()).or_default();
        let ix = match cells.iter().position(|cell| cell.column == column) {
            Some(ix) => ix,
            None => {
                cells.push(CellEdits {
                    column,
                    edits: Vec::new(),
                    failed: None,
                });
                cells.len() - 1
            }
        };
        &mut cells[ix]
    }

    /// The value the cell of `key` and `column` shows while its newest
    /// commit waits.
    pub(super) fn overlay(&self, key: &K, column: C) -> Option<&OwnedValue> {
        Some(&self.cell(key, column)?.newest()?.value)
    }

    /// What the cell's commits mark it with; `None` for a cell with none.
    pub(super) fn marks(&self, key: &K, column: C) -> Option<CellEditMarks<'_>> {
        let cell = self.cell(key, column)?;
        let newest = cell.newest();
        Some(CellEditMarks {
            value: newest.map(|edit| &edit.value),
            state: newest
                .map(|edit| &edit.state)
                .or_else(|| cell.failed.as_ref().map(|(state, _)| state)),
            pending: newest.is_some_and(|edit| edit.state == EditState::Pending),
            failed: cell.failed.is_some(),
        })
    }

    /// The state the cell reads as: its newest waiting commit's, else its
    /// error mark.
    pub(super) fn state(&self, key: &K, column: C) -> Option<&EditState> {
        self.marks(key, column)?.state
    }

    /// Commits that wait for an answer or for the store.
    pub(super) fn count(&self) -> usize {
        self.ids.len()
    }

    /// Adds a commit (after its `EditCommit`): the cell shows `value` until
    /// it settles. The cell's error mark goes: it is edited again.
    pub(super) fn begin(&mut self, id: EditId, key: K, column: C, value: OwnedValue) {
        let cell = self.cell_mut(&key, column);
        cell.failed = None;
        cell.edits.push(PendingEdit {
            id,
            value,
            state: EditState::Pending,
            accepted: None,
        });
        self.ids.insert(id, (key, column));
        self.publish();
    }

    /// Answers a commit: `Ok` accepts it at `now`, with the store's value
    /// then; `Err` drops it and marks the cell, the mark going at
    /// `mark_until`. Returns false for an unknown, answered or settled id.
    pub(super) fn resolve(
        &mut self,
        id: EditId,
        result: Result<(), EditError>,
        now: Instant,
        store_value: impl FnOnce(&K, C) -> Option<OwnedValue>,
        mark_until: Option<Instant>,
    ) -> bool {
        let Some((key, column)) = self.ids.get(&id).cloned() else {
            return false;
        };
        let store_value = store_value(&key, column);
        let cell = self.cell_mut(&key, column);
        let Some(ix) = cell
            .edits
            .iter()
            .position(|edit| edit.id == id && edit.state == EditState::Pending)
        else {
            return false;
        };
        match result {
            Ok(()) => {
                let edit = &mut cell.edits[ix];
                edit.state = EditState::Accepted;
                edit.accepted = Some((now, store_value.unwrap_or_else(|| edit.value.clone())));
            }
            Err(error) => {
                cell.edits.remove(ix);
                cell.failed = Some((EditState::Failed(error), mark_until));
                self.ids.remove(&id);
            }
        }
        self.publish();
        true
    }

    /// Takes the error mark off a cell. Returns false if it had none.
    pub(super) fn clear_failed(&mut self, key: &K, column: C) -> bool {
        let Some(cells) = self.rows.get_mut(key) else {
            return false;
        };
        let Some(cell) = cells.iter_mut().find(|cell| cell.column == column) else {
            return false;
        };
        if cell.failed.take().is_none() {
            return false;
        }
        self.prune();
        true
    }

    /// Settles what the store and the clock decide: rows that left the store
    /// take their commits and marks with them; an accepted commit goes when
    /// the store has its value, when the store's value changed since the
    /// answer (the store said its own), or at `now` past `settle`; a mark
    /// goes past its deadline. Returns true if anything went.
    pub(super) fn follow<R>(
        &mut self,
        snapshot: &StoreSnapshot<R>,
        now: Instant,
        settle: Duration,
    ) -> bool
    where
        R: GridRow<Key = K, Column = C>,
        C: crate::model::GridColumns,
    {
        let before = (self.ids.len(), self.marks_count());
        let mut gone: Vec<EditId> = Vec::new();
        self.rows.retain(|key, cells| {
            let Some(row) = snapshot.row_id(key).and_then(|id| snapshot.row(id)) else {
                gone.extend(
                    cells
                        .iter()
                        .flat_map(|cell| cell.edits.iter().map(|edit| edit.id)),
                );
                return false;
            };
            for cell in cells.iter_mut() {
                if cell.edits.iter().any(|edit| edit.accepted.is_some()) {
                    let store = OwnedValue::from_cell(row.value(cell.column));
                    cell.edits.retain(|edit| {
                        let Some((at, at_answer)) = &edit.accepted else {
                            return true;
                        };
                        let settled = store.cmp_total(&edit.value).is_eq()
                            || !store.cmp_total(at_answer).is_eq()
                            || now >= *at + settle;
                        if settled {
                            gone.push(edit.id);
                        }
                        !settled
                    });
                }
                if cell
                    .failed
                    .as_ref()
                    .is_some_and(|(_, until)| until.is_some_and(|until| now >= until))
                {
                    cell.failed = None;
                }
            }
            cells.retain(|cell| !cell.is_empty());
            !cells.is_empty()
        });
        for id in gone {
            self.ids.remove(&id);
        }
        let changed = before != (self.ids.len(), self.marks_count());
        if changed {
            self.publish();
        }
        changed
    }

    fn marks_count(&self) -> usize {
        self.rows
            .values()
            .flatten()
            .filter(|cell| cell.failed.is_some())
            .count()
    }

    /// The nearest instant `follow` has something to do at, given `settle`:
    /// an accepted commit's settle deadline or a mark's.
    pub(super) fn next_deadline(&self, settle: Duration) -> Option<Instant> {
        self.rows
            .values()
            .flatten()
            .flat_map(|cell| {
                cell.edits
                    .iter()
                    .filter_map(|edit| edit.accepted.as_ref().map(|(at, _)| *at + settle))
                    .chain(cell.failed.as_ref().and_then(|(_, until)| *until))
            })
            .min()
    }

    /// Drops empty cells and rows, then publishes the values.
    fn prune(&mut self) {
        self.rows.retain(|_, cells| {
            cells.retain(|cell| !cell.is_empty());
            !cells.is_empty()
        });
        self.publish();
    }

    /// Makes the values a copy or an export reads again.
    fn publish(&mut self) {
        self.rows.retain(|_, cells| {
            cells.retain(|cell| !cell.is_empty());
            !cells.is_empty()
        });
        let rows = self
            .rows
            .iter()
            .filter_map(|(key, cells)| {
                let values: Box<[(C, OwnedValue)]> = cells
                    .iter()
                    .filter_map(|cell| Some((cell.column, cell.newest()?.value.clone())))
                    .collect();
                (!values.is_empty()).then(|| (key.clone(), values))
            })
            .collect();
        self.values = Arc::new(PendingValues { rows });
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Answers a commit (`LiveGridEvent::EditCommit::edit`). `Ok`: the
    /// value was applied; the cell shows it until the store has it too, the
    /// store says its own, or `EditPolicy::settle_timeout` passes. `Err`:
    /// the cell goes back to the store's value and shows the error until
    /// the cell is edited again, `clear_edit_error`, or
    /// `EditPolicy::error_mark_timeout`. Returns false for an unknown id,
    /// one answered already, or one settled: a removed row's commits are
    /// dropped.
    pub fn resolve_edit(
        &mut self,
        edit: EditId,
        result: Result<(), EditError>,
        cx: &mut Context<Self>,
    ) -> bool {
        let now = self.store.now();
        let mark_until = self
            .edit_setup
            .policy()
            .error_mark_timeout()
            .map(|timeout| now + timeout);
        let snapshot = self.snapshot.clone();
        let resolved = self.pending_edits.resolve(
            edit,
            result,
            now,
            |key, column| {
                let row = snapshot.row(snapshot.row_id(key)?)?;
                Some(OwnedValue::from_cell(row.value(column)))
            },
            mark_until,
        );
        if resolved {
            // A settle timeout of zero shows the store's value at once.
            self.follow_pending_edits(cx);
            cx.notify();
        }
        resolved
    }

    /// What a cell's edits look like now: waiting for an answer, accepted
    /// and waiting for the store, or failed. `None` when the cell shows the
    /// store's value with no error.
    pub fn edit_state(&self, key: &KeyOf<D>, column: ColumnOf<D>) -> Option<&EditState> {
        self.pending_edits.state(key, column)
    }

    /// Commits not answered yet, or accepted and waiting for the store.
    pub fn pending_edit_count(&self) -> usize {
        self.pending_edits.count()
    }

    /// Takes the error mark off a cell (`EditState::Failed`); the next edit
    /// of the cell does it too. Returns false if the cell had none.
    pub fn clear_edit_error(
        &mut self,
        key: &KeyOf<D>,
        column: ColumnOf<D>,
        cx: &mut Context<Self>,
    ) -> bool {
        let cleared = self.pending_edits.clear_failed(key, column);
        if cleared {
            cx.notify();
        }
        cleared
    }

    /// The optimistic values now: what cells with a waiting commit show, for
    /// a copy and an export, which write what the screen shows. O(1): the
    /// set is shared, not copied.
    pub(crate) fn pending_values(&self) -> Arc<PendingValues<super::KeyOf<D>, super::ColumnOf<D>>> {
        self.pending_edits.values.clone()
    }

    /// Sets the optimistic values as waiting commits would, for a test of a
    /// copy or an export without real commits: the newest value of a cell
    /// wins.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_pending_values(
        &mut self,
        values: impl IntoIterator<Item = (super::KeyOf<D>, super::ColumnOf<D>, OwnedValue)>,
        cx: &mut Context<Self>,
    ) {
        let mut rows: FxHashMap<_, Vec<(super::ColumnOf<D>, OwnedValue)>> = FxHashMap::default();
        for (key, column, value) in values {
            let row = rows.entry(key).or_default();
            row.retain(|(edited, _)| *edited != column);
            row.push((column, value));
        }
        let rows = rows
            .into_iter()
            .map(|(key, values)| (key, values.into_boxed_slice()))
            .collect();
        self.pending_edits.values = Arc::new(PendingValues { rows });
        cx.notify();
    }

    /// Adds a commit that went out: its cell shows the value until it
    /// settles.
    pub(super) fn begin_pending_edit(
        &mut self,
        edit: EditId,
        key: KeyOf<D>,
        column: ColumnOf<D>,
        value: OwnedValue,
        cx: &mut Context<Self>,
    ) {
        self.pending_edits.begin(edit, key, column, value);
        cx.notify();
    }

    /// Settles the commits the store or the clock decided, after a new
    /// snapshot or at a deadline, and waits for the next deadline with one
    /// timer on the store's clock, as the highlight fade does.
    pub(super) fn follow_pending_edits(&mut self, cx: &mut Context<Self>) {
        if self.pending_edits.is_empty() {
            self.pending_edits.timer = None;
            return;
        }
        let now = self.store.now();
        let settle = self.edit_setup.policy().settle_timeout();
        if self.pending_edits.follow(&self.snapshot, now, settle) {
            cx.notify();
        }
        let next = self.pending_edits.next_deadline(settle);
        match next {
            None => self.pending_edits.timer = None,
            Some(deadline)
                if self
                    .pending_edits
                    .timer
                    .as_ref()
                    .is_some_and(|(at, _)| *at == deadline) => {}
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(now);
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _ = this.update(cx, |state, cx| {
                        state.pending_edits.timer = None;
                        state.follow_pending_edits(cx);
                    });
                });
                self.pending_edits.timer = Some((deadline, task));
            }
        }
    }

    /// A new order was shown: an edited row it no longer shows ends as
    /// `EditPolicy::on_hidden` decides.
    pub(super) fn follow_edit_shown(&mut self, cx: &mut Context<Self>) {
        self.end_hidden_edit(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pending_edits_stack() {
        let now = Instant::now();
        let mut pending = PendingEdits::<u32, u16>::default();
        assert!(pending.is_empty());
        assert_eq!(pending.overlay(&1, 0), None);
        assert!(pending.values.is_empty());

        // Two commits in one cell: the newer shows; both count.
        pending.begin(EditId::new(1), 1, 0, OwnedValue::Int(5));
        pending.begin(EditId::new(2), 1, 0, OwnedValue::Int(6));
        assert_eq!(pending.overlay(&1, 0), Some(&OwnedValue::Int(6)));
        assert_eq!(pending.count(), 2);
        assert_eq!(pending.state(&1, 0), Some(&EditState::Pending));
        assert_eq!(pending.values.get(&1), Some(&[(0, OwnedValue::Int(6))][..]));
        assert_eq!(pending.overlay(&1, 1), None);
        assert_eq!(pending.overlay(&2, 0), None);

        // The older refused: the mark shows, the newer still shows.
        let error = EditError::new("Price out of band");
        assert!(pending.resolve(EditId::new(1), Err(error.clone()), now, |_, _| None, None));
        let marks = pending.marks(&1, 0).expect("marks");
        assert!(marks.failed && marks.pending);
        assert_eq!(marks.value, Some(&OwnedValue::Int(6)));
        assert_eq!(pending.count(), 1);
        // The newer accepted, then its answer again: unknown.
        assert!(pending.resolve(
            EditId::new(2),
            Ok(()),
            now,
            |_, _| Some(OwnedValue::Int(1)),
            None
        ));
        assert_eq!(pending.state(&1, 0), Some(&EditState::Accepted));
        assert!(!pending.resolve(EditId::new(7), Ok(()), now, |_, _| None, None));
        assert_eq!(
            pending.next_deadline(Duration::from_secs(5)),
            Some(now + Duration::from_secs(5))
        );
        // The mark goes by a call; the cell keeps its accepted value.
        assert!(pending.clear_failed(&1, 0));
        assert!(!pending.clear_failed(&1, 0));
        assert_eq!(pending.overlay(&1, 0), Some(&OwnedValue::Int(6)));
    }
}
