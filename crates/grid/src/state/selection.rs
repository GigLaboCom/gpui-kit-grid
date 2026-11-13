//! Key-based selection: the selection mode, the selected rows or cells, the
//! focused cell and the anchor. All of them are held by row key and column,
//! so they survive re-sorting, filtering and the update stream.
//!
//! [`SelectionModel`] works on the projection on screen and its snapshot and
//! needs no window, so it is tested without one. Every branch on
//! [`SelectionMode`] lives in this module: painting, Actions and the pointer
//! ask the model (`columns(key)`, an edit) instead of the mode.
//!
//! Below the model, the [`LiveGridState`] side: builders and readers, the
//! keyboard Actions, pointer presses and drags, programmatic changes, and
//! their events (one `SelectionChange`, then one `FocusChange`, per action).

use std::{fmt, hash::Hash, ops::RangeInclusive, sync::Arc};

use gpui_kit::base::AutoScroll;
use gpui_kit::base::actions::{
    Cancel, Confirm, SelectDown, SelectFirst, SelectLast, SelectNextColumn, SelectPageDown,
    SelectPageUp, SelectPrevColumn, SelectUp,
};
use gpui_kit::{
    Context, InteractiveElement, Modifiers, MouseButton, MouseDownEvent, Pixels, Point, px,
};
use rustc_hash::FxHashMap;

use super::{
    ColumnOf, KeyOf, LiveGridDelegate, LiveGridEvent, LiveGridState, SelectionOrigin, ViewColumnIx,
    a11y::SelectionUnit,
    columns::{ColumnLayout, column_of},
    scroll::page_size,
};
use crate::{
    actions::{
        ExtendSelectionDown, ExtendSelectionNextColumn, ExtendSelectionPageDown,
        ExtendSelectionPageUp, ExtendSelectionPrevColumn, ExtendSelectionToFirst,
        ExtendSelectionToFirstCell, ExtendSelectionToFirstRow, ExtendSelectionToLast,
        ExtendSelectionToLastCell, ExtendSelectionToLastRow, ExtendSelectionUp, SelectAll,
        SelectFirstCell, SelectFirstRow, SelectLastCell, SelectLastRow, ToggleSelection,
    },
    engine::{DisplayRow, GroupInfo, GroupPath, Projection},
    model::{ColumnIx, ColumnSet, GridColumns, GridRow},
    policy::{
        ContextMenuGesture, ContextMenuGestureContext, ExpandArea, FocusCaptions, GestureInput,
        GridArea, LineScopeContext, SelectableLine, SelectionEdit, SelectionGesture,
        SelectionGestureContext, SelectionPolicy, SelectionScope, SelectionScopeContext,
    },
    store::{PinnedEdge, RowId, StoreSnapshot},
};

/// A pointer press turns into a drag past this distance on either axis
/// (GPUI's drag threshold).
const DRAG_THRESHOLD: Pixels = px(2.);

/// Returns true if the pointer at `position` is past the drag threshold from
/// the press at `origin`: the press is a drag, not a click.
pub(super) fn is_past_drag_threshold(origin: Point<Pixels>, position: Point<Pixels>) -> bool {
    (position.x - origin.x).abs() > DRAG_THRESHOLD || (position.y - origin.y).abs() > DRAG_THRESHOLD
}

/// What a click or a key press selects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionMode {
    /// One row at a time (the default).
    #[default]
    RowSingle,
    /// Any set of rows.
    RowSet,
    /// One cell at a time.
    CellSingle,
    /// Any set of cells.
    CellSet,
}

impl SelectionMode {
    fn selects_cells(self) -> bool {
        matches!(self, Self::CellSingle | Self::CellSet)
    }

    /// One row or one cell at a time; otherwise the grid is
    /// multiselectable for accessibility.
    pub(super) fn is_single(self) -> bool {
        matches!(self, Self::RowSingle | Self::CellSingle)
    }

    /// What one selected item is for accessibility: rows carry `selected`
    /// in the row modes, cells in the cell modes.
    pub(super) fn selection_unit(self) -> SelectionUnit {
        if self.selects_cells() {
            SelectionUnit::Cell
        } else {
            SelectionUnit::Row
        }
    }
}

/// The selected rows or cells, by key. Cloning is O(1): the set is shared
/// and copied on write.
#[derive(Clone)]
pub struct GridSelection<K> {
    rows: Arc<FxHashMap<K, ColumnSet>>,
}

impl<K> Default for GridSelection<K> {
    fn default() -> Self {
        Self {
            rows: Arc::default(),
        }
    }
}

impl<K: fmt::Debug> fmt::Debug for GridSelection<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A selection may hold every row: print its rows only when few.
        let mut debug = f.debug_struct("GridSelection");
        debug.field("len", &self.rows.len());
        if self.rows.len() <= 8 {
            debug.field("rows", &*self.rows);
        }
        debug.finish()
    }
}

impl<K: Eq + Hash> GridSelection<K> {
    fn from_map(rows: FxHashMap<K, ColumnSet>) -> Self {
        Self {
            rows: Arc::new(rows),
        }
    }

    /// A selection of these rows and their columns; rows with no columns
    /// are left out.
    pub(crate) fn from_rows(rows: impl IntoIterator<Item = (K, ColumnSet)>) -> Self {
        Self::from_map(
            rows.into_iter()
                .filter(|(_, columns)| !columns.is_empty())
                .collect(),
        )
    }

    /// Rows with a selected row or at least one selected cell.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns true if nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns true if the row is selected, or one of its cells is.
    pub fn contains(&self, key: &K) -> bool {
        self.rows.contains_key(key)
    }

    /// Returns true if the cell is selected; in the row modes, if its row is.
    pub fn contains_cell(&self, key: &K, column: ColumnIx) -> bool {
        self.columns(key).contains(column)
    }

    /// The selected columns of a row: every schema column in the row modes,
    /// `ColumnSet::EMPTY` if the row has nothing selected.
    pub fn columns(&self, key: &K) -> ColumnSet {
        self.rows.get(key).copied().unwrap_or(ColumnSet::EMPTY)
    }

    /// The selected rows in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &K> + '_ {
        self.rows.keys()
    }

    /// Returns true if both select the same rows and cells.
    fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.rows, &other.rows)
            || (self.rows.len() == other.rows.len()
                && self
                    .rows
                    .iter()
                    .all(|(key, columns)| other.rows.get(key) == Some(columns)))
    }
}

impl<K: Clone + Eq + Hash> GridSelection<K> {
    /// The rows to change; copied first if another clone shares them.
    fn rows_mut(&mut self) -> &mut FxHashMap<K, ColumnSet> {
        Arc::make_mut(&mut self.rows)
    }

    /// Adds every row and cell of `other`; returns true if any was new.
    fn union_with(&mut self, other: &Self) -> bool {
        let new = other.rows.iter().any(|(key, columns)| {
            let mut union = self.columns(key);
            union.union_with(*columns);
            union != self.columns(key)
        });
        if new {
            let rows = self.rows_mut();
            for (key, columns) in other.rows.iter() {
                rows.entry(key.clone()).or_default().union_with(*columns);
            }
        }
        new
    }

    /// Drops the keys `missing` says are gone; returns true if any was.
    fn retain(&mut self, missing: impl Fn(&K) -> bool) -> bool {
        let gone = self.rows.keys().any(&missing);
        if gone {
            self.rows_mut().retain(|key, _| !missing(key));
        }
        gone
    }

    /// Flips one item of the row `key`: `item` is its columns (a row's or
    /// a cell's), `column` the one that tells whether it is selected.
    fn flip(&mut self, key: &K, column: ColumnIx, item: ColumnSet) {
        let selected = self.columns(key);
        let rows = self.rows_mut();
        if selected.contains(column) {
            let rest = difference(selected, item);
            if rest.is_empty() {
                rows.remove(key);
            } else {
                rows.insert(key.clone(), rest);
            }
        } else {
            let mut columns = selected;
            columns.union_with(item);
            rows.insert(key.clone(), columns);
        }
    }
}

/// The focused cell: the keyboard target inside the grid, a cell of a data
/// row or a group caption.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusedCell<K, C> {
    row: Option<K>,
    group: Option<GroupPath>,
    column: C,
}

impl<K, C: Copy> FocusedCell<K, C> {
    pub(crate) fn new(row: K, column: C) -> Self {
        Self {
            row: Some(row),
            group: None,
            column,
        }
    }

    /// The caption of the group at `path`; `column` is kept for the way
    /// back to the data rows.
    pub(crate) fn caption(path: GroupPath, column: C) -> Self {
        Self {
            row: None,
            group: Some(path),
            column,
        }
    }

    /// The row; `None` on a group caption.
    pub fn row(&self) -> Option<&K> {
        self.row.as_ref()
    }

    /// The group of a focused caption; `None` on a row.
    pub fn group(&self) -> Option<&GroupPath> {
        self.group.as_ref()
    }

    /// The column. On a caption, the column the focus keeps for the data
    /// rows: arrows up and down onto a row land in it.
    pub fn column(&self) -> C {
        self.column
    }
}

/// The line a command acts on, by identity: a data row by key, a group
/// caption by path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LineTarget<K> {
    Row(K),
    Group(GroupPath),
}

impl<K: Clone, C: Copy> FocusedCell<K, C> {
    /// The focused line as a command's target.
    pub(crate) fn line(&self) -> Option<LineTarget<K>> {
        match (&self.row, &self.group) {
            (Some(key), _) => Some(LineTarget::Row(key.clone())),
            (None, Some(path)) => Some(LineTarget::Group(path.clone())),
            (None, None) => None,
        }
    }
}

/// What a command acts on: the selection within its scope, in display order
/// ([`crate::SelectionScope`]). Collect one from `(key, columns)` pairs.
#[derive(Clone, Debug)]
pub struct ScopedSelection<K> {
    keys: Arc<[K]>,
    columns: Arc<[ColumnSet]>,
}

impl<K> Default for ScopedSelection<K> {
    fn default() -> Self {
        Self {
            keys: Vec::new().into(),
            columns: Vec::new().into(),
        }
    }
}

impl<K> FromIterator<(K, ColumnSet)> for ScopedSelection<K> {
    fn from_iter<I: IntoIterator<Item = (K, ColumnSet)>>(rows: I) -> Self {
        let (keys, columns): (Vec<K>, Vec<ColumnSet>) = rows.into_iter().unzip();
        Self {
            keys: keys.into(),
            columns: columns.into(),
        }
    }
}

impl<K> ScopedSelection<K> {
    /// The rows, in display order.
    pub fn keys(&self) -> &[K] {
        &self.keys
    }

    /// The selected columns of `keys()[ix]`: every schema column in the row
    /// modes; `ColumnSet::EMPTY` past the end.
    pub fn columns(&self, ix: usize) -> ColumnSet {
        self.columns.get(ix).copied().unwrap_or(ColumnSet::EMPTY)
    }

    /// The number of rows.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Returns true if there are no rows.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The keys without a copy, for an event.
    pub(crate) fn shared_keys(&self) -> Arc<[K]> {
        self.keys.clone()
    }
}

/// What the last mouse-down on the body hit, captured at that moment: a
/// re-sort before the release, or before a menu is built, cannot retarget
/// it.
#[derive(Clone, Debug)]
pub(crate) struct PointerTarget<K> {
    /// `None`: the empty area below the rows, or a group caption. The key
    /// of a pinned row is of its band's key space (`pinned`).
    row: Option<K>,
    /// The group of a pressed caption.
    group: Option<GroupPath>,
    /// The cell's column; on a caption, the aggregate cell's, if any.
    column: Option<ColumnIx>,
    button: MouseButton,
    position: Point<Pixels>,
    /// The band of a pinned row: the press selects nothing, and its menu
    /// targets `GridTarget::Pinned`.
    pinned: Option<PinnedEdge>,
}

impl<K> PointerTarget<K> {
    pub(crate) fn row(&self) -> Option<&K> {
        self.row.as_ref()
    }

    /// The group of a pressed caption.
    pub(crate) fn group(&self) -> Option<&GroupPath> {
        self.group.as_ref()
    }

    /// The band of a pinned row; `None` for a data row or no row.
    pub(crate) fn pinned(&self) -> Option<PinnedEdge> {
        self.pinned
    }

    pub(crate) fn column(&self) -> Option<ColumnIx> {
        self.column
    }

    #[expect(dead_code, reason = "no caller reads the pressed button yet")]
    pub(crate) fn button(&self) -> MouseButton {
        self.button
    }

    pub(crate) fn position(&self) -> Point<Pixels> {
        self.position
    }
}

/// The lines on screen: the projection shown, the snapshot it is drawn
/// from and the [`SelectionPolicy`] that says which lines take the
/// selection. Positions are display indices. A data row removed since the
/// projection was built (a tombstone) has no key; it and a group footer
/// take neither the focus nor the selection. A group caption takes the
/// focus, and the selection only if the policy says so.
pub(super) struct ShownRows<'a, R: GridRow> {
    projection: &'a Projection,
    snapshot: &'a StoreSnapshot<R>,
    policy: &'a dyn SelectionPolicy<R>,
}

impl<R: GridRow> Clone for ShownRows<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: GridRow> Copy for ShownRows<'_, R> {}

impl<'a, R: GridRow> ShownRows<'a, R> {
    /// The lines with the default policy, [`FocusCaptions`].
    pub(super) fn new(projection: &'a Projection, snapshot: &'a StoreSnapshot<R>) -> Self {
        Self {
            projection,
            snapshot,
            policy: &FocusCaptions,
        }
    }

    pub(super) fn with_policy(mut self, policy: &'a dyn SelectionPolicy<R>) -> Self {
        self.policy = policy;
        self
    }

    fn len(&self) -> usize {
        self.projection.len()
    }

    /// The key of the live row at a display position.
    fn key_at(&self, display_ix: usize) -> Option<&'a R::Key> {
        let id = self.projection.row(display_ix)?;
        self.snapshot.row(id).map(|row| row.key())
    }

    /// The group whose caption is at a display position.
    fn group_at(&self, display_ix: usize) -> Option<GroupInfo<'a>> {
        match self.projection.line(display_ix)? {
            DisplayRow::GroupCaption { group, .. } => self.projection.groups().get(group),
            _ => None,
        }
    }

    /// The display position of a live row on screen.
    fn display_ix(&self, key: &R::Key) -> Option<usize> {
        self.snapshot
            .row_id(key)
            .and_then(|id| self.projection.display_ix(id))
    }

    /// The display position of the caption of the group at `path`, if
    /// shown.
    pub(super) fn caption_ix(&self, path: &GroupPath) -> Option<usize> {
        let group = self.projection.groups().find(path)?;
        self.projection.group_line(group.id())
    }

    /// The display position of a focused row or caption, if shown.
    fn line_of<C: Copy>(&self, cell: &FocusedCell<R::Key, C>) -> Option<usize> {
        match (cell.row(), cell.group()) {
            (Some(key), _) => self.display_ix(key),
            (None, Some(path)) => self.caption_ix(path),
            (None, None) => None,
        }
    }

    /// Returns true if the line at a display position takes the focus: a
    /// live data row or a group caption.
    fn is_focusable(&self, display_ix: usize) -> bool {
        self.key_at(display_ix).is_some() || self.group_at(display_ix).is_some()
    }

    /// Returns true if a press or a key on the line of `cell` may select
    /// it: a data row the policy selects (with `FocusBranches`, not a tree
    /// row with children). A caption never: a
    /// selectable one joins a range through its group's rows. A row the
    /// snapshot lacks is left to the edit.
    fn takes_selection<C: Copy>(&self, cell: &FocusedCell<R::Key, C>) -> bool {
        let Some(key) = cell.row() else {
            return false;
        };
        let Some((id, row)) = self
            .snapshot
            .row_id(key)
            .and_then(|id| Some((id, self.snapshot.row(id)?)))
        else {
            return true;
        };
        self.policy.is_selectable(&SelectableLine::Row {
            row: &**row,
            branch: self.projection.branch(id),
        })
    }

    /// The focus on the line at a display position, in `column`.
    fn cell_at(
        &self,
        display_ix: usize,
        column: R::Column,
    ) -> Option<FocusedCell<R::Key, R::Column>> {
        if let Some(key) = self.key_at(display_ix) {
            return Some(FocusedCell::new(key.clone(), column));
        }
        let group = self.group_at(display_ix)?;
        Some(FocusedCell::caption(group.path().clone(), column))
    }

    /// Adds the selectable lines of `lines` to `map`, each with `item`: a
    /// live data row its key, a selectable caption its group's live rows.
    fn select_lines(
        &self,
        lines: impl Iterator<Item = usize>,
        item: ColumnSet,
        map: &mut FxHashMap<R::Key, ColumnSet>,
    ) {
        let (projection, snapshot, policy) = (self.projection, self.snapshot, self.policy);
        let groups = projection.groups();
        for display_ix in lines {
            match projection.line(display_ix) {
                Some(DisplayRow::Data { row: id, .. }) => {
                    if let Some(row) = snapshot.row(id)
                        && policy.is_selectable(&SelectableLine::Row {
                            row: &**row,
                            branch: projection.branch(id),
                        })
                    {
                        map.insert(row.key().clone(), item);
                    }
                }
                Some(DisplayRow::GroupCaption { group, .. }) => {
                    let Some(group) = groups.get(group) else {
                        continue;
                    };
                    if policy.is_selectable(&SelectableLine::Caption { group }) {
                        for row in group.rows().iter().filter_map(|id| snapshot.row(*id)) {
                            map.insert(row.key().clone(), item);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The first and the last line that take the focus.
    fn first_focusable(&self) -> Option<usize> {
        (0..self.len()).find(|ix| self.is_focusable(*ix))
    }

    fn last_focusable(&self) -> Option<usize> {
        (0..self.len()).rev().find(|ix| self.is_focusable(*ix))
    }

    /// The live row nearest to `display_ix`: it, else the next one, else the
    /// one before.
    fn nearest_live(&self, display_ix: usize) -> Option<usize> {
        let display_ix = display_ix.min(self.len().checked_sub(1)?);
        (display_ix..self.len())
            .find(|ix| self.key_at(*ix).is_some())
            .or_else(|| (0..display_ix).rev().find(|ix| self.key_at(*ix).is_some()))
    }

    /// Where a move from `origin` to `target` lands: on a focusable
    /// `target`, else on the first focusable line past it in the same
    /// direction, else on the one nearest to it on the way back; `origin`
    /// if there is none.
    fn focusable_toward(&self, origin: usize, target: usize) -> usize {
        let focusable = |ix: &usize| self.is_focusable(*ix);
        if target > origin {
            (target..self.len())
                .find(focusable)
                .or_else(|| (origin + 1..target).rev().find(focusable))
                .unwrap_or(origin)
        } else if target < origin {
            (0..=target)
                .rev()
                .find(focusable)
                .or_else(|| (target + 1..origin).find(focusable))
                .unwrap_or(origin)
        } else {
            origin
        }
    }

    /// The line [`SelectionPolicy::line_scope`] is asked about for a
    /// command over `target`, and the rows under it (a range of
    /// `Projection::rows`); `None` if the line is not in the projection.
    fn scope_line(
        &self,
        target: &LineTarget<R::Key>,
    ) -> Option<(SelectableLine<'a, R>, std::ops::Range<usize>)> {
        let (projection, snapshot) = (self.projection, self.snapshot);
        match target {
            LineTarget::Row(key) => {
                let id = snapshot.row_id(key)?;
                let row = snapshot.row(id)?;
                let branch = projection.branch(id);
                let under = match (branch, projection.order_ix(id)) {
                    (Some(branch), Some(ix)) => ix + 1..ix + 1 + branch.descendant_count(),
                    _ => 0..0,
                };
                Some((
                    SelectableLine::Row {
                        row: &**row,
                        branch,
                    },
                    under,
                ))
            }
            LineTarget::Group(path) => {
                let group = projection.groups().find(path)?;
                let start = group
                    .rows()
                    .first()
                    .and_then(|id| projection.order_ix(*id))
                    .unwrap_or(0);
                Some((
                    SelectableLine::Caption { group },
                    start..start + group.rows().len(),
                ))
            }
        }
    }
}

/// The visible columns in display order, as the selection sees them: ranges
/// of cells and navigation go by [`ViewColumnIx`], identity by
/// [`ColumnIx`], and only the column layout converts between them.
pub(super) trait ColumnView {
    fn view_column_ix(&self, column: ColumnIx) -> Option<ViewColumnIx>;
    fn column_at(&self, view: ViewColumnIx) -> Option<ColumnIx>;
    fn visible_count(&self) -> usize;
    fn visible_set(&self) -> ColumnSet;
    /// The visible column next to a hidden `column` in display order: the
    /// first one right of it, else the first one left of it.
    fn visible_neighbour(&self, column: ColumnIx) -> Option<ColumnIx>;
}

impl<C: GridColumns> ColumnView for ColumnLayout<C> {
    fn view_column_ix(&self, column: ColumnIx) -> Option<ViewColumnIx> {
        ColumnLayout::view_column_ix(self, column)
    }

    fn column_at(&self, view: ViewColumnIx) -> Option<ColumnIx> {
        ColumnLayout::column_at(self, view)
    }

    fn visible_count(&self) -> usize {
        ColumnLayout::visible_set(self).len()
    }

    fn visible_set(&self) -> ColumnSet {
        ColumnLayout::visible_set(self)
    }

    fn visible_neighbour(&self, column: ColumnIx) -> Option<ColumnIx> {
        let order = self.order();
        let position = order.iter().position(|ix| *ix == column)?;
        order[position + 1..]
            .iter()
            .chain(order[..position].iter().rev())
            .copied()
            .find(|ix| self.is_visible(*ix))
    }
}

fn first_column(columns: &impl ColumnView) -> Option<ColumnIx> {
    columns.column_at(ViewColumnIx::new(0))
}

fn last_column(columns: &impl ColumnView) -> Option<ColumnIx> {
    let last = columns.visible_count().checked_sub(1)?;
    columns.column_at(ViewColumnIx::new(last as u16))
}

/// The visible column `step` places from `column`, stopping at the edges.
fn step_column(column: ColumnIx, step: isize, columns: &impl ColumnView) -> ColumnIx {
    let Some(view) = columns.view_column_ix(column) else {
        return column;
    };
    let last = columns.visible_count().saturating_sub(1);
    let target = view.get().saturating_add_signed(step).min(last);
    columns
        .column_at(ViewColumnIx::new(target as u16))
        .unwrap_or(column)
}

/// `columns` without the columns of `drop`.
fn difference(columns: ColumnSet, drop: ColumnSet) -> ColumnSet {
    (0..128u16)
        .map(ColumnIx::new)
        .filter(|ix| columns.contains(*ix) && !drop.contains(*ix))
        .fold(ColumnSet::EMPTY, |mut set, ix| {
            set.insert(ix);
            set
        })
}

/// The live row next to `id` among the data rows `previous` shows that
/// `accept` takes: the first one after it, else the nearest one before it
/// (the successor rule). Without `id` among them, the first one `accept`
/// takes at all.
pub(super) fn successor<T>(
    previous: &Projection,
    id: Option<RowId>,
    accept: impl Fn(RowId) -> Option<T>,
) -> Option<T> {
    let accept_at = |display_ix: usize| previous.row(display_ix).and_then(&accept);
    match id.and_then(|id| previous.display_ix(id)) {
        Some(ix) => (ix + 1..previous.len())
            .find_map(accept_at)
            .or_else(|| (0..ix).rev().find_map(accept_at)),
        None => (0..previous.len()).find_map(accept_at),
    }
}

/// A keyboard move of the focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Navigation {
    Up,
    Down,
    PrevColumn,
    NextColumn,
    PageUp,
    PageDown,
    /// The first visible column.
    First,
    /// The last visible column.
    Last,
    FirstRow,
    LastRow,
    FirstCell,
    LastCell,
}

/// The selection's revision and the focus before an action: the action's
/// events compare against it. It holds no clone of the selection, so an
/// edit in place during the action does not copy the set.
pub(super) struct ChangeMark<K, C> {
    revision: u64,
    focus: Option<FocusedCell<K, C>>,
}

/// A drag in progress: the range from the anchor to `target` is painted and
/// becomes the selection on release.
struct Drag<K, C> {
    target: FocusedCell<K, C>,
    /// The selection before the gesture, when the drag adds to it (after a
    /// Toggle or an AddRange press).
    base: Option<GridSelection<K>>,
}

/// What a frame paints as selected: the selection, or during a drag the
/// range under the pointer, alone or over the selection before the gesture.
/// Built once per frame; one hash lookup per painted row.
pub(super) struct SelectionPainter<'a, K> {
    selection: Option<&'a GridSelection<K>>,
    range: Option<(RangeInclusive<usize>, ColumnSet)>,
}

impl<K: Eq + Hash> SelectionPainter<'_, K> {
    /// The painted columns of the row with `key` at `display_ix`.
    pub(super) fn columns(&self, key: &K, display_ix: usize) -> ColumnSet {
        let mut columns = self
            .selection
            .map_or(ColumnSet::EMPTY, |selection| selection.columns(key));
        if let Some((rows, range)) = &self.range
            && rows.contains(&display_ix)
        {
            columns.union_with(*range);
        }
        columns
    }
}

/// The selection, the focused cell and the anchor, by key; no GPUI types.
pub(super) struct SelectionModel<K, C> {
    mode: SelectionMode,
    /// Every schema column: the columns of a selected row in the row modes.
    all: ColumnSet,
    selection: GridSelection<K>,
    /// Counts the changes of `selection`: every edit that changes it bumps
    /// it, one that leaves it as it was does not.
    revision: u64,
    focus: Option<FocusedCell<K, C>>,
    /// Where a range starts (Shift); set by `Replace` and `Toggle`.
    anchor: Option<FocusedCell<K, C>>,
    drag: Option<Drag<K, C>>,
}

impl<K: Clone + Eq + Hash, C: GridColumns> SelectionModel<K, C> {
    pub(super) fn new(mode: SelectionMode) -> Self {
        Self {
            mode,
            all: ColumnSet::first(C::all().len()),
            selection: GridSelection::default(),
            revision: 0,
            focus: None,
            anchor: None,
            drag: None,
        }
    }

    /// Replaces the selection, counting a change only if it differs.
    fn set_selection(&mut self, selection: GridSelection<K>) {
        if !self.selection.same_as(&selection) {
            self.selection = selection;
            self.revision += 1;
        }
    }

    pub(super) fn mode(&self) -> SelectionMode {
        self.mode
    }

    pub(super) fn selection(&self) -> &GridSelection<K> {
        &self.selection
    }

    pub(super) fn focus(&self) -> Option<&FocusedCell<K, C>> {
        self.focus.as_ref()
    }

    pub(super) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub(super) fn mark(&self) -> ChangeMark<K, C> {
        ChangeMark {
            revision: self.revision,
            focus: self.focus.clone(),
        }
    }

    /// Whether the selection and the focus changed since `mark`.
    pub(super) fn changes_since(&self, mark: &ChangeMark<K, C>) -> (bool, bool) {
        (self.revision != mark.revision, self.focus != mark.focus)
    }

    /// The columns one selected item at `column` covers: the whole row in
    /// the row modes, the cell in the cell modes.
    fn item(&self, column: ColumnIx) -> ColumnSet {
        if self.mode.selects_cells() {
            let mut cell = ColumnSet::EMPTY;
            cell.insert(column);
            cell
        } else {
            self.all
        }
    }

    /// The columns a programmatically selected row gets: the whole row, its
    /// visible cells in `CellSet`, the focused column's cell in `CellSingle`.
    fn row_item(&self, columns: &impl ColumnView) -> ColumnSet {
        match self.mode {
            SelectionMode::RowSingle | SelectionMode::RowSet => self.all,
            SelectionMode::CellSet => columns.visible_set(),
            SelectionMode::CellSingle => self
                .focus_column(columns)
                .map_or(ColumnSet::EMPTY, |column| self.item(column)),
        }
    }

    /// The focused column if visible, else the first visible one.
    fn focus_column(&self, columns: &impl ColumnView) -> Option<ColumnIx> {
        self.focus
            .as_ref()
            .map(|focus| focus.column().ix())
            .filter(|column| columns.view_column_ix(*column).is_some())
            .or_else(|| first_column(columns))
    }

    fn only(&self, target: &FocusedCell<K, C>) -> GridSelection<K> {
        target.row().map_or_else(GridSelection::default, |key| {
            GridSelection::from_rows([(key.clone(), self.item(target.column().ix()))])
        })
    }

    /// Returns true if the target row (in the cell modes, cell) is selected.
    pub(super) fn is_selected(&self, target: &FocusedCell<K, C>) -> bool {
        target
            .row()
            .is_some_and(|key| self.selection.contains_cell(key, target.column().ix()))
    }

    /// Returns true if a `Replace` on the selected `target` would drop more
    /// than the target: a press there collapses the selection on release.
    pub(super) fn holds_more_than(&self, target: &FocusedCell<K, C>) -> bool {
        target.row().is_some_and(|key| {
            self.selection.len() > 1
                || self.selection.columns(key) != self.item(target.column().ix())
        })
    }

    /// Applies `edit` with `target`, a line on screen; the target always
    /// gets the focus. In the single modes `Extend` and `AddRange` replace.
    /// A group caption takes no selection, nor a line the selection policy
    /// does not select (a tree row with children under `FocusBranches`): on
    /// one, `Replace` and `Toggle` only move the focus, and a range from the
    /// anchor stops at it, the anchor staying where it is.
    pub(super) fn apply<R>(
        &mut self,
        edit: SelectionEdit,
        target: FocusedCell<K, C>,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) where
        R: GridRow<Key = K, Column = C>,
    {
        let selectable = rows.takes_selection(&target);
        let edit = match edit {
            SelectionEdit::Extend | SelectionEdit::AddRange if self.mode.is_single() => {
                SelectionEdit::Replace
            }
            edit => edit,
        };
        let edit = match edit {
            SelectionEdit::Replace | SelectionEdit::Toggle if !selectable => SelectionEdit::Keep,
            edit => edit,
        };
        match edit {
            SelectionEdit::Replace => self.replace(&target),
            SelectionEdit::Extend | SelectionEdit::AddRange => {
                let range = self
                    .anchor
                    .as_ref()
                    .and_then(|anchor| self.range(anchor, &target, rows, columns));
                match range {
                    Some(range) if edit == SelectionEdit::AddRange => {
                        if self.selection.union_with(&range) {
                            self.revision += 1;
                        }
                    }
                    Some(range) => self.set_selection(range),
                    // No anchor on screen: start over from the target; a
                    // caption or a line the policy does not select only
                    // takes the focus.
                    None if !selectable => {}
                    None => self.replace(&target),
                }
            }
            SelectionEdit::Toggle => {
                self.toggle(&target);
                self.anchor = Some(target.clone());
            }
            SelectionEdit::Keep => {}
        }
        self.focus = Some(target);
    }

    fn replace(&mut self, target: &FocusedCell<K, C>) {
        self.set_selection(self.only(target));
        self.anchor = Some(target.clone());
    }

    fn toggle(&mut self, target: &FocusedCell<K, C>) {
        let Some(key) = target.row() else {
            return;
        };
        let column = target.column().ix();
        if self.mode.is_single() {
            // Select the target, or clear the one selected item.
            let toggled = if self.selection.contains_cell(key, column) {
                GridSelection::default()
            } else {
                self.only(target)
            };
            self.set_selection(toggled);
            return;
        }
        let item = self.item(column);
        self.selection.flip(key, column, item);
        self.revision += 1;
    }

    /// The selection with `target` flipped back: what it was before a
    /// `Toggle` of `target`.
    fn before_toggle(&self, target: &FocusedCell<K, C>) -> GridSelection<K> {
        let mut selection = self.selection.clone();
        if let Some(key) = target.row() {
            let column = target.column().ix();
            selection.flip(key, column, self.item(column));
        }
        selection
    }

    /// The selectable lines from `anchor` to `target` in the projection on
    /// screen, tombstones skipped, captions by the selection policy; in the
    /// cell modes, with the visible columns from the anchor's to the
    /// target's. `None` if either is not on screen.
    fn range<R>(
        &self,
        anchor: &FocusedCell<K, C>,
        target: &FocusedCell<K, C>,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) -> Option<GridSelection<K>>
    where
        R: GridRow<Key = K, Column = C>,
    {
        let from = rows.line_of(anchor)?;
        let to = rows.line_of(target)?;
        let item = self.range_columns(anchor.column().ix(), target.column().ix(), columns);
        let (start, end) = (from.min(to), from.max(to));
        let mut map = FxHashMap::with_capacity_and_hasher(end - start + 1, Default::default());
        rows.select_lines(start..=end, item, &mut map);
        Some(GridSelection::from_map(map))
    }

    /// The columns of a range: every column in the row modes, the visible
    /// ones from `from` to `to` by [`ViewColumnIx`] in the cell modes.
    fn range_columns(&self, from: ColumnIx, to: ColumnIx, columns: &impl ColumnView) -> ColumnSet {
        if !self.mode.selects_cells() {
            return self.all;
        }
        let (Some(from), Some(to)) = (columns.view_column_ix(from), columns.view_column_ix(to))
        else {
            // A hidden anchor column: the target's column alone.
            return self.item(to);
        };
        let (start, end) = (from.get().min(to.get()), from.get().max(to.get()));
        (start..=end)
            .filter_map(|view| columns.column_at(ViewColumnIx::new(view as u16)))
            .fold(ColumnSet::EMPTY, |mut set, column| {
                set.insert(column);
                set
            })
    }

    /// Where a keyboard move puts the focus, in model coordinates (display
    /// index, [`ViewColumnIx`]): rows off screen are reached the same way.
    /// No wrap at the edges. Without a focus on screen, the first move
    /// focuses the first cell, the `…Last…` moves the last row or column.
    pub(super) fn navigation_target<R>(
        &self,
        navigation: Navigation,
        page: usize,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) -> Option<FocusedCell<K, C>>
    where
        R: GridRow<Key = K, Column = C>,
    {
        let first = first_column(columns)?;
        let last = last_column(columns)?;
        let focus = self.focus.as_ref().and_then(|focus| {
            let display_ix = rows.line_of(focus)?;
            let column = Some(focus.column().ix())
                .filter(|column| columns.view_column_ix(*column).is_some())
                .unwrap_or(first);
            Some((display_ix, column))
        });
        let (display_ix, column) = match focus {
            None => {
                let top = rows.first_focusable()?;
                let bottom = rows.last_focusable().unwrap_or(top);
                match navigation {
                    Navigation::Last => (top, last),
                    Navigation::LastRow => (bottom, first),
                    Navigation::LastCell => (bottom, last),
                    _ => (top, first),
                }
            }
            Some((origin, column)) => {
                let end = rows.len().saturating_sub(1);
                let page = page.max(1);
                let (target, column) = match navigation {
                    Navigation::Up => (origin.saturating_sub(1), column),
                    Navigation::Down => ((origin + 1).min(end), column),
                    Navigation::PageUp => (origin.saturating_sub(page), column),
                    Navigation::PageDown => ((origin + page).min(end), column),
                    Navigation::PrevColumn => (origin, step_column(column, -1, columns)),
                    Navigation::NextColumn => (origin, step_column(column, 1, columns)),
                    Navigation::First => (origin, first),
                    Navigation::Last => (origin, last),
                    Navigation::FirstRow => (0, column),
                    Navigation::LastRow => (end, column),
                    Navigation::FirstCell => (0, first),
                    Navigation::LastCell => (end, last),
                };
                (rows.focusable_toward(origin, target), column)
            }
        };
        rows.cell_at(display_ix, column_of(column))
    }

    /// Moves the focus to `target` alone; the selection and the anchor stay.
    pub(super) fn focus_on(&mut self, target: FocusedCell<K, C>) {
        self.focus = Some(target);
    }

    /// Focuses the caption of the group at `path` if the projection on
    /// screen shows it; the focused column stays, or is the first visible
    /// one. The selection and the anchor stay. Returns false if the caption
    /// is not shown.
    pub(super) fn focus_caption<R>(
        &mut self,
        path: &GroupPath,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) -> bool
    where
        R: GridRow<Key = K, Column = C>,
    {
        let Some(column) = self.focus_column(columns) else {
            return false;
        };
        if rows.caption_ix(path).is_none() {
            return false;
        }
        self.focus = Some(FocusedCell::caption(path.clone(), column_of(column)));
        true
    }

    /// Space: toggles the focused row or cell in the set modes, selects it
    /// in the single modes. Returns false without a focus on screen.
    pub(super) fn toggle_focused<R>(
        &mut self,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) -> bool
    where
        R: GridRow<Key = K, Column = C>,
    {
        let Some(focus) = self.focus.clone() else {
            return false;
        };
        // A caption takes no selection, nor a line the policy does not
        // select: Space goes on.
        if focus.row().and_then(|key| rows.display_ix(key)).is_none()
            || !rows.takes_selection(&focus)
        {
            return false;
        }
        let edit = if self.mode.is_single() {
            SelectionEdit::Replace
        } else {
            SelectionEdit::Toggle
        };
        self.apply(edit, focus, rows, columns);
        true
    }

    /// Cmd/Ctrl+A: selects the keys of the rows on screen now, tombstones
    /// skipped; in `CellSet` their visible cells. Rows of collapsed groups
    /// are not on screen, and captions take the selection only as the
    /// policy says. Rows that arrive later are never selected by it. The
    /// focus and the anchor stay. Returns false in the single modes, which
    /// have no "select all".
    pub(super) fn select_all<R>(
        &mut self,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) -> bool
    where
        R: GridRow<Key = K, Column = C>,
    {
        if self.mode.is_single() {
            return false;
        }
        let item = if self.mode.selects_cells() {
            columns.visible_set()
        } else {
            self.all
        };
        let mut map = FxHashMap::with_capacity_and_hasher(rows.len(), Default::default());
        rows.select_lines(0..rows.len(), item, &mut map);
        self.set_selection(GridSelection::from_map(map));
        true
    }

    pub(super) fn clear(&mut self) {
        if !self.selection.is_empty() {
            self.set_selection(GridSelection::default());
        }
    }

    /// Rows ↔ cells clears the selection; a set mode → a single one keeps
    /// only the focused row or cell, if it was selected.
    pub(super) fn set_mode(&mut self, mode: SelectionMode) {
        if mode == self.mode {
            return;
        }
        let unit_changed = mode.selects_cells() != self.mode.selects_cells();
        self.mode = mode;
        self.drag = None;
        if unit_changed {
            self.clear();
        } else if mode.is_single() {
            let kept = self
                .focus
                .as_ref()
                .filter(|focus| self.is_selected(focus))
                .map(|focus| self.only(focus));
            self.set_selection(kept.unwrap_or_default());
        }
    }

    /// Selects the rows of `keys` that the snapshot has (the first one in
    /// the single modes). The focus and the anchor stay.
    pub(super) fn set_rows<R>(
        &mut self,
        keys: impl IntoIterator<Item = K>,
        snapshot: &StoreSnapshot<R>,
        columns: &impl ColumnView,
    ) where
        R: GridRow<Key = K, Column = C>,
    {
        let item = self.row_item(columns);
        let limit = if self.mode.is_single() { 1 } else { usize::MAX };
        self.set_selection(GridSelection::from_rows(
            keys.into_iter()
                .filter(|key| snapshot.row_id(key).is_some())
                .take(limit)
                .map(|key| (key, item)),
        ));
    }

    /// Selects these cells of rows the snapshot has (the first one in the
    /// single modes); in the row modes, their rows.
    pub(super) fn set_cells<R>(
        &mut self,
        cells: impl IntoIterator<Item = (K, C)>,
        snapshot: &StoreSnapshot<R>,
    ) where
        R: GridRow<Key = K, Column = C>,
    {
        let mut map: FxHashMap<K, ColumnSet> = FxHashMap::default();
        for (key, column) in cells {
            if snapshot.row_id(&key).is_none() {
                continue;
            }
            if self.mode.is_single() && !map.is_empty() {
                break;
            }
            map.entry(key)
                .or_default()
                .union_with(self.item(column.ix()));
        }
        self.set_selection(GridSelection::from_map(map));
    }

    /// Selects one row (`None` clears): the row in the row modes, its cell
    /// in the focused column in the cell modes. A row on screen also takes
    /// the focus and the anchor; a key the snapshot lacks selects nothing.
    pub(super) fn select_row<R>(
        &mut self,
        key: Option<K>,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) where
        R: GridRow<Key = K, Column = C>,
    {
        let key = key.filter(|key| rows.snapshot.row_id(key).is_some());
        let (Some(key), Some(column)) = (key, self.focus_column(columns)) else {
            self.clear();
            return;
        };
        let target = FocusedCell::new(key, column_of(column));
        self.set_selection(self.only(&target));
        if target.row().and_then(|key| rows.display_ix(key)).is_some() {
            self.focus = Some(target.clone());
            self.anchor = Some(target);
        }
    }

    /// Focuses a cell of a row on screen; `None`, or a hidden column, keeps
    /// the focused column. The anchor follows. Returns false if the row is
    /// not on screen.
    pub(super) fn set_focus<R>(
        &mut self,
        key: &K,
        column: Option<C>,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) -> bool
    where
        R: GridRow<Key = K, Column = C>,
    {
        if rows.display_ix(key).is_none() {
            return false;
        }
        let column = column
            .map(|column| column.ix())
            .filter(|column| columns.view_column_ix(*column).is_some())
            .or_else(|| self.focus_column(columns));
        let Some(column) = column else {
            return false;
        };
        let target = FocusedCell::new(key.clone(), column_of(column));
        self.focus = Some(target.clone());
        self.anchor = Some(target);
        true
    }

    /// Drops the keys of rows removed from the store, read from the
    /// `previous` snapshot: a key removed and inserted again in one batch is
    /// a new row and is not selected.
    pub(super) fn remove_rows<R>(&mut self, removed: &[RowId], previous: &StoreSnapshot<R>)
    where
        R: GridRow<Key = K>,
    {
        let remove = |selection: &mut GridSelection<K>| {
            let mut changed = false;
            for id in removed {
                if let Some(row) = previous.row(*id)
                    && selection.contains(row.key())
                {
                    selection.rows_mut().remove(row.key());
                    changed = true;
                }
            }
            changed
        };
        if !self.selection.is_empty() && remove(&mut self.selection) {
            self.revision += 1;
        }
        if let Some(base) = self.drag.as_mut().and_then(|drag| drag.base.as_mut()) {
            remove(base);
        }
    }

    /// A new data set: keys the snapshot has stay selected, the rest go;
    /// the focus and the anchor stay if their keys are there, else go.
    pub(super) fn retain_present<R>(&mut self, snapshot: &StoreSnapshot<R>)
    where
        R: GridRow<Key = K>,
    {
        let missing = |key: &K| snapshot.row_id(key).is_none();
        if self.selection.retain(missing) {
            self.revision += 1;
        }
        if let Some(base) = self.drag.as_mut().and_then(|drag| drag.base.as_mut()) {
            base.retain(missing);
        }
        for cell in [&mut self.focus, &mut self.anchor] {
            if cell
                .as_ref()
                .and_then(|cell| cell.row())
                .is_some_and(missing)
            {
                *cell = None;
            }
        }
    }

    /// The successor rule for removed rows: a focus or an anchor on a row
    /// removed from the store moves to the next live row of the projection
    /// on screen, else to the previous one, else to none. The column stays.
    pub(super) fn follow_removed<R>(
        &mut self,
        projection: &Projection,
        previous: &StoreSnapshot<R>,
        current: &StoreSnapshot<R>,
    ) where
        R: GridRow<Key = K>,
    {
        for cell in [&mut self.focus, &mut self.anchor] {
            let Some(focused) = cell.as_ref() else {
                continue;
            };
            let Some(id) = focused.row().and_then(|key| previous.row_id(key)) else {
                continue;
            };
            if current.row(id).is_some() {
                continue;
            }
            let column = focused.column();
            *cell = successor(projection, Some(id), |id| {
                current.row(id).map(|row| row.key().clone())
            })
            .map(|key| FocusedCell::new(key, column));
        }
    }

    /// The successor rule for a new projection: a focus or an anchor on a
    /// row the new projection does not show moves along the `previous`
    /// order to the nearest row it shows.
    pub(super) fn follow_filtered<R>(
        &mut self,
        previous: &Projection,
        shown: &Projection,
        snapshot: &StoreSnapshot<R>,
    ) where
        R: GridRow<Key = K>,
    {
        for cell in [&mut self.focus, &mut self.anchor] {
            // A focused caption follows its group (`follow_shown_lines`).
            let Some(focused) = cell.as_ref().filter(|cell| cell.group().is_none()) else {
                continue;
            };
            let id = focused.row().and_then(|key| snapshot.row_id(key));
            if id.is_some_and(|id| shown.contains(id)) {
                continue;
            }
            let column = focused.column();
            *cell = successor(previous, id, |id| {
                shown
                    .contains(id)
                    .then(|| snapshot.row(id))
                    .flatten()
                    .map(|row| row.key().clone())
            })
            .map(|key| FocusedCell::new(key, column));
        }
    }

    /// Moves the focus and the anchor off a hidden column, to the visible
    /// column right of it, else left of it.
    pub(super) fn move_off_hidden(&mut self, columns: &impl ColumnView) {
        for cell in [&mut self.focus, &mut self.anchor].into_iter().flatten() {
            let column = cell.column().ix();
            if columns.view_column_ix(column).is_none()
                && let Some(neighbour) = columns.visible_neighbour(column)
            {
                cell.column = column_of(neighbour);
            }
        }
    }

    /// Starts a drag-selection at `target`. A drag after `Replace` or
    /// `Extend` extends from the anchor; one after `Toggle` or `AddRange`
    /// (`adds`) adds the range to `base`, the selection before the gesture.
    pub(super) fn begin_drag(
        &mut self,
        adds: bool,
        base: GridSelection<K>,
        target: FocusedCell<K, C>,
    ) {
        self.drag = Some(Drag {
            target,
            base: adds.then_some(base),
        });
    }

    /// Moves the drag's end; returns true if it moved.
    pub(super) fn drag_to(&mut self, target: FocusedCell<K, C>) -> bool {
        match &mut self.drag {
            Some(drag) if drag.target != target => {
                drag.target = target;
                true
            }
            _ => false,
        }
    }

    pub(super) fn drag_target(&self) -> Option<&FocusedCell<K, C>> {
        self.drag.as_ref().map(|drag| &drag.target)
    }

    /// Ends a drag: its range, by the projection on screen now, becomes the
    /// selection (over the base when it adds), and its end the focus.
    pub(super) fn finish_drag<R>(&mut self, rows: ShownRows<'_, R>, columns: &impl ColumnView)
    where
        R: GridRow<Key = K, Column = C>,
    {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if drag
            .target
            .row()
            .and_then(|key| rows.display_ix(key))
            .is_none()
        {
            // The row under the pointer went away: keep what the press did.
            return;
        }
        let edit = match drag.base {
            Some(base) => {
                self.set_selection(base);
                SelectionEdit::AddRange
            }
            None => SelectionEdit::Extend,
        };
        self.apply(edit, drag.target, rows, columns);
    }

    /// What this frame paints as selected.
    pub(super) fn painter<'a, R>(
        &'a self,
        rows: ShownRows<'_, R>,
        columns: &impl ColumnView,
    ) -> SelectionPainter<'a, K>
    where
        R: GridRow<Key = K, Column = C>,
    {
        let Some(drag) = &self.drag else {
            return SelectionPainter {
                selection: Some(&self.selection),
                range: None,
            };
        };
        let target = drag.target.row().and_then(|key| rows.display_ix(key));
        let anchor = self
            .anchor
            .as_ref()
            .filter(|_| !self.mode.is_single())
            .and_then(|anchor| Some((rows.display_ix(anchor.row()?)?, anchor.column().ix())));
        let range = target.map(|to| {
            let to_column = drag.target.column().ix();
            let (from, from_column) = anchor.unwrap_or((to, to_column));
            (
                from.min(to)..=from.max(to),
                self.range_columns(from_column, to_column, columns),
            )
        });
        SelectionPainter {
            selection: drag.base.as_ref().filter(|_| !self.mode.is_single()),
            range,
        }
    }
}

/// A primary-button press on the body until its release.
pub(super) struct Press<K, C> {
    /// The selection and focus before the gesture: its events come once,
    /// on release.
    mark: ChangeMark<K, C>,
    /// The selection before an `AddRange` press, for a drag that follows.
    /// (Before a `Toggle` it is rebuilt when the drag starts, so a toggle
    /// does not copy a large selection.)
    base: Option<GridSelection<K>>,
    target: FocusedCell<K, C>,
    origin: Point<Pixels>,
    /// A `Replace` on a selected row waits for the release (deferred
    /// collapse), so a drag of the selection keeps it.
    deferred: bool,
    /// The press toggled or added a range: a drag adds too.
    adds: bool,
    /// The pointer went past the drag threshold.
    moved: bool,
    /// The press only moves the focus: on a caption.
    focus_only: bool,
    /// The press may drag rows, with its modifiers for `drag_payload`: it
    /// starts no drag-selection.
    drag_candidate: Option<Modifiers>,
    /// The pointer passed the drag gesture's threshold: the drag armed, or
    /// the delegate refused it.
    drag_tried: bool,
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets what a click or a key press selects (default
    /// [`SelectionMode::RowSingle`]).
    pub fn with_selection_mode(mut self, mode: SelectionMode) -> Self {
        self.selection.set_mode(mode);
        self
    }

    /// Sets which rows and cells commands act on (default
    /// [`ProjectionScope`](crate::ProjectionScope); see also
    /// [`StoreScope`](crate::StoreScope)).
    pub fn with_selection_scope(mut self, scope: impl SelectionScope<D::Row>) -> Self {
        self.selection_scope = Box::new(scope);
        self
    }

    /// Sets how a press or a navigation command edits the selection
    /// (default [`StandardSelectionGesture`](crate::StandardSelectionGesture)).
    pub fn with_selection_gesture(mut self, gesture: impl SelectionGesture) -> Self {
        self.selection_gesture = Box::new(gesture);
        self
    }

    /// Sets which press opens the context menu (default
    /// [`StandardContextMenuGesture`](crate::StandardContextMenuGesture)). On
    /// the body that press edits the selection as
    /// [`GestureInput::ContextPointer`](crate::GestureInput::ContextPointer).
    pub fn with_context_menu_gesture(mut self, gesture: impl ContextMenuGesture) -> Self {
        self.context_menu_gesture = Box::new(gesture);
        self
    }

    /// Sets which lines take the selection and what a command over a line
    /// acts on (default [`FocusCaptions`](crate::FocusCaptions): group
    /// captions take the focus, not the selection, and a command over one
    /// acts on its group's rows).
    pub fn with_selection_policy(mut self, policy: impl SelectionPolicy<D::Row>) -> Self {
        self.selection_policy = Box::new(policy);
        self
    }

    /// The lines on screen, with the selection policy.
    fn shown_rows<'a>(
        projection: &'a Projection,
        snapshot: &'a StoreSnapshot<D::Row>,
        policy: &'a dyn SelectionPolicy<D::Row>,
    ) -> ShownRows<'a, D::Row> {
        ShownRows::new(projection, snapshot).with_policy(policy)
    }

    /// Whether a press opens the context menu: the [`ContextMenuGesture`]
    /// decides, on the body and on the header.
    pub(super) fn is_context_press(
        &self,
        button: MouseButton,
        modifiers: Modifiers,
        click_count: usize,
        area: GridArea,
    ) -> bool {
        self.context_menu_gesture
            .opens_menu(&ContextMenuGestureContext::new(
                button,
                modifiers,
                click_count,
                area,
            ))
    }

    /// What a click or a key press selects.
    pub fn selection_mode(&self) -> SelectionMode {
        self.selection.mode()
    }

    /// The whole selection, by key. It may hold rows hidden by a filter:
    /// commands use [`Self::scoped_selection`].
    pub fn selection(&self) -> &GridSelection<KeyOf<D>> {
        self.selection.selection()
    }

    /// The selection within the selection scope, in display order: what a
    /// command acts on. O(selected rows).
    pub fn scoped_selection(&self) -> ScopedSelection<KeyOf<D>> {
        self.selection_scope.resolve(&SelectionScopeContext::new(
            self.selection.selection(),
            &self.projection,
            &self.snapshot,
            self.selection.mode(),
        ))
    }

    /// The focused cell: the keyboard target.
    pub fn focused_cell(&self) -> Option<&FocusedCell<KeyOf<D>, ColumnOf<D>>> {
        self.selection.focus()
    }

    /// Returns true if the cell is selected; in the row modes, if its row
    /// is.
    pub fn is_cell_selected(&self, key: &KeyOf<D>, column: ColumnOf<D>) -> bool {
        self.selection.selection().contains_cell(key, column.ix())
    }

    /// Returns true if the last frame showed keyboard focus: the grid was
    /// focused and the last input came from the keyboard.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_focus_visible(&self) -> bool {
        self.focus_visible
    }

    /// The keys of the data rows painted in the last frame, top to bottom;
    /// group captions are left out.
    pub fn visible_keys(&self) -> Vec<KeyOf<D>> {
        self.painted
            .iter()
            .filter_map(|line| self.snapshot.row(line.row()?))
            .map(|row| row.key().clone())
            .collect()
    }

    /// Changes the selection mode (`SelectionOrigin::Programmatic`). Rows ↔
    /// cells clears the selection; a set mode → a single one keeps only the
    /// focused row or cell.
    pub fn set_selection_mode(&mut self, mode: SelectionMode, cx: &mut Context<Self>) {
        self.edit_selection(SelectionOrigin::Programmatic, cx, |model, _, _| {
            model.set_mode(mode)
        });
    }

    /// Selects one row, or clears the selection with `None`; in the cell
    /// modes, its cell in the focused column. A row on screen also takes the
    /// focus. A key missing from the current snapshot selects nothing
    /// (`SelectionOrigin::Programmatic`).
    pub fn select_key(&mut self, key: Option<KeyOf<D>>, cx: &mut Context<Self>) {
        self.edit_selection(SelectionOrigin::Programmatic, cx, |model, rows, columns| {
            model.select_row(key, rows, columns)
        });
    }

    /// Selects these rows (the first one in the single modes; their visible
    /// cells in `CellSet`). Keys missing from the current snapshot are
    /// dropped: a row that does not exist yet is not selected in advance.
    pub fn set_selected_keys(
        &mut self,
        keys: impl IntoIterator<Item = KeyOf<D>>,
        cx: &mut Context<Self>,
    ) {
        self.edit_selection(SelectionOrigin::Programmatic, cx, |model, rows, columns| {
            model.set_rows(keys, rows.snapshot, columns)
        });
    }

    /// Selects these cells (the first one in the single modes; their rows
    /// in the row modes). Keys missing from the current snapshot are
    /// dropped.
    pub fn set_selected_cells(
        &mut self,
        cells: impl IntoIterator<Item = (KeyOf<D>, ColumnOf<D>)>,
        cx: &mut Context<Self>,
    ) {
        self.edit_selection(SelectionOrigin::Programmatic, cx, |model, rows, _| {
            model.set_cells(cells, rows.snapshot)
        });
    }

    /// Selects every row on screen, as Cmd/Ctrl+A: the keys now, so rows
    /// that arrive later are not selected. Does nothing in the single modes.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.edit_selection(SelectionOrigin::Programmatic, cx, |model, rows, columns| {
            model.select_all(rows, columns)
        });
    }

    /// Clears the selection; the focus stays.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.edit_selection(SelectionOrigin::Programmatic, cx, |model, _, _| {
            model.clear()
        });
    }

    /// Focuses a cell and scrolls it into view (`SelectionOrigin::
    /// Programmatic`); the anchor follows. `None` keeps the focused column.
    /// Returns false if the row is not on screen.
    pub fn set_focused_cell(
        &mut self,
        key: &KeyOf<D>,
        column: Option<ColumnOf<D>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let focused =
            self.edit_selection(SelectionOrigin::Programmatic, cx, |model, rows, columns| {
                model.set_focus(key, column, rows, columns)
            });
        if focused {
            self.reveal_focus();
            cx.notify();
        }
        focused
    }

    /// Focuses the caption of the group at `path` if the shown order paints
    /// it, and scrolls it into view (`SelectionOrigin::Programmatic`); the
    /// focused column stays. The selection and the anchor stay: a caption
    /// takes no selection. Returns false if the projection on screen shows
    /// no such caption, under a collapsed group for one.
    pub fn set_focused_group(&mut self, path: &GroupPath, cx: &mut Context<Self>) -> bool {
        let focused =
            self.edit_selection(SelectionOrigin::Programmatic, cx, |model, rows, columns| {
                model.focus_caption(path, rows, columns)
            });
        if focused {
            self.reveal_focus();
            cx.notify();
        }
        focused
    }

    /// What the last mouse-down on the body hit.
    pub(crate) fn pointer_target(&self) -> Option<&PointerTarget<KeyOf<D>>> {
        self.pointer_target.as_ref()
    }

    /// What a command over `target` acts on: the [`SelectionPolicy`]
    /// decides for the line, in the projection on screen; without a target,
    /// or when the policy leaves it, [`Self::scoped_selection`]. A caption
    /// whose group is gone acts on nothing. O(rows under the line) for a
    /// caption, as Cmd/Ctrl+A.
    pub(crate) fn command_scope(
        &self,
        target: Option<&LineTarget<KeyOf<D>>>,
    ) -> ScopedSelection<KeyOf<D>> {
        target
            .and_then(|target| self.line_scope(target))
            .unwrap_or_else(|| self.scoped_selection())
    }

    /// The scope the [`SelectionPolicy`] gives a command over `target` of
    /// its own; `None` leaves the command to [`Self::scoped_selection`], as
    /// for a row the projection on screen does not hold. A caption whose
    /// group is gone has an empty scope.
    fn line_scope(&self, target: &LineTarget<KeyOf<D>>) -> Option<ScopedSelection<KeyOf<D>>> {
        let rows = Self::shown_rows(&self.projection, &self.snapshot, &*self.selection_policy);
        let Some((line, under)) = rows.scope_line(target) else {
            return match target {
                LineTarget::Group(_) => Some(ScopedSelection::default()),
                LineTarget::Row(_) => None,
            };
        };
        let context = LineScopeContext::new(
            line,
            under,
            &self.projection,
            &self.snapshot,
            self.selection.mode(),
        );
        self.selection_policy.line_scope(&context)
    }

    /// The focused line, as a command's target.
    pub(crate) fn focus_target(&self) -> Option<LineTarget<KeyOf<D>>> {
        self.selection.focus().and_then(FocusedCell::line)
    }

    /// Emits `Confirm` for the command scope of the focused line (the
    /// rows of a focused caption's group, else the scoped selection);
    /// returns false, emitting nothing, if it is empty. Enter,
    /// Cmd/Ctrl+Enter, and the primary menu command, which the menu's press
    /// focused first.
    pub(crate) fn confirm(&mut self, secondary: bool, cx: &mut Context<Self>) -> bool {
        let scoped = self.command_scope(self.focus_target().as_ref());
        if scoped.is_empty() {
            return false;
        }
        cx.emit(LiveGridEvent::Confirm {
            keys: scoped.shared_keys(),
            secondary,
        });
        true
    }

    /// Runs `edit` on the selection model with the rows and columns on
    /// screen, then emits what changed.
    fn edit_selection<T>(
        &mut self,
        origin: SelectionOrigin,
        cx: &mut Context<Self>,
        edit: impl FnOnce(
            &mut SelectionModel<KeyOf<D>, ColumnOf<D>>,
            ShownRows<'_, D::Row>,
            &ColumnLayout<ColumnOf<D>>,
        ) -> T,
    ) -> T {
        let mark = self.selection.mark();
        let (projection, snapshot) = (self.projection.clone(), self.snapshot.clone());
        let result = edit(
            &mut self.selection,
            Self::shown_rows(&projection, &snapshot, &*self.selection_policy),
            &self.columns,
        );
        self.finish_change(mark, origin, cx);
        result
    }

    /// Emits `SelectionChange`, then `FocusChange`, for what changed since
    /// `mark`; a new focus also becomes the current column. Returns true if
    /// anything changed.
    pub(super) fn finish_change(
        &mut self,
        mark: ChangeMark<KeyOf<D>, ColumnOf<D>>,
        origin: SelectionOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let (selection, focus) = self.selection.changes_since(&mark);
        if selection {
            cx.emit(LiveGridEvent::SelectionChange {
                selection: self.selection.selection().clone(),
                origin,
            });
        }
        if focus {
            if let Some(cell) = self.selection.focus() {
                self.set_current_column(cell.column().ix());
            }
            cx.emit(LiveGridEvent::FocusChange {
                cell: self.selection.focus().cloned(),
                origin,
            });
        }
        if selection || focus {
            cx.notify();
        }
        selection || focus
    }

    /// Moves the focus off a hidden column, after the `LayoutChange` that hid it.
    pub(super) fn move_focus_off_hidden(
        &mut self,
        origin: SelectionOrigin,
        cx: &mut Context<Self>,
    ) {
        self.edit_selection(origin, cx, |model, _, columns| {
            model.move_off_hidden(columns)
        });
    }

    /// The rows a page key moves by: the rows that fit in the row area.
    fn page_rows(&self) -> usize {
        page_size(self.rows_area_height(), self.row_height)
    }

    /// A keyboard move: the [`SelectionGesture`] edits the selection with
    /// the target, which gets the focus and is scrolled into view.
    pub(super) fn navigate(
        &mut self,
        navigation: Navigation,
        extending: bool,
        cx: &mut Context<Self>,
    ) {
        // ← and → on a caption expand, collapse and move along the tree
        // (WAI-ARIA treegrid).
        if matches!(navigation, Navigation::PrevColumn | Navigation::NextColumn)
            && let Some(path) = self
                .selection
                .focus()
                .and_then(|focus| focus.group())
                .cloned()
        {
            self.caption_arrow(&path, navigation == Navigation::NextColumn, cx);
            return;
        }
        let page = self.page_rows();
        let (projection, snapshot) = (self.projection.clone(), self.snapshot.clone());
        let rows = Self::shown_rows(&projection, &snapshot, &*self.selection_policy);
        let Some(target) = self
            .selection
            .navigation_target(navigation, page, rows, &self.columns)
        else {
            return;
        };
        let context = SelectionGestureContext::new(
            GestureInput::Keyboard,
            Modifiers::default(),
            extending,
            self.selection.is_selected(&target),
            self.selection.mode(),
        );
        let edit = self.selection_gesture.edit(&context);
        let mark = self.selection.mark();
        self.selection.apply(edit, target, rows, &self.columns);
        // Page keys scroll by a page, so the focus keeps its screen line.
        match navigation {
            Navigation::PageDown => self.scroll_row += page,
            Navigation::PageUp => self.scroll_row = self.scroll_row.saturating_sub(page),
            _ => {}
        }
        self.clamp_scroll();
        self.reveal_focus();
        self.finish_change(mark, SelectionOrigin::User, cx);
        cx.notify();
    }

    /// Scrolls the focused cell into view: its line by the nearest edge,
    /// its column if it scrolls (frozen zones do not). A focused caption
    /// does not scroll sideways: it is painted at the left edge.
    pub(super) fn reveal_focus(&mut self) {
        let Some(focus) = self.selection.focus() else {
            return;
        };
        let column = focus.column().ix();
        let is_caption = focus.group().is_some();
        let display_ix = ShownRows::new(&self.projection, &self.snapshot).line_of(focus);
        if let Some(display_ix) = display_ix {
            self.reveal_row(display_ix);
        }
        if !is_caption {
            let mut columns = ColumnSet::EMPTY;
            columns.insert(column);
            self.reveal_columns(columns);
        }
        self.clamp_scroll();
    }

    /// The keyboard Actions of the selection, on the grid root.
    pub(super) fn bind_selection_actions<E: InteractiveElement>(root: E, cx: &Context<Self>) -> E {
        use Navigation::*;
        root.on_action(cx.listener(|this, _: &SelectUp, _, cx| this.navigate(Up, false, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.navigate(Down, false, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPrevColumn, _, cx| {
                    this.navigate(PrevColumn, false, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &SelectNextColumn, _, cx| {
                    this.navigate(NextColumn, false, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &SelectPageUp, _, cx| this.navigate(PageUp, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectPageDown, _, cx| this.navigate(PageDown, false, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.navigate(First, false, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.navigate(Last, false, cx)))
            .on_action(
                cx.listener(|this, _: &SelectFirstRow, _, cx| this.navigate(FirstRow, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLastRow, _, cx| this.navigate(LastRow, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectFirstCell, _, cx| this.navigate(FirstCell, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLastCell, _, cx| this.navigate(LastCell, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendSelectionUp, _, cx| this.navigate(Up, true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendSelectionDown, _, cx| this.navigate(Down, true, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendSelectionPrevColumn, _, cx| {
                this.navigate(PrevColumn, true, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionNextColumn, _, cx| {
                this.navigate(NextColumn, true, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ExtendSelectionPageUp, _, cx| {
                    this.navigate(PageUp, true, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ExtendSelectionPageDown, _, cx| {
                this.navigate(PageDown, true, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ExtendSelectionToFirst, _, cx| {
                    this.navigate(First, true, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ExtendSelectionToLast, _, cx| this.navigate(Last, true, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendSelectionToFirstRow, _, cx| {
                this.navigate(FirstRow, true, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionToLastRow, _, cx| {
                this.navigate(LastRow, true, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionToFirstCell, _, cx| {
                this.navigate(FirstCell, true, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionToLastCell, _, cx| {
                this.navigate(LastCell, true, cx)
            }))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_toggle_selection))
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(Self::on_cancel))
    }

    /// Cmd/Ctrl+A; the single modes let the key go on.
    fn on_select_all(&mut self, _: &SelectAll, _: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        let handled = self.edit_selection(SelectionOrigin::User, cx, |model, rows, columns| {
            model.select_all(rows, columns)
        });
        if !handled {
            cx.propagate();
        }
    }

    /// Space; without a focused row the key goes on.
    fn on_toggle_selection(
        &mut self,
        _: &ToggleSelection,
        _: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let handled = self.edit_selection(SelectionOrigin::User, cx, |model, rows, columns| {
            model.toggle_focused(rows, columns)
        });
        if !handled {
            cx.propagate();
        }
    }

    /// Enter and Cmd/Ctrl+Enter; with nothing selected the key goes on, so
    /// a dialog's default button still answers it.
    fn on_confirm(&mut self, action: &Confirm, _: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        if !self.confirm(action.secondary, cx) {
            cx.propagate();
        }
    }

    /// Escape clears the selection (the focus and the anchor stay); with
    /// nothing selected the key goes on, so a dialog can close. During a
    /// row drag from this grid it cancels the drag instead.
    fn on_cancel(&mut self, _: &Cancel, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        if self.cancel_row_drag(window, cx) {
            return;
        }
        if self.selection.selection().is_empty() {
            cx.propagate();
            return;
        }
        self.edit_selection(SelectionOrigin::User, cx, |model, _, _| model.clear());
    }

    /// The key of the data row painted at a window position.
    fn row_at(&self, position: Point<Pixels>) -> Option<KeyOf<D>> {
        self.line_at(position)
            .and_then(|line| self.painted.get(line)?.row())
            .and_then(|id| self.snapshot.row(id))
            .map(|row| row.key().clone())
    }

    /// The column at a window position; left of the columns the first
    /// visible one, right of them the last.
    fn column_toward(&self, position: Point<Pixels>) -> Option<ColumnIx> {
        self.column_at_position(position).or_else(|| {
            if position.x < self.body_bounds.origin.x {
                first_column(&self.columns)
            } else {
                last_column(&self.columns)
            }
        })
    }

    /// A press on the body. The target is captured now, so a re-sort
    /// before the release cannot retarget it. A context press then opens
    /// the menu for it. On a group caption or a tree row's chevron the
    /// [`ExpandGesture`](crate::ExpandGesture) may expand or collapse the
    /// line; a caption then takes the focus, never the selection, and a
    /// tree row's chevron neither. Any press but one on a chevron may end
    /// in a click on its target.
    pub(crate) fn on_body_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let context = self.is_context_press(
            event.button,
            event.modifiers,
            event.click_count,
            GridArea::Body,
        );
        // The press that closes the menu changes nothing else: not the
        // selection, no drag, no double click. A context press opens a new
        // menu for its own target.
        if self.is_menu_open() && !context {
            self.press_series = None;
            return;
        }
        // A pinned row first: the bands are over the data rows' area.
        let (pinned, row) = match self.pinned_row_at(event.position) {
            Some((edge, key)) => (Some(edge), Some(key)),
            None => (None, self.row_at(event.position)),
        };
        // A group caption: its path and the aggregate cell under the press.
        let caption = pinned
            .is_none()
            .then(|| self.caption_hit(event.position))
            .flatten();
        let column = match &caption {
            Some((_, column)) => *column,
            None => row.as_ref().and(self.column_toward(event.position)),
        };
        self.pointer_target = Some(PointerTarget {
            row,
            group: caption.map(|(path, _)| path),
            column,
            button: event.button,
            position: event.position,
            pinned,
        });
        if context {
            self.context_press(event.modifiers, cx);
            self.open_body_menu(window, cx);
            return;
        }
        // Then the expand gesture, on a caption or a tree row's chevron.
        let disclosure = pinned
            .is_none()
            .then(|| self.disclosure_hit(event.position))
            .flatten();
        if let Some((line, area)) = &disclosure {
            self.press_disclosure(line, *area, event, cx);
        }
        let chevron = disclosure
            .as_ref()
            .and_then(|(line, area)| (*area == ExpandArea::Chevron).then_some(line));
        // Any other press may end in a `Click` on its target; a chevron is
        // a control, as a resize handle is.
        if chevron.is_none()
            && let Some(target) = self.pressed_target()
        {
            self.begin_click(target, event);
        }
        if pinned.is_some() {
            // A pinned row is not selected or focused: the press only
            // focuses the grid (the root does that), and a double click on
            // it does nothing.
            self.finish_press(cx);
            self.press_series = None;
        } else if event.button == MouseButton::Left {
            let group = self
                .pointer_target
                .as_ref()
                .and_then(|pointer| pointer.group())
                .cloned();
            if let Some(group) = group {
                // A caption takes the focus, not the selection.
                if let Some(column) = self.focus_column_or_first() {
                    self.focus_press(FocusedCell::caption(group, column), cx);
                }
            } else if chevron.is_some() {
                // A tree row's chevron is a control, as Finder's disclosure
                // triangle: the focus, the selection and the anchor stay; no
                // drag, no double click.
                self.finish_press(cx);
                self.press_series = None;
            } else {
                self.primary_press(event.modifiers, event.click_count, window, cx);
            }
        }
    }

    /// The focused column, if visible, else the first visible one.
    fn focus_column_or_first(&self) -> Option<ColumnOf<D>> {
        self.selection
            .focus()
            .map(|focus| focus.column().ix())
            .filter(|column| self.columns.view_column_ix(*column).is_some())
            .or_else(|| first_column(&self.columns))
            .map(column_of)
    }

    /// A primary press that only moves the focus to `target`, a caption:
    /// no selection, no drag, no double click. Its `FocusChange` comes on
    /// the release.
    pub(super) fn focus_press(
        &mut self,
        target: FocusedCell<KeyOf<D>, ColumnOf<D>>,
        cx: &mut Context<Self>,
    ) {
        self.finish_press(cx);
        self.press_series = None;
        let mark = self.selection.mark();
        self.selection.focus_on(target.clone());
        let origin = self
            .pointer_target
            .as_ref()
            .map_or(Point::default(), PointerTarget::position);
        self.press = Some(Press {
            mark,
            base: None,
            target,
            origin,
            deferred: false,
            adds: false,
            moved: false,
            focus_only: true,
            drag_candidate: None,
            drag_tried: false,
        });
        cx.notify();
    }

    /// The data row and cell of the last press, as a target; a pinned row
    /// is none.
    fn pressed_cell(&self) -> Option<FocusedCell<KeyOf<D>, ColumnOf<D>>> {
        let pointer = self
            .pointer_target
            .as_ref()
            .filter(|pointer| pointer.pinned.is_none())?;
        let key = pointer.row()?.clone();
        Some(FocusedCell::new(key, column_of(pointer.column()?)))
    }

    /// A primary press on a data cell (or the empty area below the rows):
    /// the selection gesture, a drag-selection, and the second press of a
    /// series; the host of a cell's element reaches it for a press the
    /// element did not stop.
    pub(super) fn primary_press(
        &mut self,
        modifiers: Modifiers,
        click_count: usize,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        // A release this grid never saw ends the last gesture first.
        self.finish_press(cx);
        let pressed = self.pressed_cell();
        if click_count >= 2 {
            // A double click on a cell with an editor edits it; otherwise it
            // confirms the row of the series' first press, whatever moved
            // under the pointer since.
            if self.start_edit_by_press(click_count, window, cx) {
                return;
            }
            // A double click on a link's text: its first click opened the
            // link, which is the cell's command.
            if self.links.series_on_link {
                return;
            }
            if let Some(key) = self.press_series.clone() {
                self.confirm_row(&key, cx);
            }
            return;
        }
        self.press_series = pressed.as_ref().and_then(|cell| cell.row().cloned());
        self.links.series_on_link = self.is_pointer_on_link();
        // The empty area below the rows: nothing to select.
        let Some(target) = pressed else {
            return;
        };
        let selected = self.selection.is_selected(&target);
        // A press the drag gesture takes may drag rows.
        let drag_candidate = target
            .row()
            .is_some_and(|key| {
                self.is_row_drag_press(key, target.column(), selected, modifiers, click_count)
            })
            .then_some(modifiers);
        let context = SelectionGestureContext::new(
            GestureInput::Pointer,
            modifiers,
            false,
            selected,
            self.selection.mode(),
        );
        let edit = self.selection_gesture.edit(&context);
        let mark = self.selection.mark();
        let base = (edit == SelectionEdit::AddRange).then(|| self.selection.selection().clone());
        let deferred =
            edit == SelectionEdit::Replace && selected && self.selection.holds_more_than(&target);
        if deferred {
            self.selection.focus_on(target.clone());
        } else {
            let (projection, snapshot) = (self.projection.clone(), self.snapshot.clone());
            self.selection.apply(
                edit,
                target.clone(),
                Self::shown_rows(&projection, &snapshot, &*self.selection_policy),
                &self.columns,
            );
        }
        let origin = self
            .pointer_target
            .as_ref()
            .map_or(Point::default(), PointerTarget::position);
        self.press = Some(Press {
            mark,
            base,
            target,
            origin,
            deferred,
            adds: matches!(edit, SelectionEdit::Toggle | SelectionEdit::AddRange),
            moved: false,
            focus_only: false,
            drag_candidate,
            drag_tried: false,
        });
        cx.notify();
    }

    /// The menu gesture: an unselected row is selected first (Windows
    /// convention), a selected one keeps the selection; the row gets the
    /// focus. A caption gets the focus and the selection stays. The events
    /// come at once, before the menu is built.
    pub(super) fn context_press(&mut self, modifiers: Modifiers, cx: &mut Context<Self>) {
        self.finish_press(cx);
        if let Some(group) = self
            .pointer_target
            .as_ref()
            .and_then(|pointer| pointer.group())
            .cloned()
        {
            self.edit_selection(SelectionOrigin::User, cx, |model, rows, columns| {
                model.focus_caption(&group, rows, columns)
            });
            return;
        }
        let Some(target) = self.pressed_cell() else {
            return;
        };
        let context = SelectionGestureContext::new(
            GestureInput::ContextPointer,
            modifiers,
            false,
            self.selection.is_selected(&target),
            self.selection.mode(),
        );
        let edit = self.selection_gesture.edit(&context);
        self.edit_selection(SelectionOrigin::User, cx, |model, rows, columns| {
            model.apply(edit, target, rows, columns)
        });
    }

    /// The second press of a series: `Confirm` for the row of the first
    /// one. Its scope of its own, if the selection policy gives it one and
    /// it is not empty (a tree row with children under `FocusBranches`: its
    /// subtree's leaves); else the scoped selection, if the row is in it.
    fn confirm_row(&mut self, key: &KeyOf<D>, cx: &mut Context<Self>) {
        let scoped = match self.line_scope(&LineTarget::Row(key.clone())) {
            Some(scope) => Some(scope).filter(|scope| !scope.is_empty()),
            None => Some(self.scoped_selection()).filter(|scope| scope.keys().contains(key)),
        };
        if let Some(scoped) = scoped {
            cx.emit(LiveGridEvent::Confirm {
                keys: scoped.shared_keys(),
                secondary: false,
            });
        }
    }

    /// The pointer moved with the primary button down: past the threshold a
    /// press becomes a drag-selection, and the range follows the pointer.
    pub(crate) fn on_body_drag_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(press) = &mut self.press else {
            return;
        };
        // A press on a caption starts no drag-selection.
        if press.focus_only {
            return;
        }
        if !self.selection.is_dragging() {
            if !is_past_drag_threshold(press.origin, position) {
                return;
            }
            press.moved = true;
            // A press that may drag rows starts no drag-selection; past the
            // gesture's threshold it arms the drag, or, refused, does
            // nothing more.
            if let Some(modifiers) = press.drag_candidate {
                let distance = (position - press.origin).magnitude();
                let threshold = f64::from(f32::from(self.drag.threshold()));
                if !press.drag_tried && distance >= threshold {
                    press.drag_tried = true;
                    if let Some(pressed) = press.target.row().cloned() {
                        self.arm_row_drag(&pressed, modifiers, cx);
                    }
                }
                return;
            }
            if press.deferred {
                // Dragging a selection drags its rows, so it does not start
                // a new selection.
                return;
            }
            let target = press.target.clone();
            let adds = press.adds;
            let base = match press.base.take() {
                Some(base) => base,
                None if adds => self.selection.before_toggle(&target),
                None => GridSelection::default(),
            };
            self.selection.begin_drag(adds, base, target);
        }
        self.drag_scroll.last_drag_position = Some(position);
        self.update_drag_target(position, cx);
        // Above or below the data rows (over a pinned band too), the rows
        // scroll (`AutoScroll`, as a header drag scrolls the columns).
        let delta = AutoScroll::compute_delta(position.y, self.rows_bounds());
        self.drag_scroll.set(delta, cx, |delta, state, cx| {
            state.auto_scroll_rows(delta, cx)
        });
    }

    /// The drag's end: the row under `position`, held within the rows on
    /// screen, and the column under it.
    fn update_drag_target(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(target) = self.drag_target_at(position) else {
            return;
        };
        if self.selection.drag_to(target) {
            cx.notify();
        }
    }

    fn drag_target_at(
        &self,
        position: Point<Pixels>,
    ) -> Option<FocusedCell<KeyOf<D>, ColumnOf<D>>> {
        let rows = ShownRows::new(&self.projection, &self.snapshot);
        let lines = ((self.rows_area_height() + self.scroll_offset) / self.row_height).ceil();
        let lines = (lines as usize).max(1);
        let y = position.y - self.rows_bounds().origin.y + self.scroll_offset;
        let line = if y < px(0.) {
            0
        } else {
            ((y / self.row_height).floor() as usize).min(lines - 1)
        };
        let display_ix = rows.nearest_live(self.scroll_row + line)?;
        let key = rows.key_at(display_ix)?.clone();
        let column = self
            .column_at_position(position)
            .or_else(|| {
                self.selection
                    .drag_target()
                    .map(|target| target.column().ix())
            })
            .or_else(|| self.column_toward(position))?;
        Some(FocusedCell::new(key, column_of(column)))
    }

    /// One auto-scroll step of a drag-selection; the range follows the last
    /// pointer position.
    fn auto_scroll_rows(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        if !self.selection.is_dragging() {
            self.drag_scroll.stop();
            return;
        }
        let before = (self.scroll_row, self.scroll_offset);
        self.scroll_rows_by(delta);
        if (self.scroll_row, self.scroll_offset) == before {
            return;
        }
        if let Some(position) = self.drag_scroll.last_drag_position {
            self.update_drag_target(position, cx);
        }
        cx.notify();
    }

    /// The primary button went up anywhere in the window.
    pub(crate) fn on_body_mouse_up(&mut self, cx: &mut Context<Self>) {
        self.finish_press(cx);
    }

    /// Ends the press: a drag fixes its range, a deferred collapse applies
    /// `Replace` unless the pointer moved; then one `SelectionChange` and one
    /// `FocusChange` for the whole gesture.
    fn finish_press(&mut self, cx: &mut Context<Self>) {
        let Some(press) = self.press.take() else {
            return;
        };
        self.drag_scroll.stop();
        // The row drag, if any, ends with the press.
        self.unarm_row_drag();
        let (projection, snapshot) = (self.projection.clone(), self.snapshot.clone());
        let rows = Self::shown_rows(&projection, &snapshot, &*self.selection_policy);
        if self.selection.is_dragging() {
            self.selection.finish_drag(rows, &self.columns);
        } else if press.deferred
            && !press.moved
            && press
                .target
                .row()
                .and_then(|key| rows.display_ix(key))
                .is_some()
        {
            self.selection
                .apply(SelectionEdit::Replace, press.target, rows, &self.columns);
        }
        self.finish_change(press.mark, SelectionOrigin::User, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        sync::Arc,
        time::Instant,
    };

    use super::*;
    use crate::{
        engine::{ProjectionEngine as _, Rebuild, SortDirection, ViewSpec},
        highlight::HighlightPolicy,
        model::CellValue,
        policy::{ProjectionScope, StandardSelectionGesture},
        store::{Ingest, Op},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct Col(u16);

    const COLUMNS: [Col; 4] = [Col(0), Col(1), Col(2), Col(3)];

    impl GridColumns for Col {
        fn all() -> &'static [Self] {
            &COLUMNS
        }

        fn id(self) -> &'static str {
            ["c0", "c1", "c2", "c3"][self.0 as usize]
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self.0)
        }
    }

    #[derive(Debug)]
    struct Row {
        id: u32,
        price: i64,
    }

    impl GridRow for Row {
        type Key = u32;
        type Column = Col;

        fn key(&self) -> &u32 {
            &self.id
        }

        fn value(&self, column: Col) -> CellValue<'_> {
            match column.0 {
                0 => CellValue::Int(self.id.into()),
                1 => CellValue::Int(self.price),
                _ => CellValue::Empty,
            }
        }
    }

    fn row(id: u32, price: i64) -> Op<Row> {
        Op::Upsert(Arc::new(Row { id, price }))
    }

    /// Visible columns in display order; the others are hidden.
    struct Visible(Vec<ColumnIx>);

    impl ColumnView for Visible {
        fn view_column_ix(&self, column: ColumnIx) -> Option<ViewColumnIx> {
            let position = self.0.iter().position(|ix| *ix == column)?;
            Some(ViewColumnIx::new(position as u16))
        }

        fn column_at(&self, view: ViewColumnIx) -> Option<ColumnIx> {
            self.0.get(view.get()).copied()
        }

        fn visible_count(&self) -> usize {
            self.0.len()
        }

        fn visible_set(&self) -> ColumnSet {
            set(self.0.iter().copied())
        }

        fn visible_neighbour(&self, column: ColumnIx) -> Option<ColumnIx> {
            // Schema order stands in for the display order here.
            let right = self.0.iter().copied().filter(|ix| *ix > column).min();
            right.or_else(|| self.0.iter().copied().filter(|ix| *ix < column).max())
        }
    }

    fn set(columns: impl IntoIterator<Item = ColumnIx>) -> ColumnSet {
        columns.into_iter().fold(ColumnSet::EMPTY, |mut set, ix| {
            set.insert(ix);
            set
        })
    }

    fn cols(columns: &[u16]) -> ColumnSet {
        set(columns.iter().copied().map(ColumnIx::new))
    }

    fn all_visible() -> Visible {
        Visible((0..4).map(ColumnIx::new).collect())
    }

    /// A store, its snapshot, and the projection on screen.
    struct World {
        ingest: Ingest<Row>,
        snapshot: Arc<StoreSnapshot<Row>>,
        projection: Projection,
    }

    impl World {
        /// Rows `0..count`, `price = id`, shown in insertion order.
        fn new(count: u32) -> Self {
            let mut ingest = Ingest::new(HighlightPolicy::default(), Instant::now());
            ingest.apply((0..count).map(|id| row(id, id.into())), Instant::now());
            let snapshot = ingest.snapshot();
            let projection = Rebuild::default().rebuild(&snapshot, &ViewSpec::new());
            Self {
                ingest,
                snapshot,
                projection,
            }
        }

        fn rows(&self) -> ShownRows<'_, Row> {
            ShownRows::new(&self.projection, &self.snapshot)
        }

        /// Applies `ops`; the projection on screen stays (tombstones).
        fn apply(
            &mut self,
            ops: impl IntoIterator<Item = Op<Row>>,
        ) -> (Arc<StoreSnapshot<Row>>, crate::store::ChangeSet) {
            let changes = self.ingest.apply(ops, Instant::now());
            let previous = std::mem::replace(&mut self.snapshot, self.ingest.snapshot());
            (previous, changes)
        }

        /// Rebuilds the projection; returns the one it replaced.
        fn rebuild(&mut self, view: &ViewSpec<Row>) -> Projection {
            std::mem::replace(
                &mut self.projection,
                Rebuild::default().rebuild(&self.snapshot, view),
            )
        }
    }

    fn at(key: u32, column: u16) -> FocusedCell<u32, Col> {
        FocusedCell::new(key, Col(column))
    }

    fn keys(model: &SelectionModel<u32, Col>) -> Vec<u32> {
        let mut keys: Vec<u32> = model.selection().iter().copied().collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn test_selection_edits() {
        use SelectionEdit::*;
        let world = World::new(20);
        let columns = all_visible();
        let rows = world.rows();

        // RowSet: every edit, the anchor, and every column of a row.
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSet);
        model.apply(Replace, at(3, 1), rows, &columns);
        assert_eq!(keys(&model), [3]);
        assert_eq!(model.selection().columns(&3), ColumnSet::first(4));
        model.apply(Extend, at(6, 2), rows, &columns);
        assert_eq!(keys(&model), [3, 4, 5, 6]);
        assert_eq!(model.focus(), Some(&at(6, 2)));
        model.apply(Extend, at(1, 0), rows, &columns);
        assert_eq!(keys(&model), [1, 2, 3], "Extend drops the rest");
        model.apply(Toggle, at(9, 0), rows, &columns);
        assert_eq!(keys(&model), [1, 2, 3, 9]);
        model.apply(AddRange, at(11, 0), rows, &columns);
        assert_eq!(keys(&model), [1, 2, 3, 9, 10, 11], "the anchor is 9");
        model.apply(Toggle, at(2, 0), rows, &columns);
        assert_eq!(keys(&model), [1, 3, 9, 10, 11]);
        model.apply(Keep, at(15, 3), rows, &columns);
        assert_eq!(keys(&model), [1, 3, 9, 10, 11]);
        assert_eq!(model.focus(), Some(&at(15, 3)));
        // Keep left the anchor at 2.
        model.apply(Extend, at(4, 0), rows, &columns);
        assert_eq!(keys(&model), [2, 3, 4]);

        // RowSingle: Extend and AddRange replace; Toggle selects or clears.
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSingle);
        model.apply(Replace, at(3, 0), rows, &columns);
        model.apply(Extend, at(6, 0), rows, &columns);
        assert_eq!(keys(&model), [6]);
        model.apply(AddRange, at(8, 0), rows, &columns);
        assert_eq!(keys(&model), [8]);
        model.apply(Toggle, at(8, 0), rows, &columns);
        assert!(model.selection().is_empty());
        model.apply(Toggle, at(2, 0), rows, &columns);
        assert_eq!(keys(&model), [2]);

        // CellSet: ranges of cells by `ViewColumnIx`, hidden columns left
        // out; columns visible in the order c0, c3, c1 with c2 hidden.
        let shuffled = Visible([0, 3, 1].map(ColumnIx::new).to_vec());
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::CellSet);
        model.apply(Replace, at(3, 0), rows, &shuffled);
        assert_eq!(model.selection().columns(&3), cols(&[0]));
        model.apply(Extend, at(5, 3), rows, &shuffled);
        assert_eq!(keys(&model), [3, 4, 5]);
        assert_eq!(model.selection().columns(&4), cols(&[0, 3]));
        model.apply(Extend, at(4, 1), rows, &shuffled);
        assert_eq!(keys(&model), [3, 4]);
        assert_eq!(model.selection().columns(&3), cols(&[0, 1, 3]));
        assert!(!model.selection().contains_cell(&3, ColumnIx::new(2)));
        model.apply(Toggle, at(3, 3), rows, &shuffled);
        assert_eq!(model.selection().columns(&3), cols(&[0, 1]));
        model.apply(Toggle, at(10, 1), rows, &shuffled);
        model.apply(AddRange, at(11, 1), rows, &shuffled);
        assert_eq!(model.selection().columns(&11), cols(&[1]));
        assert_eq!(model.selection().columns(&4), cols(&[0, 1, 3]));
        // Toggling a row's last cell drops the row.
        model.apply(Toggle, at(11, 1), rows, &shuffled);
        assert!(!model.selection().contains(&11));
        // `Replace` on a selected cell of a larger selection collapses.
        assert!(model.holds_more_than(&at(4, 0)));
        assert!(model.is_selected(&at(4, 0)));
        assert!(!model.is_selected(&at(4, 2)));

        // CellSingle: one cell.
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::CellSingle);
        model.apply(Replace, at(3, 0), rows, &shuffled);
        model.apply(Extend, at(5, 1), rows, &shuffled);
        assert_eq!(keys(&model), [5]);
        assert_eq!(model.selection().columns(&5), cols(&[1]));
        assert!(!model.holds_more_than(&at(5, 1)));

        // Rows removed after the projection was built are not in a range.
        let mut world = World::new(20);
        world.apply([Op::Remove(4), Op::Remove(5)]);
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSet);
        model.apply(Replace, at(3, 0), world.rows(), &columns);
        model.apply(Extend, at(7, 0), world.rows(), &columns);
        assert_eq!(keys(&model), [3, 6, 7]);
        assert!(model.select_all(world.rows(), &columns));
        assert_eq!(model.selection().len(), 18);
        // Select all in the cell modes takes the visible cells; none in the
        // single modes.
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::CellSet);
        assert!(model.select_all(world.rows(), &shuffled));
        assert_eq!(model.selection().columns(&0), cols(&[0, 1, 3]));
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSingle);
        assert!(!model.select_all(world.rows(), &columns));
    }

    #[test]
    fn test_mode_changes_and_programmatic_selection() {
        let world = World::new(10);
        let columns = all_visible();
        let rows = world.rows();
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSet);
        model.apply(SelectionEdit::Replace, at(2, 1), rows, &columns);
        model.apply(SelectionEdit::Extend, at(4, 1), rows, &columns);
        // A set mode to a single one keeps the focused row.
        model.set_mode(SelectionMode::RowSingle);
        assert_eq!(keys(&model), [4]);
        // Rows to cells clears.
        model.set_mode(SelectionMode::CellSingle);
        assert!(model.selection().is_empty());

        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSet);
        model.set_rows([1, 3, 99], &world.snapshot, &columns);
        assert_eq!(keys(&model), [1, 3], "a key the snapshot lacks is dropped");
        model.set_cells([(5, Col(2)), (6, Col(1))], &world.snapshot);
        assert_eq!(keys(&model), [5, 6]);
        assert_eq!(model.selection().columns(&5), ColumnSet::first(4));
        model.set_mode(SelectionMode::CellSet);
        model.set_cells([(5, Col(2)), (5, Col(1)), (99, Col(1))], &world.snapshot);
        assert_eq!(model.selection().columns(&5), cols(&[1, 2]));
        model.set_rows([7], &world.snapshot, &columns);
        assert_eq!(model.selection().columns(&7), ColumnSet::first(4));

        // `select_row` moves the focus and the anchor to a row on screen.
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSingle);
        model.select_row(Some(8), rows, &columns);
        assert_eq!(keys(&model), [8]);
        assert_eq!(model.focus(), Some(&at(8, 0)));
        model.select_row(Some(99), rows, &columns);
        assert!(model.selection().is_empty());
        assert_eq!(model.focus(), Some(&at(8, 0)));
        assert!(model.set_focus(&3, Some(Col(2)), rows, &columns));
        assert_eq!(model.focus(), Some(&at(3, 2)));
        assert!(model.set_focus(&5, None, rows, &columns));
        assert_eq!(model.focus(), Some(&at(5, 2)), "None keeps the column");
        assert!(!model.set_focus(&99, None, rows, &columns));

        // A hidden focus column moves right, else left.
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSingle);
        model.set_focus(&3, Some(Col(1)), rows, &columns);
        model.move_off_hidden(&Visible([0, 2, 3].map(ColumnIx::new).to_vec()));
        assert_eq!(model.focus(), Some(&at(3, 2)));
        model.set_focus(&3, Some(Col(3)), rows, &columns);
        model.move_off_hidden(&Visible([0, 1].map(ColumnIx::new).to_vec()));
        assert_eq!(model.focus(), Some(&at(3, 1)));

        assert_eq!(SelectionMode::RowSet.selection_unit(), SelectionUnit::Row);
        assert_eq!(
            SelectionMode::CellSingle.selection_unit(),
            SelectionUnit::Cell
        );
    }

    #[test]
    fn test_navigation_targets() {
        let mut world = World::new(20);
        let columns = Visible([0, 3, 1].map(ColumnIx::new).to_vec());
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSingle);
        let target = |model: &SelectionModel<u32, Col>, world: &World, navigation| {
            model
                .navigation_target(navigation, 5, world.rows(), &columns)
                .expect("a target")
        };
        // Without a focus: the first cell, or the last row or column.
        assert_eq!(target(&model, &world, Navigation::Down), at(0, 0));
        assert_eq!(target(&model, &world, Navigation::Last), at(0, 1));
        assert_eq!(target(&model, &world, Navigation::LastCell), at(19, 1));
        assert_eq!(target(&model, &world, Navigation::LastRow), at(19, 0));

        model.set_focus(&10, Some(Col(3)), world.rows(), &columns);
        assert_eq!(target(&model, &world, Navigation::Up), at(9, 3));
        assert_eq!(target(&model, &world, Navigation::PageDown), at(15, 3));
        assert_eq!(target(&model, &world, Navigation::PageUp), at(5, 3));
        assert_eq!(target(&model, &world, Navigation::PrevColumn), at(10, 0));
        assert_eq!(target(&model, &world, Navigation::NextColumn), at(10, 1));
        assert_eq!(target(&model, &world, Navigation::First), at(10, 0));
        assert_eq!(target(&model, &world, Navigation::FirstCell), at(0, 0));
        assert_eq!(target(&model, &world, Navigation::LastRow), at(19, 3));
        // No wrap at the edges.
        model.set_focus(&19, Some(Col(1)), world.rows(), &columns);
        assert_eq!(target(&model, &world, Navigation::Down), at(19, 1));
        assert_eq!(target(&model, &world, Navigation::NextColumn), at(19, 1));
        assert_eq!(target(&model, &world, Navigation::PageDown), at(19, 1));

        // Tombstones: the move goes on past them in the same direction, and
        // stays where none is left.
        world.apply([Op::Remove(11), Op::Remove(12), Op::Remove(0)]);
        model.set_focus(&10, None, world.rows(), &columns);
        assert_eq!(target(&model, &world, Navigation::Down), at(13, 1));
        assert_eq!(target(&model, &world, Navigation::FirstRow), at(1, 1));
        model.set_focus(&1, None, world.rows(), &columns);
        assert_eq!(target(&model, &world, Navigation::Up), at(1, 1));
    }

    #[test]
    fn test_successor_rule() {
        let columns = all_visible();
        let mut world = World::new(10);
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSingle);
        model.apply(SelectionEdit::Replace, at(5, 2), world.rows(), &columns);

        // Removed: the next row by the old order, the column kept.
        let (previous, _) = world.apply([Op::Remove(5)]);
        model.follow_removed(&world.projection, &previous, &world.snapshot);
        assert_eq!(model.focus(), Some(&at(6, 2)));
        assert_eq!(model.anchor, Some(at(6, 2)), "the anchor follows too");
        // The selection does not follow the focus in a single mode.
        model.remove_rows(&[], &previous);
        // Removed with the rows after it: the one before.
        let (previous, _) = world.apply([6, 7, 8, 9].map(Op::Remove));
        model.follow_removed(&world.projection, &previous, &world.snapshot);
        assert_eq!(model.focus(), Some(&at(4, 2)));
        // Everything removed: no focus.
        let (previous, _) = world.apply([0, 1, 2, 3, 4].map(Op::Remove));
        model.follow_removed(&world.projection, &previous, &world.snapshot);
        assert_eq!(model.focus(), None);

        // Filtered: the nearest row of the old order the new one shows.
        let mut world = World::new(10);
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSet);
        model.apply(SelectionEdit::Replace, at(5, 0), world.rows(), &columns);
        let even = ViewSpec::new().with_filter(ColumnSet::first(1), |row: &Row| row.id % 2 == 0);
        let previous = world.rebuild(&even);
        model.follow_filtered(&previous, &world.projection, &world.snapshot);
        assert_eq!(model.focus(), Some(&at(6, 0)));
        assert_eq!(keys(&model), [5], "a filter keeps the selection");
        // The last shown row filtered out: the one before it.
        model.set_focus(&8, None, world.rows(), &columns);
        let low = ViewSpec::new().with_filter(ColumnSet::first(1), |row: &Row| row.id < 5);
        let previous = world.rebuild(&low);
        model.follow_filtered(&previous, &world.projection, &world.snapshot);
        assert_eq!(model.focus(), Some(&at(4, 0)));
        // A focus the new order still shows stays, even if it moved.
        let descending = ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]);
        let previous = world.rebuild(&descending);
        model.follow_filtered(&previous, &world.projection, &world.snapshot);
        assert_eq!(model.focus(), Some(&at(4, 0)));
        // Nothing left to show: no focus.
        let none = ViewSpec::new().with_filter(ColumnSet::first(1), |_: &Row| false);
        let previous = world.rebuild(&none);
        model.follow_filtered(&previous, &world.projection, &world.snapshot);
        assert_eq!(model.focus(), None);
    }

    #[test]
    fn test_removed_and_reset_keys() {
        let columns = all_visible();
        let mut world = World::new(10);
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSet);
        model.apply(SelectionEdit::Replace, at(2, 0), world.rows(), &columns);
        model.apply(SelectionEdit::Extend, at(5, 0), world.rows(), &columns);

        let (previous, changes) = world.apply([Op::Remove(3)]);
        model.remove_rows(changes.removed(), &previous);
        assert_eq!(keys(&model), [2, 4, 5]);
        // Removed and inserted again in one batch: a new row, not selected.
        let (previous, changes) = world.apply([Op::Remove(4), row(4, 4)]);
        model.remove_rows(changes.removed(), &previous);
        assert_eq!(keys(&model), [2, 5]);
        assert!(world.snapshot.get(&4).is_some());

        // A new data set keeps only the keys it has; the focus (on 5) goes
        // with its key.
        let (previous, changes) = world.apply([Op::ReplaceAll(
            [2, 8].map(|id| Arc::new(Row { id, price: 0 })).to_vec(),
        )]);
        assert!(changes.is_reset());
        model.remove_rows(changes.removed(), &previous);
        model.retain_present(&world.snapshot);
        assert_eq!(keys(&model), [2]);
        assert_eq!(model.focus(), None);
        assert_eq!(model.anchor, Some(at(2, 0)));
    }

    #[test]
    fn test_drag_painter_and_finish() {
        let world = World::new(20);
        let columns = all_visible();
        let mut model = SelectionModel::<u32, Col>::new(SelectionMode::RowSet);
        model.apply(SelectionEdit::Replace, at(2, 0), world.rows(), &columns);
        let base = model.selection().clone();
        // An extending drag paints only its range, by display position.
        model.begin_drag(false, base.clone(), at(2, 0));
        assert!(model.drag_to(at(5, 0)));
        assert!(!model.drag_to(at(5, 0)));
        let painter = model.painter(world.rows(), &columns);
        assert_eq!(painter.columns(&4, 4), ColumnSet::first(4));
        assert_eq!(painter.columns(&6, 6), ColumnSet::EMPTY);
        assert_eq!(keys(&model), [2], "nothing is selected before the release");
        model.finish_drag(world.rows(), &columns);
        assert_eq!(keys(&model), [2, 3, 4, 5]);
        assert_eq!(model.focus(), Some(&at(5, 0)));

        // An adding drag paints and selects over the selection before it.
        model.apply(SelectionEdit::Toggle, at(10, 0), world.rows(), &columns);
        model.begin_drag(true, base, at(10, 0));
        model.drag_to(at(12, 0));
        let painter = model.painter(world.rows(), &columns);
        assert_eq!(painter.columns(&2, 2), ColumnSet::first(4));
        assert_eq!(painter.columns(&3, 3), ColumnSet::EMPTY, "not in the base");
        assert_eq!(painter.columns(&11, 11), ColumnSet::first(4));
        model.finish_drag(world.rows(), &columns);
        assert_eq!(keys(&model), [2, 10, 11, 12]);
    }

    // -----------------------------------------------------------------------
    // The model against a naive reference: `Vec` order, `HashSet` keys.

    /// The naive store and screen: keys with price and insertion sequence;
    /// the screen's rows with the generation they were shown at.
    #[derive(Default)]
    struct Reference {
        store: HashMap<u32, (i64, u64)>,
        generation: HashMap<u32, u64>,
        next_seq: u64,
        shown: Vec<(u32, u64)>,
        selected: HashSet<u32>,
        focus: Option<u32>,
        anchor: Option<u32>,
        single: bool,
    }

    impl Reference {
        fn generation_of(generation: &HashMap<u32, u64>, key: u32) -> u64 {
            generation.get(&key).copied().unwrap_or(0)
        }

        fn is_live(&self, (key, generation): (u32, u64)) -> bool {
            self.store.contains_key(&key)
                && Self::generation_of(&self.generation, key) == generation
        }

        /// Positions of the live rows on screen, keyed.
        fn position(&self, key: u32) -> Option<usize> {
            self.shown
                .iter()
                .position(|entry| entry.0 == key && self.is_live(*entry))
        }

        fn live_keys(&self) -> Vec<u32> {
            self.shown
                .iter()
                .filter(|entry| self.is_live(**entry))
                .map(|entry| entry.0)
                .collect()
        }

        fn upsert(&mut self, key: u32, price: i64) {
            match self.store.get_mut(&key) {
                Some(entry) => entry.0 = price,
                None => {
                    self.store.insert(key, (price, self.next_seq));
                    self.next_seq += 1;
                }
            }
        }

        /// Returns true if the key was in the store.
        fn remove(&mut self, key: u32) -> bool {
            let removed = self.store.remove(&key).is_some();
            if removed {
                *self.generation.entry(key).or_default() += 1;
                self.selected.remove(&key);
            }
            removed
        }

        fn rebuild(&self, sort: i8, filter: Option<u32>) -> Vec<(u32, u64)> {
            let mut rows: Vec<(u32, i64, u64)> = self
                .store
                .iter()
                .filter(|(key, _)| filter.is_none_or(|r| **key % 3 != r))
                .map(|(key, (price, seq))| (*key, *price, *seq))
                .collect();
            rows.sort_by_key(|(_, price, seq)| (i64::from(sort) * price, *seq));
            rows.into_iter()
                .map(|(key, _, _)| (key, self.generation.get(&key).copied().unwrap_or(0)))
                .collect()
        }

        /// The successor of `key` along `order` among rows `accept` takes.
        fn successor(
            order: &[(u32, u64)],
            key: u32,
            accept: impl Fn((u32, u64)) -> bool,
        ) -> Option<u32> {
            let position = order.iter().position(|entry| entry.0 == key);
            match position {
                Some(position) => order[position + 1..]
                    .iter()
                    .copied()
                    .find(|entry| accept(*entry))
                    .or_else(|| {
                        order[..position]
                            .iter()
                            .rev()
                            .copied()
                            .find(|entry| accept(*entry))
                    })
                    .map(|entry| entry.0),
                None => order
                    .iter()
                    .copied()
                    .find(|entry| accept(*entry))
                    .map(|entry| entry.0),
            }
        }

        fn range(&self, from: u32, to: u32) -> Option<Vec<u32>> {
            let (from, to) = (self.position(from)?, self.position(to)?);
            Some(
                self.shown[from.min(to)..=from.max(to)]
                    .iter()
                    .filter(|entry| self.is_live(**entry))
                    .map(|entry| entry.0)
                    .collect(),
            )
        }

        fn apply(&mut self, edit: SelectionEdit, target: u32) {
            let edit = match edit {
                SelectionEdit::Extend | SelectionEdit::AddRange if self.single => {
                    SelectionEdit::Replace
                }
                edit => edit,
            };
            let range = self.anchor.and_then(|anchor| self.range(anchor, target));
            match (edit, range) {
                (SelectionEdit::Extend, Some(range)) => self.selected = range.into_iter().collect(),
                (SelectionEdit::AddRange, Some(range)) => self.selected.extend(range),
                (SelectionEdit::Replace | SelectionEdit::Extend | SelectionEdit::AddRange, _) => {
                    self.selected = HashSet::from([target]);
                    self.anchor = Some(target);
                }
                (SelectionEdit::Toggle, _) => {
                    if self.single {
                        self.selected = if self.selected.contains(&target) {
                            HashSet::new()
                        } else {
                            HashSet::from([target])
                        };
                    } else if !self.selected.remove(&target) {
                        self.selected.insert(target);
                    }
                    self.anchor = Some(target);
                }
                _ => {}
            }
            self.focus = Some(target);
        }

        fn navigate(&self, navigation: Navigation, page: usize) -> Option<u32> {
            let live: Vec<usize> = (0..self.shown.len())
                .filter(|ix| self.is_live(self.shown[*ix]))
                .collect();
            let Some(origin) = self.focus.and_then(|key| self.position(key)) else {
                let ix = match navigation {
                    Navigation::LastRow | Navigation::LastCell => live.last()?,
                    _ => live.first()?,
                };
                return Some(self.shown[*ix].0);
            };
            let end = self.shown.len() - 1;
            let target = match navigation {
                Navigation::Up => origin.saturating_sub(1),
                Navigation::Down => (origin + 1).min(end),
                Navigation::PageUp => origin.saturating_sub(page),
                Navigation::PageDown => (origin + page).min(end),
                Navigation::FirstRow | Navigation::FirstCell => 0,
                Navigation::LastRow | Navigation::LastCell => end,
                _ => origin,
            };
            let ix = if target > origin {
                live.iter().copied().find(|ix| *ix >= target).or_else(|| {
                    live.iter()
                        .copied()
                        .rev()
                        .find(|ix| *ix > origin && *ix < target)
                })
            } else if target < origin {
                live.iter()
                    .copied()
                    .rev()
                    .find(|ix| *ix <= target)
                    .or_else(|| live.iter().copied().find(|ix| *ix > target && *ix < origin))
            } else {
                None
            };
            Some(self.shown[ix.unwrap_or(origin)].0)
        }
    }

    fn view(sort: i8, filter: Option<u32>) -> ViewSpec<Row> {
        let view = match sort {
            1 => ViewSpec::new().with_sort([(Col(1), SortDirection::Ascending)]),
            -1 => ViewSpec::new().with_sort([(Col(1), SortDirection::Descending)]),
            _ => ViewSpec::new(),
        };
        match filter {
            Some(r) => view.with_filter(ColumnSet::first(1), move |row: &Row| row.id % 3 != r),
            None => view,
        }
    }

    #[test]
    fn test_selection_matches_reference_model() {
        const PAGE: usize = 3;
        let columns = Visible(vec![ColumnIx::new(0)]);
        let gesture = StandardSelectionGesture;
        for seed in 0..60 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let mode = if rng.bool() {
                SelectionMode::RowSet
            } else {
                SelectionMode::RowSingle
            };
            let mut world = World::new(0);
            let mut model = SelectionModel::<u32, Col>::new(mode);
            let mut reference = Reference {
                single: mode == SelectionMode::RowSingle,
                ..Reference::default()
            };
            let initial: Vec<Op<Row>> = (0..20)
                .map(|id| {
                    let price = rng.i64(0..50);
                    reference.upsert(id, price);
                    row(id, price)
                })
                .collect();
            world.apply(initial);
            let (mut sort, mut filter) = (0i8, None);
            world.rebuild(&view(sort, filter));
            reference.shown = reference.rebuild(sort, filter);

            for step in 0..40 {
                let context = format!("seed {seed}, step {step}");
                // Data: updates, inserts, removals, a removal and insert of
                // one key in one batch, and now and then a new data set.
                let mut ops = Vec::new();
                let mut removed = HashSet::new();
                let reset = rng.u8(..20) == 0;
                if reset {
                    let count = rng.u32(0..30);
                    let mut rows: Vec<Arc<Row>> = Vec::new();
                    for id in 0..count {
                        if rng.bool() {
                            rows.push(Arc::new(Row {
                                id,
                                price: rng.i64(0..50),
                            }));
                        }
                    }
                    // Every row of the old set is gone, even under a key the
                    // new set has again.
                    for key in reference.store.keys() {
                        *reference.generation.entry(*key).or_default() += 1;
                    }
                    reference.store.clear();
                    for row in &rows {
                        reference.upsert(row.id, row.price);
                    }
                    ops.push(Op::ReplaceAll(rows));
                } else {
                    for _ in 0..rng.usize(0..4) {
                        let key = rng.u32(0..40);
                        match rng.u8(..4) {
                            0 => {
                                if reference.remove(key) {
                                    removed.insert(key);
                                }
                                ops.push(Op::Remove(key));
                            }
                            1 => {
                                if reference.remove(key) {
                                    removed.insert(key);
                                }
                                ops.push(Op::Remove(key));
                                let price = rng.i64(0..50);
                                reference.upsert(key, price);
                                ops.push(row(key, price));
                            }
                            _ => {
                                let price = rng.i64(0..50);
                                reference.upsert(key, price);
                                ops.push(row(key, price));
                            }
                        }
                    }
                }
                let (previous, changes) = world.apply(ops);
                model.remove_rows(changes.removed(), &previous);
                if changes.is_reset() {
                    model.retain_present(&world.snapshot);
                    let store = reference.store.clone();
                    reference.selected.retain(|key| store.contains_key(key));
                    reference.focus = reference.focus.filter(|key| store.contains_key(key));
                    reference.anchor = reference.anchor.filter(|key| store.contains_key(key));
                } else if !changes.removed().is_empty() {
                    model.follow_removed(&world.projection, &previous, &world.snapshot);
                    // Removed in this batch, even if inserted again: the next
                    // row on screen that is still live.
                    let moved = [reference.focus, reference.anchor].map(|cell| {
                        let key = cell?;
                        if !removed.contains(&key) {
                            return Some(key);
                        }
                        Reference::successor(&reference.shown, key, |entry| {
                            reference.is_live(entry)
                        })
                    });
                    [reference.focus, reference.anchor] = moved;
                }

                // Now and then a new order: a sort, a filter, or the same view.
                // A new data set is shown at once, as the grid does: until
                // then the focus kept by key is not on screen.
                if reset || rng.u8(..3) == 0 {
                    if rng.bool() {
                        sort = rng.i8(-1..=1);
                        filter = rng.bool().then(|| rng.u32(0..3));
                    }
                    let previous = world.rebuild(&view(sort, filter));
                    model.follow_filtered(&previous, &world.projection, &world.snapshot);
                    let shown = reference.rebuild(sort, filter);
                    let old = std::mem::replace(&mut reference.shown, shown);
                    let new: HashSet<u32> = reference.shown.iter().map(|entry| entry.0).collect();
                    let moved = [reference.focus, reference.anchor].map(|cell| {
                        let key = cell?;
                        if new.contains(&key) {
                            return Some(key);
                        }
                        Reference::successor(&old, key, |entry| {
                            new.contains(&entry.0) && reference.is_live(entry)
                        })
                    });
                    [reference.focus, reference.anchor] = moved;
                }

                // A gesture on the rows on screen.
                let live = reference.live_keys();
                let rows = world.rows();
                match rng.u8(..6) {
                    0 | 1 if !live.is_empty() => {
                        let key = live[rng.usize(..live.len())];
                        let mut modifiers = Modifiers::default();
                        modifiers.shift = rng.bool();
                        modifiers.platform = rng.bool();
                        modifiers.control = modifiers.platform;
                        let target = at(key, 0);
                        let edit = gesture.edit(&SelectionGestureContext::new(
                            GestureInput::Pointer,
                            modifiers,
                            false,
                            model.is_selected(&target),
                            mode,
                        ));
                        model.apply(edit, target, rows, &columns);
                        reference.apply(edit, key);
                    }
                    2 => {
                        let navigation = [
                            Navigation::Up,
                            Navigation::Down,
                            Navigation::PageUp,
                            Navigation::PageDown,
                            Navigation::FirstRow,
                            Navigation::LastRow,
                        ][rng.usize(..6)];
                        let extending = rng.bool();
                        let target = model.navigation_target(navigation, PAGE, rows, &columns);
                        let expected = reference.navigate(navigation, PAGE);
                        assert_eq!(
                            target.as_ref().and_then(|cell| cell.row().copied()),
                            expected,
                            "{context}"
                        );
                        if let Some(target) = target {
                            let edit = gesture.edit(&SelectionGestureContext::new(
                                GestureInput::Keyboard,
                                Modifiers::default(),
                                extending,
                                model.is_selected(&target),
                                mode,
                            ));
                            let key = *target.row().expect("a row");
                            model.apply(edit, target, rows, &columns);
                            reference.apply(edit, key);
                        }
                    }
                    3 => {
                        let toggled = model.toggle_focused(rows, &columns);
                        let focus = reference
                            .focus
                            .filter(|key| reference.position(*key).is_some());
                        assert_eq!(toggled, focus.is_some(), "{context}");
                        if let Some(key) = focus {
                            let edit = if reference.single {
                                SelectionEdit::Replace
                            } else {
                                SelectionEdit::Toggle
                            };
                            reference.apply(edit, key);
                        }
                    }
                    4 => {
                        if model.select_all(rows, &columns) {
                            reference.selected = live.iter().copied().collect();
                        }
                    }
                    _ => {
                        model.clear();
                        reference.selected.clear();
                    }
                }

                // 1, 3, 4: the selection is the reference's: no key selected
                // ahead of its gesture, none back after a removal.
                let selected: HashSet<u32> = model.selection().iter().copied().collect();
                assert_eq!(selected, reference.selected, "{context}");
                // 2: the scope is the selection within the screen, in order.
                let scoped = ProjectionScope.resolve(&SelectionScopeContext::new(
                    model.selection(),
                    &world.projection,
                    &world.snapshot,
                    mode,
                ));
                let expected: Vec<u32> = reference
                    .live_keys()
                    .into_iter()
                    .filter(|key| reference.selected.contains(key))
                    .collect();
                assert_eq!(scoped.keys(), expected, "{context}");
                // 5, 6: the focus is the reference's, on a live row on screen.
                let focus = model.focus().and_then(|cell| cell.row().copied());
                assert_eq!(focus, reference.focus, "{context}");
                if let Some(key) = focus {
                    assert!(world.rows().display_ix(&key).is_some(), "{context}");
                }
                assert_eq!(
                    model.anchor.as_ref().and_then(|cell| cell.row().copied()),
                    reference.anchor,
                    "{context}"
                );
            }
        }
    }
}
