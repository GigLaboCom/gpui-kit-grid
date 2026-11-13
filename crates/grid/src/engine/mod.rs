//! The projection engine: filter and sort a snapshot into the display order.
//! No GPUI types.
//!
//! Each part of the engine has its own file: the shown lines and their
//! readers (`display`), grouping (`group`), aggregates (`aggregate`),
//! expansion (`expand`) and the tree (`tree`). This file keeps the view,
//! the projection and the flat path.

mod aggregate;
mod display;
mod expand;
mod group;
mod tree;

use std::{
    cmp::Ordering,
    panic,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    },
    thread,
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

pub use self::{
    aggregate::{Aggregate, AggregateFn},
    display::{
        AggregateRow, DisplayRow, ExpandDepth, Expansion, GroupId, GroupInfo, GroupPath, GroupTable,
    },
    group::{
        AggregateOrder, EveryGroupFooter, GroupBy, GroupFooter, GroupKey, GroupOrder,
        GroupOrderContext, KeyOrder, NoGroupFooter,
    },
    tree::{
        Branch, TreeFilter, TreeFilterContext, TreeFilterRule, TreeSpec, TreeTotals,
        TreeTotalsContext, TreeTotalsRule,
    },
};
use self::{
    display::{AggregateValues, Lines, SummaryStamps},
    group::GroupSpec,
};
use crate::{
    model::{CellValue, ColumnSet, GridColumns, GridRow},
    quick_filter::{QuickFilter, QuickFilterPass},
    store::{RowId, StoreSnapshot},
};

/// Sort direction of a column. Serialized as `"ascending"` or
/// `"descending"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    /// Smallest first.
    Ascending,
    /// Largest first.
    Descending,
}

type Filter<R> = Arc<dyn Fn(&R) -> bool + Send + Sync>;

/// What the view shows: a filter, a quick filter and a multi-column sort,
/// a grouping or a tree, aggregates and the expansion.
///
/// Cloning is cheap: every part is shared or small.
pub struct ViewSpec<R: GridRow> {
    sort: Vec<(R::Column, SortDirection)>,
    filter: Option<(ColumnSet, Filter<R>)>,
    quick_filter: Option<QuickFilter>,
    /// Flat, grouped or a tree; the last of `with_group_by` and
    /// `with_tree` wins.
    structure: Structure<R>,
    /// The order of sibling groups; `None` is [`KeyOrder`].
    group_order: Option<Arc<dyn GroupOrder<R>>>,
    /// Which groups have a footer; `None` is [`NoGroupFooter`].
    group_footer: Option<Arc<dyn GroupFooter>>,
    /// Aggregates by column; `None` has none. They outlive a change of the
    /// structure, as the group order and footer do.
    aggregates: Option<Arc<[(R::Column, Aggregate<R>)]>>,
    /// Set by the grid, which owns it like the quick filter.
    expansion: Expansion<R::Key>,
}

/// How a view arranges its rows.
pub(crate) enum Structure<R: GridRow> {
    /// A flat list.
    Flat,
    /// Groups by these levels.
    Groups(GroupSpec<R>),
    /// A tree by `GridRow::parent_key`.
    Tree(TreeSpec<R>),
}

impl<R: GridRow> Clone for Structure<R> {
    fn clone(&self) -> Self {
        match self {
            Self::Flat => Self::Flat,
            Self::Groups(groups) => Self::Groups(groups.clone()),
            Self::Tree(tree) => Self::Tree(tree.clone()),
        }
    }
}

impl<R: GridRow> Clone for ViewSpec<R> {
    fn clone(&self) -> Self {
        Self {
            sort: self.sort.clone(),
            filter: self.filter.clone(),
            quick_filter: self.quick_filter.clone(),
            structure: self.structure.clone(),
            group_order: self.group_order.clone(),
            group_footer: self.group_footer.clone(),
            aggregates: self.aggregates.clone(),
            expansion: self.expansion.clone(),
        }
    }
}

impl<R: GridRow> Default for ViewSpec<R> {
    fn default() -> Self {
        Self {
            sort: Vec::new(),
            filter: None,
            quick_filter: None,
            structure: Structure::Flat,
            group_order: None,
            group_footer: None,
            aggregates: None,
            expansion: Expansion::default(),
        }
    }
}

impl<R: GridRow> ViewSpec<R> {
    /// No filter, insertion order.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sorts by these columns, first one most significant. Ties keep
    /// insertion order, so the order is stable.
    pub fn with_sort(mut self, sort: impl IntoIterator<Item = (R::Column, SortDirection)>) -> Self {
        self.sort = sort.into_iter().collect();
        self
    }

    /// Shows only rows for which `filter` returns true. `depends_on` lists
    /// the columns the filter reads; pass every column when unsure. A
    /// rebuild of a large snapshot calls `filter` from several threads at
    /// once.
    pub fn with_filter(
        mut self,
        depends_on: ColumnSet,
        filter: impl Fn(&R) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.filter = Some((depends_on, Arc::new(filter)));
        self
    }

    /// Shows only rows that also pass the quick text search; `None` turns
    /// it off. A row is shown when it passes both the filter and the
    /// quick filter.
    pub fn with_quick_filter(mut self, filter: Option<QuickFilter>) -> Self {
        self.quick_filter = filter;
        self
    }

    /// The sort columns.
    pub fn sort(&self) -> &[(R::Column, SortDirection)] {
        &self.sort
    }

    /// The quick filter, if it is on.
    pub fn quick_filter(&self) -> Option<&QuickFilter> {
        self.quick_filter.as_ref()
    }

    /// Columns whose changes can move rows: the sort columns, the columns
    /// both filters read and, in a grouped view, the grouping columns, the
    /// columns `GroupKey`s read and those the group order reads (for an
    /// order by an aggregate, what that aggregate reads). Inserts and
    /// removals always do.
    ///
    /// A change of the other aggregated columns
    /// ([`Self::aggregate_columns`]) changes values alone: the aggregate job
    /// ([`ProjectionEngine::aggregate`]) refreshes them in the same order.
    pub fn dependencies(&self) -> ColumnSet {
        let mut columns = ColumnSet::EMPTY;
        for (column, _) in &self.sort {
            columns.insert(column.ix());
        }
        if let Some((depends_on, _)) = &self.filter {
            columns.union_with(*depends_on);
        }
        if let Some(quick_filter) = &self.quick_filter {
            columns.union_with(quick_filter.columns());
        }
        if let Structure::Groups(groups) = &self.structure {
            for level in groups.levels() {
                match (level.as_column(), level.as_key()) {
                    (Some(column), _) => columns.insert(column.ix()),
                    (None, Some(key)) => columns.union_with(key.depends_on()),
                    (None, None) => {}
                }
            }
            // An order by an aggregate reads what that aggregate reads.
            let order_columns = self.group_order().depends_on();
            columns.union_with(order_columns);
            for (column, aggregate) in self.aggregates() {
                if order_columns.contains(column.ix()) {
                    columns.union_with(aggregate.depends_on(*column));
                }
            }
        }
        columns
    }

    /// Flat, grouped or a tree.
    pub(crate) fn structure(&self) -> &Structure<R> {
        &self.structure
    }
}

/// The display order of one view at one store version.
///
/// A projection has two orders. [`Self::rows`] lists every data row that
/// passes the view, as if everything were expanded. The lines shown
/// ([`Self::line`], [`Self::len`]) are what the body paints: in a flat view
/// the same rows; with grouping also group captions and footers, without
/// the rows of collapsed groups; in a tree, the rows whose ancestors are
/// all expanded. Display positions index the lines.
#[derive(Debug, Default)]
pub struct Projection {
    store_version: u64,
    /// Every data row that passes the view, in order as if all were
    /// expanded; the rows of a group follow each other.
    rows: Vec<RowId>,
    /// The place of each row in `rows`, by slot; `u32::MAX` for none. In a
    /// flat projection it is the display position too: a flat projection
    /// keeps no second list.
    row_to_order: Vec<u32>,
    /// The places of the rows whose slot an earlier row of `rows` holds in
    /// `row_to_order`: a fading row shares its slot with the new row that
    /// reused it. Empty otherwise.
    shared_order: FxHashMap<RowId, u32>,
    /// Live rows of `rows` when built: no fading rows.
    row_count: usize,
    /// The shown lines of a grouped or tree projection.
    lines: Lines,
    /// The aggregates of the whole view.
    totals: AggregateValues,
    /// Change stamps of the aggregates and captions while any is hot;
    /// `None` for none.
    summary_stamps: Option<Box<SummaryStamps>>,
}

impl Projection {
    /// A flat projection of `rows`, in display order, all of them live
    /// until [`Self::with_row_count`] says otherwise.
    fn new<R: GridRow>(snapshot: &StoreSnapshot<R>, rows: Vec<RowId>) -> Self {
        let (row_to_order, shared_order) =
            places(rows.iter().copied().enumerate(), snapshot.capacity());
        Self {
            store_version: snapshot.version(),
            row_count: rows.len(),
            rows,
            row_to_order,
            shared_order,
            lines: Lines::Flat,
            totals: AggregateValues::default(),
            summary_stamps: None,
        }
    }

    /// A copy for a job that derives a new projection from this one; there
    /// is no public `Clone`.
    pub(crate) fn duplicate(&self) -> Self {
        Self {
            store_version: self.store_version,
            rows: self.rows.clone(),
            row_to_order: self.row_to_order.clone(),
            shared_order: self.shared_order.clone(),
            row_count: self.row_count,
            lines: self.lines.clone(),
            totals: self.totals.clone(),
            summary_stamps: self.summary_stamps.clone(),
        }
    }

    /// The store version the projection was built from.
    pub fn store_version(&self) -> u64 {
        self.store_version
    }

    /// Every data row that passes the view, in order as if every group and
    /// tree row were expanded: the rows of a group follow each other. In a
    /// flat view these are the displayed rows.
    pub fn rows(&self) -> &[RowId] {
        &self.rows
    }

    /// Returns true if the row passes the view, displayed or not: a row
    /// of a collapsed group passes too, so commands may act on it when it
    /// is selected.
    pub fn contains(&self, id: RowId) -> bool {
        self.order_ix(id).is_some()
    }
}

/// Builds projections. The default is [`Rebuild`]; incremental engines
/// implement the same trait.
pub trait ProjectionEngine<R: GridRow>: Send + Sync + 'static {
    /// Builds the display order of `snapshot` for `spec`.
    fn rebuild(&self, snapshot: &StoreSnapshot<R>, spec: &ViewSpec<R>) -> Projection;

    /// Drops rows removed since `previous` was built. Removing rows never
    /// reorders the rest and never makes a filtered-out row visible, so this
    /// is an O(n) filter, with no sorting. With groups it also recounts
    /// them, drops the emptied ones and lays the lines out again with each
    /// group's expansion; the aggregate job ([`Self::aggregate`]) refreshes
    /// the aggregates.
    fn compact(&self, previous: &Projection, snapshot: &StoreSnapshot<R>) -> Projection {
        if previous.is_tree() {
            tree::compact(previous, snapshot)
        } else {
            display::compact(previous, snapshot)
        }
    }

    /// The order of `previous` with fresh aggregates and row counts from
    /// `snapshot`. Neither reorders nor drops rows: [`Self::compact`] drops
    /// removed ones. Groups combine their subgroups in key order, as a
    /// rebuild does, so over the same order the result is the rebuild's.
    fn aggregate(
        &self,
        previous: &Projection,
        snapshot: &StoreSnapshot<R>,
        spec: &ViewSpec<R>,
    ) -> Projection {
        if previous.is_tree() {
            tree::refresh(previous, snapshot, spec)
        } else {
            aggregate::refresh(previous, snapshot, spec)
        }
    }

    /// The order of `previous` laid out for `spec.expansion()`. The
    /// snapshot resolves the keys of tree rows: a tree is expanded by key.
    fn expand(
        &self,
        previous: &Projection,
        snapshot: &StoreSnapshot<R>,
        spec: &ViewSpec<R>,
    ) -> Projection {
        if previous.is_tree() {
            tree::relayout(previous, snapshot, spec)
        } else {
            expand::relayout(previous, snapshot, spec)
        }
    }
}

/// Full filter, quick filter and sort of the whole snapshot.
///
/// A large snapshot is split across threads: each thread takes rows in
/// chunks, keeps the visible ones and sorts them, and the sorted runs are
/// merged. Every row has its own insertion sequence, the last sort key, so
/// the order is the same on any number of threads. By default a rebuild
/// leaves two cores to the UI thread and ingest and uses at most eight
/// ([`Self::with_max_threads`] changes that). On wasm and for small
/// snapshots it runs on the calling thread alone.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rebuild {
    /// Set by [`Self::with_max_threads`]; `None`: [`default_thread_budget`].
    max_threads: Option<usize>,
}

impl Rebuild {
    /// Splits a rebuild across at most `threads` threads, the calling one
    /// included: `1` rebuilds on the calling thread alone, and `0` counts as
    /// `1`. Each thread still takes at least 8192 rows, so a small snapshot
    /// stays on one thread whatever the count. The count is used as given:
    /// neither the cores of the machine nor the default's limit of eight cap
    /// it. On wasm a rebuild always runs on the calling thread.
    ///
    /// The default is the available cores less two (for the UI thread and
    /// ingest), at least one and at most eight.
    pub fn with_max_threads(mut self, threads: usize) -> Self {
        self.max_threads = Some(threads.max(1));
        self
    }

    /// The most threads one rebuild uses, the calling one included: the
    /// count given to [`Self::with_max_threads`], or the default budget.
    /// Always `1` on wasm.
    pub fn max_threads(&self) -> usize {
        if cfg!(target_family = "wasm") {
            // Threads may not exist on wasm.
            1
        } else {
            self.max_threads.unwrap_or_else(default_thread_budget)
        }
    }

    /// The flat path: filter and sort into one flat list, fading rows in
    /// their places. A view with aggregates totals the live rows in one
    /// more pass, in the order of the rows.
    fn rebuild_flat<R: GridRow>(
        &self,
        snapshot: &StoreSnapshot<R>,
        spec: &ViewSpec<R>,
    ) -> Projection {
        let parallelism = Parallelism::for_view(snapshot.len(), spec, self.max_threads());
        let (rows, fading) = project(snapshot, spec, parallelism);
        let live = rows.len() - fading;
        let projection = Projection::new(snapshot, rows).with_row_count(live);
        let aggregates = aggregate::ViewAggregates::new(spec);
        if aggregates.is_empty() {
            return projection;
        }
        let (totals, _) = aggregate::flat_totals(
            &aggregates,
            projection
                .rows
                .iter()
                .filter_map(|id| snapshot.row(*id).map(|row| &**row)),
        );
        projection.with_totals(AggregateValues::new(aggregates.columns(), totals))
    }
}

fn directed(ordering: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Ascending => ordering,
        SortDirection::Descending => ordering.reverse(),
    }
}

impl<R: GridRow> ProjectionEngine<R> for Rebuild {
    fn rebuild(&self, snapshot: &StoreSnapshot<R>, spec: &ViewSpec<R>) -> Projection {
        match spec.structure() {
            Structure::Flat => self.rebuild_flat(snapshot, spec),
            Structure::Groups(groups) => group::rebuild(self, snapshot, spec, groups),
            Structure::Tree(tree) => tree::rebuild(self, snapshot, spec, tree),
        }
    }
}

/// Rows below which another thread does not pay for itself.
const MIN_ROWS_PER_THREAD: usize = 8 * 1024;
/// Rows a thread takes at a time: small enough to even out slower cores,
/// large enough that the shared counter is not contended.
const CHUNK_ROWS: usize = 1024;
/// Cores the default budget leaves to the UI thread and to ingest.
const RESERVED_CORES: usize = 2;
/// The default budget's limit: more threads gain little, as the merge grows
/// with their count. An explicit [`Rebuild::with_max_threads`] may exceed it.
const MAX_THREADS: usize = 8;

/// How one rebuild splits its work.
#[derive(Clone, Copy, Debug)]
struct Parallelism {
    /// Threads, the calling one included.
    threads: usize,
    /// Rows a thread takes from the snapshot at a time.
    chunk_rows: usize,
}

impl Parallelism {
    /// A thread per [`MIN_ROWS_PER_THREAD`] rows, at most `budget`. In
    /// insertion order with no filter there is nothing worth splitting: the
    /// rows are only listed.
    fn for_view<R: GridRow>(rows: usize, spec: &ViewSpec<R>, budget: usize) -> Self {
        let has_work =
            !spec.sort.is_empty() || spec.filter.is_some() || spec.quick_filter.is_some();
        if has_work {
            Self::for_rows(rows, budget)
        } else {
            Self {
                threads: 1,
                chunk_rows: CHUNK_ROWS,
            }
        }
    }

    /// A thread per [`MIN_ROWS_PER_THREAD`] rows, at most `budget`, for a
    /// sort that always has keys, such as a grouped one.
    fn for_rows(rows: usize, budget: usize) -> Self {
        Self {
            threads: budget.min(rows / MIN_ROWS_PER_THREAD).max(1),
            chunk_rows: CHUNK_ROWS,
        }
    }
}

/// The threads one rebuild uses by default: the available parallelism less
/// [`RESERVED_CORES`], at least one and at most [`MAX_THREADS`].
fn default_thread_budget() -> usize {
    static BUDGET: OnceLock<usize> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        thread::available_parallelism().map_or(1, |cores| {
            cores
                .get()
                .saturating_sub(RESERVED_CORES)
                .clamp(1, MAX_THREADS)
        })
    })
}

/// The display order of `snapshot` for `spec`, with the number of fading
/// rows in it.
fn project<R: GridRow>(
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    parallelism: Parallelism,
) -> (Vec<RowId>, usize) {
    match spec.sort.split_first() {
        // Storage order is not insertion order once slots are reused.
        None => sorted_rows(
            snapshot,
            spec,
            parallelism,
            |id, _, seq| (seq, id),
            |a, b| a.0.cmp(&b.0),
            |&(_, id)| id,
        ),
        // Extract the most significant key once per row; later keys are
        // read only to break ties.
        Some((&(first, first_direction), rest)) => sorted_rows(
            snapshot,
            spec,
            parallelism,
            |id, row, seq| (row.value(first), seq, id, row),
            move |a, b| compare_sorted(a, b, first_direction, rest),
            |&(_, _, id, _)| id,
        ),
    }
}

/// A sorted row: its most significant key, insertion sequence, id and row.
type SortEntry<'a, R> = (CellValue<'a>, u64, RowId, &'a Arc<R>);

/// Compares by the sort keys, then by insertion. Called for nearly every
/// pair of rows by the sort and the merges; without `inline(always)` it is
/// not inlined there, which costs a tenth of a text sort.
#[inline(always)]
fn compare_sorted<R: GridRow>(
    a: &SortEntry<'_, R>,
    b: &SortEntry<'_, R>,
    first_direction: SortDirection,
    rest: &[(R::Column, SortDirection)],
) -> Ordering {
    directed(a.0.cmp_total(&b.0), first_direction)
        .then_with(|| {
            rest.iter()
                .map(|&(column, direction)| {
                    directed(a.3.value(column).cmp_total(&b.3.value(column)), direction)
                })
                .find(|ordering| ordering.is_ne())
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| a.1.cmp(&b.1))
}

/// The visible rows of `snapshot` and its visible fading rows, ordered by
/// their entries and mapped by `row_id`, with the number of fading rows
/// among them. `entry` takes a row's insertion sequence: a fading row keeps
/// its place by its last values and its sequence. `compare` is a total
/// order under which no two rows are equal (the insertion sequence breaks
/// ties), so the result does not depend on how the work is split.
fn sorted_rows<'a, R: GridRow, E: Copy + Send + Sync, T>(
    snapshot: &'a StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    parallelism: Parallelism,
    entry: impl Fn(RowId, &'a Arc<R>, u64) -> E + Sync,
    compare: impl Fn(&E, &E) -> Ordering + Sync,
    row_id: impl Fn(&E) -> T,
) -> (Vec<T>, usize) {
    let mut fading = fading_entries(snapshot, spec, &entry);
    let fading_count = fading.len();
    if parallelism.threads <= 1 {
        let mut visible = VisibleRows::new(spec);
        let mut entries: Vec<E> = snapshot
            .iter()
            .filter(|(_, row)| visible.contains(row))
            .map(|(id, row)| entry(id, row, snapshot.seq(id)))
            .collect();
        entries.append(&mut fading);
        entries.sort_unstable_by(|a, b| compare(a, b));
        let rows = entries.into_iter().map(|entry| row_id(&entry)).collect();
        return (rows, fading_count);
    }

    let rows: Vec<(RowId, &Arc<R>)> = snapshot.iter().collect();
    let next_chunk = AtomicUsize::new(0);
    // The first thread done with the chunks sorts the fading rows into its
    // run: one more run would cost one more merge of every row.
    let fading_taken = AtomicBool::new(fading.is_empty());
    // One thread's share: chunks taken until none is left, so a slower
    // thread takes fewer. One set of quick filter buffers per thread.
    let sorted_run = || {
        let mut visible = VisibleRows::new(spec);
        let mut entries = Vec::new();
        loop {
            let start = next_chunk.fetch_add(parallelism.chunk_rows, AtomicOrdering::Relaxed);
            if start >= rows.len() {
                break;
            }
            let chunk = &rows[start..rows.len().min(start + parallelism.chunk_rows)];
            entries.extend(
                chunk
                    .iter()
                    .filter(|(_, row)| visible.contains(row))
                    .map(|&(id, row)| entry(id, row, snapshot.seq(id))),
            );
        }
        if !fading_taken.swap(true, AtomicOrdering::Relaxed) {
            entries.extend_from_slice(&fading);
        }
        entries.sort_unstable_by(|a, b| compare(a, b));
        entries
    };
    let mut runs = run_on_threads(&vec![&sorted_run; parallelism.threads]);
    runs.retain(|run| !run.is_empty());

    // Neighbouring runs merge in pairs, a thread per pair, until two are
    // left for the calling thread.
    while runs.len() > 2 {
        let merges: Vec<_> = runs
            .chunks(2)
            .map(|pair| {
                let compare = &compare;
                move || match pair {
                    [first, second] => {
                        let mut merged = Vec::with_capacity(first.len() + second.len());
                        merge(first, second, compare, |entry| *entry, &mut merged);
                        merged
                    }
                    _ => pair.concat(),
                }
            })
            .collect();
        runs = run_on_threads(&merges);
    }
    let mut rows = Vec::with_capacity(runs.iter().map(Vec::len).sum());
    match runs.as_slice() {
        [first, second] => merge(first, second, &compare, row_id, &mut rows),
        runs => rows.extend(runs.iter().flatten().map(row_id)),
    }
    (rows, fading_count)
}

/// The entries of the fading rows of `snapshot` that pass the view's
/// filters, by their last versions and insertion sequences. At most the
/// removals of one fade: listed on the calling thread.
fn fading_entries<'a, R: GridRow, E>(
    snapshot: &'a StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    entry: &impl Fn(RowId, &'a Arc<R>, u64) -> E,
) -> Vec<E> {
    let mut fading = snapshot.fading_rows().peekable();
    if fading.peek().is_none() {
        return Vec::new();
    }
    let mut visible = VisibleRows::new(spec);
    fading
        .filter(|(_, row, ..)| visible.contains(row))
        .map(|(id, row, _, seq)| entry(id, row, seq))
        .collect()
}

/// The place of each row by slot (`u32::MAX` for none) from `(place, id)`
/// pairs, and the places of the rows whose slot an earlier pair took, by id:
/// a fading row shares its slot with the new row that reused it.
fn places(
    rows: impl Iterator<Item = (usize, RowId)>,
    capacity: usize,
) -> (Vec<u32>, FxHashMap<RowId, u32>) {
    let mut by_slot = vec![u32::MAX; capacity];
    let mut shared = FxHashMap::default();
    for (place, id) in rows {
        match by_slot.get_mut(id.ix()) {
            Some(slot) if *slot == u32::MAX => *slot = place as u32,
            _ => {
                shared.insert(id, place as u32);
            }
        }
    }
    (by_slot, shared)
}

/// The place of a row whose slot another row holds: see [`places`].
#[inline]
fn shared_place(shared: &FxHashMap<RowId, u32>, id: RowId) -> Option<usize> {
    if shared.is_empty() {
        None
    } else {
        shared.get(&id).map(|&place| place as usize)
    }
}

/// The fading rows of a snapshot, as a rebuild tells them from live rows: a
/// bit per slot first, then the id, as a slot may hold both a fading row
/// and the new row that reused it. Without fading rows it holds nothing and
/// costs a bounds check.
#[derive(Default)]
struct FadingIds {
    slots: Vec<u64>,
    ids: FxHashSet<RowId>,
}

impl FadingIds {
    fn new<R: GridRow>(snapshot: &StoreSnapshot<R>) -> Self {
        let mut fading = Self::default();
        for (id, ..) in snapshot.fading_rows() {
            if fading.slots.is_empty() {
                fading.slots = vec![0; snapshot.capacity().div_ceil(64)];
            }
            if let Some(word) = fading.slots.get_mut(id.ix() / 64) {
                *word |= 1 << (id.ix() % 64);
            }
            fading.ids.insert(id);
        }
        fading
    }

    /// Returns true if the row is a fading one.
    #[inline]
    fn contains(&self, id: RowId) -> bool {
        self.slots
            .get(id.ix() / 64)
            .is_some_and(|word| word & (1 << (id.ix() % 64)) != 0)
            && self.ids.contains(&id)
    }

    /// Returns true if the snapshot has no fading rows.
    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Appends the entries of two sorted runs to `out` in order, mapped by
/// `map`.
fn merge<E, T>(
    first: &[E],
    second: &[E],
    compare: impl Fn(&E, &E) -> Ordering,
    map: impl Fn(&E) -> T,
    out: &mut Vec<T>,
) {
    let (mut first_ix, mut second_ix) = (0, 0);
    while let (Some(a), Some(b)) = (first.get(first_ix), second.get(second_ix)) {
        if compare(b, a).is_lt() {
            out.push(map(b));
            second_ix += 1;
        } else {
            out.push(map(a));
            first_ix += 1;
        }
    }
    out.extend(first[first_ix..].iter().map(&map));
    out.extend(second[second_ix..].iter().map(&map));
}

/// Runs every job and returns the results in order: the first on the
/// calling thread, each other on a scoped thread of its own, or on the
/// calling thread when that thread fails to start. A panic in a job
/// resumes on the calling thread.
fn run_on_threads<T: Send>(jobs: &[impl FnOnce() -> T + Copy + Send]) -> Vec<T> {
    let Some((&first, rest)) = jobs.split_first() else {
        return Vec::new();
    };
    thread::scope(|scope| {
        let started: Vec<_> = rest
            .iter()
            .map(|&job| {
                thread::Builder::new()
                    .name("live-grid-rebuild".into())
                    .spawn_scoped(scope, job)
                    .map_err(|_| job)
            })
            .collect();
        let mut results = Vec::with_capacity(jobs.len());
        results.push(first());
        for job in started {
            results.push(match job {
                Ok(thread) => thread
                    .join()
                    .unwrap_or_else(|panic| panic::resume_unwind(panic)),
                Err(job) => job(),
            });
        }
        results
    })
}

/// The view's filter, then its quick filter, row by row. Holds the quick
/// filter's buffers for one thread.
struct VisibleRows<'a, R: GridRow> {
    filter: Option<&'a Filter<R>>,
    quick_filter: Option<QuickFilterPass<'a, R>>,
}

impl<'a, R: GridRow> VisibleRows<'a, R> {
    fn new(spec: &'a ViewSpec<R>) -> Self {
        Self {
            filter: spec.filter.as_ref().map(|(_, filter)| filter),
            // Off, it costs a branch per row.
            quick_filter: spec.quick_filter.as_ref().map(QuickFilterPass::new),
        }
    }

    fn contains(&mut self, row: &'a R) -> bool {
        self.filter.is_none_or(|filter| filter(row))
            && self
                .quick_filter
                .as_mut()
                .is_none_or(|quick_filter| quick_filter.matches(row))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::{
        highlight::HighlightPolicy,
        model::{ColumnId, ColumnIx, OwnedValue},
        quick_filter::ContainsAllTerms,
        store::{Ingest, Op},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Column {
        Id,
        Name,
        Price,
    }

    impl GridColumns for Column {
        fn all() -> &'static [Self] {
            &[Self::Id, Self::Name, Self::Price]
        }

        fn id(self) -> &'static str {
            match self {
                Self::Id => "id",
                Self::Name => "name",
                Self::Price => "price",
            }
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self as u16)
        }
    }

    struct Row;

    impl GridRow for Row {
        type Key = u32;
        type Column = Column;

        fn key(&self) -> &u32 {
            &0
        }

        fn value(&self, _column: Column) -> CellValue<'_> {
            CellValue::Empty
        }
    }

    #[test]
    fn test_quick_filter_dependencies() {
        let mut name = ColumnSet::EMPTY;
        name.insert(Column::Name.ix());
        let quick_filter = QuickFilter::new("sber", name, &ContainsAllTerms);
        let spec = ViewSpec::<Row>::new()
            .with_sort([(Column::Price, SortDirection::Ascending)])
            .with_quick_filter(quick_filter);
        assert_eq!(spec.quick_filter().map(QuickFilter::text), Some("sber"));
        let dependencies = spec.dependencies();
        assert!(dependencies.contains(Column::Name.ix()));
        assert!(dependencies.contains(Column::Price.ix()));
        assert!(!dependencies.contains(Column::Id.ix()));

        // Turned off, the quick filter columns no longer rebuild the view.
        let off = spec.with_quick_filter(None);
        assert!(off.quick_filter().is_none());
        assert!(!off.dependencies().contains(Column::Name.ix()));
        assert!(off.dependencies().contains(Column::Price.ix()));
    }

    #[test]
    fn test_sort_direction_serde() {
        // Saved layouts depend on these names.
        for (direction, json) in [
            (SortDirection::Ascending, r#""ascending""#),
            (SortDirection::Descending, r#""descending""#),
        ] {
            assert_eq!(serde_json::to_string(&direction).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SortDirection>(json).unwrap(),
                direction
            );
        }
        assert!(serde_json::from_str::<SortDirection>(r#""Ascending""#).is_err());
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum TradeColumn {
        Id,
        Group,
        Price,
        Name,
        Flag,
        Mixed,
    }

    impl GridColumns for TradeColumn {
        fn all() -> &'static [Self] {
            &[
                Self::Id,
                Self::Group,
                Self::Price,
                Self::Name,
                Self::Flag,
                Self::Mixed,
            ]
        }

        fn id(self) -> &'static str {
            match self {
                Self::Id => "id",
                Self::Group => "group",
                Self::Price => "price",
                Self::Name => "name",
                Self::Flag => "flag",
                Self::Mixed => "mixed",
            }
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self as u16)
        }
    }

    /// Few distinct values per column, so sort keys tie often and only the
    /// insertion sequence tells rows apart.
    struct Trade {
        id: u32,
        group: i64,
        price: f64,
        name: String,
        flag: Option<bool>,
        /// A column of every type, for the order across types.
        mixed: u8,
    }

    impl Trade {
        fn random(rng: &mut fastrand::Rng, id: u32) -> Self {
            const NAME_CHARS: &[char] = &['a', 'B', 'я', 'Ж', '1'];
            Self {
                id,
                group: rng.i64(0..4),
                price: [0.5, 1.0, 1.5, f64::NAN, -0.0][rng.usize(..5)],
                name: (0..rng.usize(0..=2))
                    .map(|_| NAME_CHARS[rng.usize(..NAME_CHARS.len())])
                    .collect(),
                flag: [None, Some(false), Some(true)][rng.usize(..3)],
                mixed: rng.u8(0..8),
            }
        }
    }

    impl GridRow for Trade {
        type Key = u32;
        type Column = TradeColumn;

        fn key(&self) -> &u32 {
            &self.id
        }

        fn value(&self, column: TradeColumn) -> CellValue<'_> {
            match column {
                TradeColumn::Id => CellValue::Int(self.id.into()),
                TradeColumn::Group => CellValue::Int(self.group),
                TradeColumn::Price => CellValue::Float(self.price),
                TradeColumn::Name => CellValue::Text(&self.name),
                TradeColumn::Flag => self.flag.map_or(CellValue::Empty, CellValue::Bool),
                TradeColumn::Mixed => match self.mixed {
                    0 => CellValue::Empty,
                    1 => CellValue::Bool(true),
                    2 | 3 => CellValue::Int(i64::from(self.mixed)),
                    4 | 5 => CellValue::Float(f64::from(self.mixed) / 2.0),
                    _ => CellValue::Text(if self.mixed == 6 { "x" } else { "Y" }),
                },
            }
        }
    }

    fn random_spec(rng: &mut fastrand::Rng) -> ViewSpec<Trade> {
        let columns = TradeColumn::all();
        let direction = |rng: &mut fastrand::Rng| {
            [SortDirection::Ascending, SortDirection::Descending][rng.usize(..2)]
        };
        let sort: Vec<_> = (0..rng.usize(0..=3))
            .map(|_| (columns[rng.usize(..columns.len())], direction(rng)))
            .collect();
        let mut quick_columns = ColumnSet::EMPTY;
        for column in columns {
            if rng.bool() {
                quick_columns.insert(column.ix());
            }
        }
        let text = ["", "a", "я 1", "b ж", ".5", "1 a"][rng.usize(..6)];
        let spec = ViewSpec::new()
            .with_sort(sort)
            .with_quick_filter(QuickFilter::new(text, quick_columns, &ContainsAllTerms));
        if rng.bool() {
            spec.with_filter(ColumnSet::first(columns.len()), |trade: &Trade| {
                trade.group != 1
            })
        } else {
            spec
        }
    }

    /// The store's highlight policy: removed rows fade with `fade`, longer
    /// than any test.
    fn policy(fade: bool) -> HighlightPolicy {
        if fade {
            HighlightPolicy::default().with_removal_fade(Duration::from_secs(3_600))
        } else {
            HighlightPolicy::default()
        }
    }

    #[test]
    fn test_parallel_rebuild_matches_sequential() {
        // Small chunks and odd thread counts put chunk and run boundaries
        // everywhere: the order must not depend on them. Removed rows fade
        // in odd seeds: one more sorted run.
        let epoch = Instant::now();
        let mut fading = 0;
        for seed in 0..48 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let mut ingest = Ingest::<Trade>::new(policy(seed % 2 == 1), epoch);
            let count = [0, 1, 2, 7, 100, 1_000, 3_000][rng.usize(..7)];
            ingest.apply(
                (0..count).map(|id| Op::UpsertSilent(Arc::new(Trade::random(&mut rng, id)))),
                epoch,
            );
            // Removed slots are reused, so storage order differs from
            // insertion order.
            ingest.apply(
                (0..count / 3).map(|_| Op::Remove(rng.u32(..count.max(1)))),
                epoch,
            );
            ingest.apply(
                (count..count + count / 4)
                    .map(|id| Op::UpsertSilent(Arc::new(Trade::random(&mut rng, id)))),
                epoch,
            );
            let snapshot = ingest.snapshot();
            for _ in 0..8 {
                let spec = random_spec(&mut rng);
                let sequential = project(
                    &snapshot,
                    &spec,
                    Parallelism {
                        threads: 1,
                        chunk_rows: CHUNK_ROWS,
                    },
                );
                fading += sequential.1;
                let parallelism = Parallelism {
                    threads: rng.usize(2..=7),
                    chunk_rows: rng.usize(1..=97),
                };
                assert_eq!(
                    project(&snapshot, &spec, parallelism),
                    sequential,
                    "seed {seed}, {count} rows, {parallelism:?}, sort {:?}, quick filter {:?}",
                    spec.sort(),
                    spec.quick_filter().map(QuickFilter::text),
                );
            }
        }
        assert!(fading > 1_000, "{fading} fading rows");
    }

    #[test]
    fn test_parallelism_for_view() {
        let sorted =
            ViewSpec::<Trade>::new().with_sort([(TradeColumn::Name, SortDirection::Ascending)]);
        let budget = Rebuild::default().max_threads();
        // Small snapshots and plain listing stay on the calling thread.
        assert_eq!(
            Parallelism::for_view(MIN_ROWS_PER_THREAD - 1, &sorted, budget).threads,
            1
        );
        assert_eq!(
            Parallelism::for_view(1 << 20, &ViewSpec::<Trade>::new(), budget).threads,
            1
        );
        // Large ones use the default budget, which leaves cores to the UI
        // and ingest.
        let threads = Parallelism::for_view(1 << 20, &sorted, budget).threads;
        assert_eq!(threads, default_thread_budget());
        assert!((1..=MAX_THREADS).contains(&threads));
        if let Ok(cores) = thread::available_parallelism() {
            assert!(threads == 1 || threads + RESERVED_CORES <= cores.get());
        }
        // Rows still limit an explicit count.
        for (rows, threads) in [
            (MIN_ROWS_PER_THREAD * 3 - 1, 2),
            (MIN_ROWS_PER_THREAD * 3, 3),
            (MIN_ROWS_PER_THREAD * 40, 12),
        ] {
            assert_eq!(
                Parallelism::for_view(rows, &sorted, 12).threads,
                threads,
                "{rows} rows"
            );
        }
    }

    #[test]
    fn test_rebuild_max_threads() {
        assert_eq!(Rebuild::default().max_threads(), default_thread_budget());
        assert_eq!(Rebuild::default().with_max_threads(1).max_threads(), 1);
        assert_eq!(
            Rebuild::default().with_max_threads(0).max_threads(),
            1,
            "at least one"
        );
        // The default's limit does not cap an explicit count.
        assert_eq!(
            Rebuild::default()
                .with_max_threads(MAX_THREADS * 2)
                .max_threads(),
            MAX_THREADS * 2
        );
    }

    #[test]
    fn test_parallel_rebuild_resumes_panic() {
        // A panicking application filter fails the rebuild on the calling
        // thread with its own message, as it would on one thread.
        let epoch = Instant::now();
        let mut rng = fastrand::Rng::with_seed(1);
        let mut ingest = Ingest::<Trade>::new(HighlightPolicy::default(), epoch);
        ingest.apply(
            (0..2_000).map(|id| Op::UpsertSilent(Arc::new(Trade::random(&mut rng, id)))),
            epoch,
        );
        let snapshot = ingest.snapshot();
        let spec = ViewSpec::new().with_filter(ColumnSet::first(1), |trade: &Trade| {
            assert_ne!(trade.id, 1_500, "filter failed");
            true
        });
        let parallelism = Parallelism {
            threads: 4,
            chunk_rows: 16,
        };
        let panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            project(&snapshot, &spec, parallelism)
        }))
        .expect_err("the filter panics");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default();
        assert!(message.contains("filter failed"), "{message}");
    }

    /// A snapshot of `count` random trades with some removed, so slots are
    /// reused and storage order differs from insertion order.
    fn random_snapshot(rng: &mut fastrand::Rng, count: u32) -> Arc<StoreSnapshot<Trade>> {
        random_snapshot_with(rng, count, false)
    }

    /// [`random_snapshot`], the removed rows fading when `fade`: new rows
    /// take their slots.
    fn random_snapshot_with(
        rng: &mut fastrand::Rng,
        count: u32,
        fade: bool,
    ) -> Arc<StoreSnapshot<Trade>> {
        let epoch = Instant::now();
        let mut ingest = Ingest::<Trade>::new(policy(fade), epoch);
        ingest.apply(
            (0..count).map(|id| Op::UpsertSilent(Arc::new(Trade::random(rng, id)))),
            epoch,
        );
        ingest.apply(
            (0..count / 3).map(|_| Op::Remove(rng.u32(..count.max(1)))),
            epoch,
        );
        ingest.apply(
            (count..count + count / 4).map(|id| Op::UpsertSilent(Arc::new(Trade::random(rng, id)))),
            epoch,
        );
        ingest.snapshot()
    }

    #[test]
    fn test_flat_projection_readers() {
        let engine = Rebuild::default().with_max_threads(1);
        for seed in 0..24 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let count = [0, 1, 7, 100, 300][rng.usize(..5)];
            let snapshot = random_snapshot(&mut rng, count);
            let spec = random_spec(&mut rng);
            let projection = engine.rebuild(&snapshot, &spec);
            let context = format!("seed {seed}, {count} rows");

            // The lines are the rows at depth 0; nothing else is listed.
            assert_eq!(projection.len(), projection.rows().len(), "{context}");
            assert_eq!(projection.row_count(), projection.len(), "{context}");
            assert_eq!(projection.is_empty(), projection.rows().is_empty());
            for (ix, &id) in projection.rows().iter().enumerate() {
                assert_eq!(projection.row(ix), Some(id), "{context}");
                assert_eq!(
                    projection.line(ix),
                    Some(DisplayRow::Data { row: id, depth: 0 }),
                    "{context}"
                );
                assert_eq!(projection.display_ix(id), Some(ix), "{context}");
                assert_eq!(projection.order_ix(id), Some(ix), "{context}");
                assert!(projection.contains(id), "{context}");
            }
            assert_eq!(projection.line(projection.len()), None);
            assert_eq!(projection.row(projection.len()), None);
            // Rows the view filters out are nowhere.
            for (id, _) in snapshot.iter() {
                let shown = projection.rows().contains(&id);
                assert_eq!(projection.contains(id), shown, "{context}");
                assert_eq!(projection.order_ix(id).is_some(), shown, "{context}");
                assert_eq!(projection.branch(id), None, "{context}");
            }
            assert!(!projection.is_grouped() && !projection.is_tree());
            assert!(projection.groups().is_empty());
            assert_eq!(projection.groups().iter().count(), 0);
            assert_eq!(projection.group_line(GroupId::new(0)), None);
            assert!(projection.totals().is_empty());
            assert_eq!(projection.totals().columns(), ColumnSet::EMPTY);
            assert_eq!(projection.totals().get(TradeColumn::Price.ix()), None);
            assert_eq!(projection.totals().row_count(), projection.row_count());
            let expanded: Vec<DisplayRow> = projection.expanded_lines().collect();
            let lines: Vec<DisplayRow> = (0..projection.len())
                .filter_map(|ix| projection.line(ix))
                .collect();
            assert_eq!(expanded, lines, "{context}");
            assert_eq!(
                projection.shown_rows().collect::<Vec<_>>(),
                projection.rows(),
                "{context}"
            );

            // The default `aggregate` and `expand` keep the order: a flat
            // projection has nothing to aggregate or expand.
            let aggregated = engine.aggregate(&projection, &snapshot, &spec);
            assert_eq!(aggregated.rows(), projection.rows(), "{context}");
            assert_eq!(aggregated.store_version(), snapshot.version());
            let expanded = engine.expand(&projection, &snapshot, &spec);
            assert_eq!(expanded.rows(), projection.rows(), "{context}");
            assert_eq!(expanded.len(), projection.len(), "{context}");
            assert_eq!(expanded.store_version(), projection.store_version());
            let compacted = engine.compact(&projection, &snapshot);
            assert_eq!(compacted.rows(), projection.rows(), "{context}");
        }
    }

    #[test]
    fn test_grouped_rows_follow_the_level_sort() {
        // Grouping by a column in key order lists the rows as a flat sort by
        // that column, then the view's sort, would.
        let engine = Rebuild::default().with_max_threads(1);
        for seed in 0..12 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let snapshot = random_snapshot(&mut rng, 200);
            let spec = random_spec(&mut rng);
            let direction = [SortDirection::Ascending, SortDirection::Descending][rng.usize(..2)];
            let level =
                [TradeColumn::Group, TradeColumn::Mixed, TradeColumn::Price][rng.usize(..3)];
            let grouped = engine.rebuild(
                &snapshot,
                &spec
                    .clone()
                    .with_group_by([GroupBy::column(level).with_direction(direction)]),
            );
            let sort: Vec<_> = std::iter::once((level, direction))
                .chain(spec.sort().iter().copied())
                .collect();
            let flat = engine.rebuild(&snapshot, &spec.clone().with_sort(sort));
            assert_eq!(grouped.rows(), flat.rows(), "seed {seed}");
            assert!(grouped.is_grouped() && !grouped.is_tree());
            assert_eq!(grouped.row_count(), flat.row_count());
        }
    }

    /// Takes an aggregate over every row, in order: a float sum, so the
    /// order shows in the last bits.
    struct OrderedSum;

    impl AggregateFn<Trade> for OrderedSum {
        fn aggregate(&self, _: TradeColumn, rows: &mut dyn Iterator<Item = &Trade>) -> OwnedValue {
            OwnedValue::Float(rows.map(|trade| trade.price * 0.1).sum())
        }
    }

    /// A grouped view of `Trade`s: one to three levels, aggregates of every
    /// kind, either group order and footer.
    fn random_grouped_spec(rng: &mut fastrand::Rng) -> ViewSpec<Trade> {
        let direction = |rng: &mut fastrand::Rng| {
            [SortDirection::Ascending, SortDirection::Descending][rng.usize(..2)]
        };
        let letter = || {
            GroupKey::new("letter", ColumnSet::first(4), |trade: &Trade| {
                CellValue::Text(trade.name.get(..1).unwrap_or_default())
            })
        };
        let levels: Vec<GroupBy<Trade>> = (0..rng.usize(1..=3))
            .map(|_| {
                let level = match rng.u8(0..5) {
                    0 => GroupBy::column(TradeColumn::Group),
                    1 => GroupBy::column(TradeColumn::Flag),
                    2 => GroupBy::column(TradeColumn::Mixed),
                    3 => GroupBy::column(TradeColumn::Price),
                    _ => GroupBy::key(letter()),
                };
                level.with_direction(direction(rng))
            })
            .collect();
        let spec = random_spec(rng).with_group_by(levels).with_aggregates([
            (TradeColumn::Price, Aggregate::Sum),
            (TradeColumn::Group, Aggregate::Avg),
            (TradeColumn::Name, Aggregate::Min),
            (TradeColumn::Mixed, Aggregate::Max),
            (TradeColumn::Flag, Aggregate::Count),
            (TradeColumn::Id, Aggregate::Custom(Arc::new(OrderedSum))),
        ]);
        let spec = if rng.bool() {
            spec.with_group_order(AggregateOrder::new(TradeColumn::Price, direction(rng)))
        } else {
            spec
        };
        if rng.bool() {
            spec.with_group_footer(EveryGroupFooter)
        } else {
            spec
        }
    }

    /// Asserts two projections are the same, floats bit for bit.
    fn assert_same_projection(a: &Projection, b: &Projection, context: &str) {
        let same = |a: &OwnedValue, b: &OwnedValue| {
            std::mem::discriminant(a) == std::mem::discriminant(b) && a == b
        };
        let same_row = |a: AggregateRow<'_>, b: AggregateRow<'_>| {
            a.columns() == b.columns()
                && a.row_count() == b.row_count()
                && TradeColumn::all().iter().all(|column| {
                    match (a.get(column.ix()), b.get(column.ix())) {
                        (Some(a), Some(b)) => same(a, b),
                        (a, b) => a.is_none() && b.is_none(),
                    }
                })
        };
        assert_eq!(a.rows(), b.rows(), "{context}");
        assert_eq!(a.len(), b.len(), "{context}");
        for ix in 0..a.len() {
            assert_eq!(a.line(ix), b.line(ix), "{context}, line {ix}");
        }
        assert_eq!(a.row_count(), b.row_count(), "{context}");
        assert!(same_row(a.totals(), b.totals()), "{context}: totals");
        assert_eq!(a.groups().len(), b.groups().len(), "{context}");
        for (a, b) in a.groups().iter().zip(b.groups().iter()) {
            assert_eq!(a.path(), b.path(), "{context}");
            assert_eq!(a.parent(), b.parent(), "{context}");
            assert_eq!(a.rows(), b.rows(), "{context}");
            assert_eq!(a.row_count(), b.row_count(), "{context}");
            assert_eq!(
                (a.is_expanded(), a.has_footer()),
                (b.is_expanded(), b.has_footer()),
                "{context}"
            );
            assert!(same_row(a.aggregates(), b.aggregates()), "{context}: {a:?}");
        }
    }

    #[test]
    fn test_grouped_rebuild_is_thread_independent() {
        // As `test_parallel_rebuild_matches_sequential`, with groups and
        // aggregates: chunk and run boundaries everywhere, the same
        // projection bit for bit, float sums included. Removed rows fade in
        // odd seeds.
        let mut fading = 0;
        for seed in 0..48 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let count = [0, 1, 2, 7, 100, 1_000, 3_000][rng.usize(..7)];
            let snapshot = random_snapshot_with(&mut rng, count, seed % 2 == 1);
            for _ in 0..4 {
                let spec = random_grouped_spec(&mut rng);
                let Structure::Groups(grouping) = spec.structure() else {
                    unreachable!("a grouped view");
                };
                let sequential = group::rebuild_with(
                    &snapshot,
                    &spec,
                    grouping,
                    Parallelism {
                        threads: 1,
                        chunk_rows: CHUNK_ROWS,
                    },
                );
                let parallelism = Parallelism {
                    threads: rng.usize(2..=7),
                    chunk_rows: rng.usize(1..=97),
                };
                let parallel = group::rebuild_with(&snapshot, &spec, grouping, parallelism);
                assert_same_projection(
                    &parallel,
                    &sequential,
                    &format!("seed {seed}, {count} rows, {parallelism:?}"),
                );
                fading += sequential.rows().len() - sequential.row_count();
                for (order_ix, &id) in parallel.rows().iter().enumerate() {
                    assert_eq!(parallel.order_ix(id), Some(order_ix), "seed {seed}");
                }
            }
        }
        assert!(fading > 1_000, "{fading} fading rows");
    }

    #[test]
    fn test_group_key_panic_resumes() {
        // A panicking `GroupKey` fails the rebuild on the calling thread
        // with its own message, as a panicking filter does.
        let mut rng = fastrand::Rng::with_seed(2);
        let snapshot = random_snapshot(&mut rng, 3_000);
        let spec = ViewSpec::new().with_group_by([GroupBy::key(GroupKey::new(
            "checked",
            ColumnSet::first(1),
            |trade: &Trade| {
                assert_ne!(trade.id, 1_500, "group key failed");
                CellValue::Int(trade.group)
            },
        ))]);
        let Structure::Groups(grouping) = spec.structure() else {
            unreachable!("a grouped view");
        };
        let parallelism = Parallelism {
            threads: 4,
            chunk_rows: 16,
        };
        let panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            group::rebuild_with(&snapshot, &spec, grouping, parallelism)
        }))
        .expect_err("the group key panics");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default();
        assert!(message.contains("group key failed"), "{message}");
    }

    #[test]
    fn test_dependencies_with_groups() {
        let mut expected = ColumnSet::EMPTY;
        expected.insert(TradeColumn::Price.ix());
        let sorted =
            ViewSpec::<Trade>::new().with_sort([(TradeColumn::Price, SortDirection::Ascending)]);
        assert_eq!(sorted.dependencies(), expected);
        // Group columns, what a key reads and what the group order reads.
        let mut key_columns = ColumnSet::EMPTY;
        key_columns.insert(TradeColumn::Name.ix());
        let grouped = sorted
            .clone()
            .with_group_by([
                GroupBy::column(TradeColumn::Group),
                GroupBy::key(GroupKey::new("letter", key_columns, |trade: &Trade| {
                    CellValue::Text(trade.name.get(..1).unwrap_or_default())
                })),
            ])
            .with_aggregates([(TradeColumn::Mixed, Aggregate::Sum)])
            .with_group_order(AggregateOrder::new(
                TradeColumn::Flag,
                SortDirection::Ascending,
            ));
        for column in [TradeColumn::Group, TradeColumn::Name, TradeColumn::Flag] {
            expected.insert(column.ix());
        }
        assert_eq!(grouped.dependencies(), expected);
        // Aggregates alone change values, not the order.
        assert!(!grouped.dependencies().contains(TradeColumn::Mixed.ix()));
        assert!(
            grouped
                .aggregate_columns()
                .contains(TradeColumn::Mixed.ix())
        );
        // Without grouping, neither the levels nor the group order count.
        let flat = grouped.with_group_by([]);
        let mut price = ColumnSet::EMPTY;
        price.insert(TradeColumn::Price.ix());
        assert_eq!(flat.dependencies(), price);
    }

    /// A weighted aggregate of a column that reads another column.
    struct Weighted;

    impl AggregateFn<Trade> for Weighted {
        fn aggregate(&self, _: TradeColumn, _: &mut dyn Iterator<Item = &Trade>) -> OwnedValue {
            OwnedValue::Empty
        }

        fn depends_on(&self, column: TradeColumn) -> ColumnSet {
            let mut columns = ColumnSet::EMPTY;
            columns.insert(column.ix());
            columns.insert(TradeColumn::Group.ix());
            columns
        }

        fn name(&self) -> &str {
            "Weighted"
        }
    }

    #[test]
    fn test_view_spec_structure() {
        let by_group = GroupBy::column(TradeColumn::Group);
        let by_letter = GroupBy::key(
            GroupKey::new("letter", ColumnSet::first(4), |trade: &Trade| {
                CellValue::Text(trade.name.get(..1).unwrap_or_default())
            })
            .with_title("First letter"),
        )
        .with_direction(SortDirection::Descending);
        let spec = ViewSpec::<Trade>::new()
            .with_aggregates([
                (TradeColumn::Price, Aggregate::Sum),
                (TradeColumn::Id, Aggregate::Count),
                (TradeColumn::Price, Aggregate::Max),
            ])
            .with_group_by([by_group.clone(), by_letter, by_group.clone()]);

        // Repeated levels are dropped, the first stays.
        let levels: Vec<ColumnId> = spec.group_by().iter().map(GroupBy::id).collect();
        assert_eq!(levels, [ColumnId::from("group"), ColumnId::from("letter")]);
        assert_eq!(spec.group_by()[0].as_column(), Some(TradeColumn::Group));
        assert_eq!(spec.group_by()[1].as_column(), None);
        let key = spec.group_by()[1].as_key().expect("a key level");
        assert_eq!((key.id(), key.title()), ("letter", "First letter"));
        assert_eq!(spec.group_by()[1].direction(), SortDirection::Descending);
        assert!(spec.tree().is_none());
        // A repeated column keeps its last aggregate, in its first place.
        let aggregates: Vec<(TradeColumn, String)> = spec
            .aggregates()
            .iter()
            .map(|(column, aggregate)| (*column, format!("{aggregate:?}")))
            .collect();
        assert_eq!(
            aggregates,
            [
                (TradeColumn::Price, "Max".to_owned()),
                (TradeColumn::Id, "Count".to_owned())
            ]
        );

        // A tree replaces the grouping, and a grouping the tree; the
        // aggregates outlive both.
        let tree = spec
            .clone()
            .with_tree(TreeSpec::new().with_column(TradeColumn::Name));
        assert!(tree.group_by().is_empty());
        assert_eq!(
            tree.tree().and_then(TreeSpec::column),
            Some(TradeColumn::Name)
        );
        assert!(matches!(
            tree.tree().unwrap().filter(),
            TreeFilter::KeepAncestors
        ));
        assert!(matches!(tree.tree().unwrap().totals(), TreeTotals::Leaves));
        assert_eq!(tree.aggregates().len(), 2);
        let regrouped = tree.with_group_by([by_group]);
        assert!(regrouped.tree().is_none());
        assert_eq!(regrouped.group_by().len(), 1);
        // An empty grouping is the flat view.
        let flat = regrouped.with_group_by([]);
        assert!(flat.group_by().is_empty() && flat.tree().is_none());
        assert!(matches!(flat.structure(), Structure::Flat));
        assert_eq!(flat.aggregates().len(), 2);

        // Aggregate columns: the aggregated ones, and what a custom one
        // depends on.
        let mut expected = ColumnSet::EMPTY;
        expected.insert(TradeColumn::Price.ix());
        expected.insert(TradeColumn::Id.ix());
        assert_eq!(flat.aggregate_columns(), expected);
        let custom =
            flat.with_aggregates([(TradeColumn::Price, Aggregate::Custom(Arc::new(Weighted)))]);
        let mut expected = ColumnSet::EMPTY;
        expected.insert(TradeColumn::Price.ix());
        expected.insert(TradeColumn::Group.ix());
        assert_eq!(custom.aggregate_columns(), expected);
        assert_eq!(
            format!("{:?}", custom.aggregates()[0].1),
            r#"Custom("Weighted")"#
        );
        assert!(ViewSpec::<Trade>::new().aggregates().is_empty());
        assert!(ViewSpec::<Trade>::new().aggregate_columns().is_empty());

        // The group order and footer: key order and none by default.
        let a = GroupPath::new([(ColumnId::from("group"), OwnedValue::Int(1))]);
        let b = GroupPath::new([(ColumnId::from("group"), OwnedValue::Int(2))]);
        let values = [OwnedValue::Float(2.0)];
        let low = [OwnedValue::Float(1.0)];
        let mut price = ColumnSet::EMPTY;
        price.insert(TradeColumn::Price.ix());
        let first = GroupOrderContext::new(&a, 3, AggregateRow::new(price, &values, 3));
        let second = GroupOrderContext::new(&b, 1, AggregateRow::new(price, &low, 1));
        let spec = ViewSpec::<Trade>::new();
        assert_eq!(spec.group_order().compare(&first, &second), Ordering::Equal);
        assert!(spec.group_order().depends_on().is_empty());
        let by_sum = spec.with_group_order(AggregateOrder::new(
            TradeColumn::Price,
            SortDirection::Descending,
        ));
        assert_eq!(
            by_sum.group_order().compare(&first, &second),
            Ordering::Less
        );
        assert_eq!(by_sum.group_order().depends_on(), price);
        assert_eq!(first.value(), &OwnedValue::Int(1));
        assert_eq!((first.depth(), first.row_count()), (0, 3));
        assert_eq!(first.path(), &a);
        assert!(by_sum.expansion() == &Expansion::default());
    }
}
