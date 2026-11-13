//! The tree view: rows arranged by `GridRow::parent_key`. No GPUI types.
//!
//! [`rebuild`] works on a dense array of the snapshot's rows, in passes of
//! O(n) apart from the parent lookups and the sort, none of them recursive:
//!
//! 1. the rows that pass the view's filters and each row's parent, split
//!    across threads like the flat path;
//! 2. cycles of parents broken: their rows become roots;
//! 3. with a filter, the rows the [`TreeFilter`] shows, and for each the
//!    nearest shown ancestor as its parent;
//! 4. the shown rows sorted once by the view's sort and insertion, across
//!    threads, then dealt to their parents in that order, so siblings are
//!    sorted;
//! 5. a depth-first walk from the sorted roots: `Projection::rows`, each
//!    row's depth and subtree, and the lines its expansion shows;
//! 6. the totals of the rows [`TreeTotals`] counts.
//!
//! The order the rows arrived in does not matter: a child written before
//! its parent is a root until the parent comes, and the insert rebuilds.
//!
//! A removed row that fades stays a leaf under its parent while that is
//! shown, or a root if it had no parent; otherwise it goes at once. It is
//! never a parent: its children are orphans, roots. Neither the row count
//! nor the totals count it.

use std::{
    cmp::Ordering,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    },
};

use super::{
    CHUNK_ROWS, MIN_ROWS_PER_THREAD, Parallelism, Projection, Rebuild, Structure, ViewSpec,
    VisibleRows,
    aggregate::{ViewAggregates, flat_totals},
    compare_sorted, display,
    display::{AggregateValues, BuiltStructure, DisplayRow},
    merge, run_on_threads,
};
#[cfg(any(test, feature = "test-support"))]
use super::{ExpandDepth, Expansion};
use crate::{
    model::{GridRow, OwnedValue},
    store::{RowId, StoreSnapshot},
};

/// Which rows of a filtered tree are shown.
/// Without a filter and a quick filter every row passes and is shown.
#[derive(Clone, Default)]
#[non_exhaustive]
pub enum TreeFilter {
    /// A row is shown if it passes, or if one of its descendants does: the
    /// matches keep their ancestors for context. Rows below a match that do
    /// not pass themselves stay hidden. The default.
    #[default]
    KeepAncestors,
    /// A row is shown if it and every ancestor pass: a failing row hides
    /// its subtree.
    HideSubtree,
    /// The application's rule, such as "a passing row shows its whole
    /// subtree".
    Custom(Arc<dyn TreeFilterRule>),
}

impl TreeFilter {
    /// Whether the rule shows a row.
    fn is_shown(&self, row: &TreeFilterContext) -> bool {
        match self {
            Self::KeepAncestors => row.passes || row.has_passing_descendant,
            Self::HideSubtree => row.passes && row.ancestors_pass,
            Self::Custom(rule) => rule.is_shown(row),
        }
    }
}

impl fmt::Debug for TreeFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeepAncestors => f.write_str("KeepAncestors"),
            Self::HideSubtree => f.write_str("HideSubtree"),
            Self::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

/// A rule for [`TreeFilter::Custom`]. Called for every row in the
/// background while the view has a filter or a quick filter; without them
/// every row is shown and the rule is not called.
pub trait TreeFilterRule: Send + Sync + 'static {
    /// Returns true if the row is shown. A shown row whose parent is hidden
    /// hangs from its nearest shown ancestor, or is a root.
    fn is_shown(&self, row: &TreeFilterContext) -> bool;
}

/// What a tree filter rule knows about one row.
#[derive(Clone, Copy, Debug)]
pub struct TreeFilterContext {
    passes: bool,
    has_passing_descendant: bool,
    ancestors_pass: bool,
    has_passing_ancestor: bool,
}

impl TreeFilterContext {
    /// A row that passes the filters or not, with what its descendants and
    /// ancestors do.
    pub(crate) fn new(
        passes: bool,
        has_passing_descendant: bool,
        ancestors_pass: bool,
        has_passing_ancestor: bool,
    ) -> Self {
        Self {
            passes,
            has_passing_descendant,
            ancestors_pass,
            has_passing_ancestor,
        }
    }

    /// The row passes the view's filter and quick filter.
    pub fn passes(&self) -> bool {
        self.passes
    }

    /// Some descendant of the row passes.
    pub fn has_passing_descendant(&self) -> bool {
        self.has_passing_descendant
    }

    /// Every ancestor of the row passes (true for a root).
    pub fn ancestors_pass(&self) -> bool {
        self.ancestors_pass
    }

    /// Some ancestor of the row passes (false for a root).
    pub fn has_passing_ancestor(&self) -> bool {
        self.has_passing_ancestor
    }
}

/// Which rows of a tree the totals add up. A parent often holds the sum of
/// its children already, so adding every row would count it twice.
///
/// The totals add the view's aggregates ([`ViewSpec::with_aggregates`]) of
/// the live shown rows the rule counts, in the order of
/// `Projection::rows`; their row count is every live shown row. Fading
/// rows count for nothing: a row whose shown children all fade is a leaf.
#[derive(Clone, Default)]
#[non_exhaustive]
pub enum TreeTotals {
    /// Shown rows with no shown children. The default.
    #[default]
    Leaves,
    /// Every shown row.
    AllRows,
    /// The application's rule.
    Custom(Arc<dyn TreeTotalsRule>),
}

impl TreeTotals {
    /// Whether the rule counts a shown row.
    fn is_counted(&self, row: &TreeTotalsContext) -> bool {
        match self {
            Self::Leaves => !row.is_branch,
            Self::AllRows => true,
            Self::Custom(rule) => rule.is_counted(row),
        }
    }
}

impl fmt::Debug for TreeTotals {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Leaves => f.write_str("Leaves"),
            Self::AllRows => f.write_str("AllRows"),
            Self::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

/// A rule for [`TreeTotals::Custom`]. Called for every shown row in the
/// background.
pub trait TreeTotalsRule: Send + Sync + 'static {
    /// Returns true if the totals count the row.
    fn is_counted(&self, row: &TreeTotalsContext) -> bool;
}

/// What a tree totals rule knows about one row.
#[derive(Clone, Copy, Debug)]
pub struct TreeTotalsContext {
    depth: u16,
    is_branch: bool,
    passes: bool,
}

impl TreeTotalsContext {
    /// A shown row at `depth`, with shown children or not, passing the
    /// filters itself or kept for context.
    pub(crate) fn new(depth: u16, is_branch: bool, passes: bool) -> Self {
        Self {
            depth,
            is_branch,
            passes,
        }
    }

    /// The row's depth: 0 for a root.
    pub fn depth(&self) -> u16 {
        self.depth
    }

    /// The row has shown children.
    pub fn is_branch(&self) -> bool {
        self.is_branch
    }

    /// The row passes the filters itself, not only kept for context.
    pub fn passes(&self) -> bool {
        self.passes
    }
}

/// How a view shows a tree.
pub struct TreeSpec<R: GridRow> {
    column: Option<R::Column>,
    filter: TreeFilter,
    totals: TreeTotals,
}

impl<R: GridRow> Clone for TreeSpec<R> {
    fn clone(&self) -> Self {
        Self {
            column: self.column,
            filter: self.filter.clone(),
            totals: self.totals.clone(),
        }
    }
}

impl<R: GridRow> Default for TreeSpec<R> {
    fn default() -> Self {
        Self {
            column: None,
            filter: TreeFilter::default(),
            totals: TreeTotals::default(),
        }
    }
}

impl<R: GridRow> fmt::Debug for TreeSpec<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeSpec")
            .field("column", &self.column)
            .field("filter", &self.filter)
            .field("totals", &self.totals)
            .finish()
    }
}

impl<R: GridRow> TreeSpec<R> {
    /// A tree with the defaults: the first visible column shows the
    /// hierarchy, [`TreeFilter::KeepAncestors`], totals over the leaves.
    pub fn new() -> Self {
        Self::default()
    }

    /// The column that shows the indentation and the disclosure chevron.
    pub fn with_column(mut self, column: R::Column) -> Self {
        self.column = Some(column);
        self
    }

    /// The column that shows the hierarchy; `None`: the first visible one.
    pub fn column(&self) -> Option<R::Column> {
        self.column
    }

    /// Which rows a filtered tree shows (default
    /// [`TreeFilter::KeepAncestors`]).
    pub fn with_filter(mut self, filter: TreeFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Which rows a filtered tree shows.
    pub fn filter(&self) -> &TreeFilter {
        &self.filter
    }

    /// Which rows the totals add up (default [`TreeTotals::Leaves`]).
    pub fn with_totals(mut self, totals: TreeTotals) -> Self {
        self.totals = totals;
        self
    }

    /// Which rows the totals add up.
    pub fn totals(&self) -> &TreeTotals {
        &self.totals
    }
}

impl<R: GridRow> ViewSpec<R> {
    /// Shows the rows as a tree by `GridRow::parent_key`. Replaces a
    /// grouping, as in AG Grid and DevExpress: the last of
    /// [`Self::with_group_by`] and `with_tree` wins.
    ///
    /// A row whose parent key no row has is an orphan and shows as a root,
    /// as does a row that names itself. Rows whose parents form a cycle
    /// show as roots too, with a warning in the log. Siblings follow the
    /// view's sort; the filters choose rows by [`TreeSpec::with_filter`].
    pub fn with_tree(mut self, tree: TreeSpec<R>) -> Self {
        self.structure = Structure::Tree(tree);
        self
    }

    /// The tree, if the view is one.
    pub fn tree(&self) -> Option<&TreeSpec<R>> {
        match self.structure() {
            Structure::Tree(tree) => Some(tree),
            Structure::Flat | Structure::Groups(_) => None,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl<R: GridRow> ViewSpec<R> {
    /// Lays a tree view out for `depth`, with the rows of the `toggled` keys
    /// toggled against it: a key given twice is toggled back. The grid owns
    /// the expansion; this sets it for tests of the engine alone.
    #[doc(hidden)]
    pub fn with_tree_expansion(
        self,
        depth: ExpandDepth,
        toggled: impl IntoIterator<Item = R::Key>,
    ) -> Self {
        let expansion = toggled
            .into_iter()
            .fold(Expansion::new(depth), |expansion, key| {
                // A toggle inverts the default at any depth.
                let expanded = expansion.is_row_expanded(&key, 0);
                expansion.with_row(key, !expanded, 0)
            });
        self.with_expansion(expansion)
    }
}

/// A shown tree row with shown children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Branch {
    children: u32,
    descendants: u32,
    expanded: bool,
}

impl Branch {
    /// The shown children.
    pub fn child_count(&self) -> usize {
        self.children as usize
    }

    /// The shown descendants, children included. The subtree follows the
    /// row in `Projection::rows`: the next `descendant_count` rows.
    pub fn descendant_count(&self) -> usize {
        self.descendants as usize
    }

    /// Returns true if the children are shown. Answered for a row hidden by
    /// a collapsed ancestor too.
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }
}

/// No row: the parent of a root, the place of a row not in the array.
const NONE: u32 = u32::MAX;

/// One shown row of a tree projection, at its place in `Projection::rows`.
#[derive(Clone, Copy, Debug)]
struct TreeNode {
    /// Shown descendants: the subtree is the next `descendants` rows.
    descendants: u32,
    /// Shown children.
    children: u32,
    /// 0 for a root; a row deeper than `u16::MAX` stays at `u16::MAX`.
    depth: u16,
    /// The children are shown; kept for a row with no children too.
    expanded: bool,
    /// The row passes the filters itself, not only kept for context.
    passes: bool,
}

/// What a tree projection knows beyond its lines.
#[derive(Clone, Debug, Default)]
pub(crate) struct TreeData {
    /// One per row of `Projection::rows`, at the same place.
    nodes: Vec<TreeNode>,
    /// Rows that became roots because their parents formed a cycle.
    cycles: usize,
    /// A row that does not pass is shown only for its passing descendants
    /// ([`TreeFilter::KeepAncestors`]): [`compact`] drops those left with
    /// none, as a rebuild would.
    keeps_ancestors: bool,
}

impl TreeData {
    /// The branch at a place in `Projection::rows`, if the row has shown
    /// children.
    fn branch(&self, order_ix: usize) -> Option<Branch> {
        let node = self.nodes.get(order_ix)?;
        (node.children > 0).then_some(Branch {
            children: node.children,
            descendants: node.descendants,
            expanded: node.expanded,
        })
    }

    /// The depth of the row at a place in `Projection::rows`, shown or
    /// under a collapsed row: what `Projection::expanded_lines` gives it.
    pub(crate) fn depth(&self, order_ix: usize) -> u16 {
        self.nodes.get(order_ix).map_or(0, |node| node.depth)
    }

    /// The live rows of `rows` (the projection's) that `rule` counts, in
    /// order; `row` gives `None` for a fading row. A row is a branch to the
    /// rule when one of its shown children is live.
    fn counted<'a, R: GridRow>(
        &'a self,
        rule: &'a TreeTotals,
        live: &'a [Option<&'a R>],
    ) -> impl Iterator<Item = &'a R> + Clone + 'a {
        // Each live row marks its parent: the row above it one level up.
        let mut has_live_child = vec![false; self.nodes.len()];
        let mut ancestors: Vec<usize> = Vec::new();
        for (order_ix, node) in self.nodes.iter().enumerate() {
            ancestors.truncate(usize::from(node.depth));
            if live[order_ix].is_some()
                && let Some(&parent) = ancestors.last()
            {
                has_live_child[parent] = true;
            }
            ancestors.push(order_ix);
        }
        self.nodes.iter().zip(live).zip(has_live_child).filter_map(
            move |((node, row), is_branch)| {
                let row = (*row)?;
                rule.is_counted(&TreeTotalsContext::new(node.depth, is_branch, node.passes))
                    .then_some(row)
            },
        )
    }

    /// The totals of the rows of `rows` (the projection's) that `rule`
    /// counts, and the live rows; `row` gives `None` for a fading row.
    fn totals<'a, R: GridRow>(
        &self,
        aggregates: &ViewAggregates<'_, R>,
        rule: &TreeTotals,
        rows: &[RowId],
        row: impl Fn(RowId) -> Option<&'a R>,
    ) -> (Vec<OwnedValue>, usize) {
        let live: Vec<Option<&'a R>> = rows.iter().map(|&id| row(id)).collect();
        let count = live.iter().flatten().count();
        if aggregates.is_empty() {
            return (Vec::new(), count);
        }
        let (totals, _) = flat_totals(aggregates, self.counted(rule, &live));
        (totals, count)
    }
}

impl Projection {
    /// The tree row with shown children; `None` for a leaf, a row the view
    /// does not show, and outside a tree.
    pub fn branch(&self, id: RowId) -> Option<Branch> {
        self.tree_data()?.branch(self.order_ix(id)?)
    }

    /// The rows of a tree shown as roots because their parents formed a
    /// cycle; 0 outside a tree. A row that is its own parent is a root but
    /// not a cycle.
    pub fn tree_cycles(&self) -> usize {
        self.tree_data().map_or(0, |tree| tree.cycles)
    }
}

/// Rows in cycles at the last tree rebuild of this process. A warning is
/// written when the count changes, not on every rebuild: at 10 rebuilds a
/// second that would be noise. A count, not a clock (AGENTS.md, "Store
/// clock only").
static CYCLE_ROWS: AtomicUsize = AtomicUsize::new(0);

/// Records `cycles` in `last`: true if there are cycles and their count
/// changed since the last call.
fn is_new_cycle_count(last: &AtomicUsize, cycles: usize) -> bool {
    last.swap(cycles, AtomicOrdering::Relaxed) != cycles && cycles > 0
}

/// One row of the dense array while a tree is built.
#[derive(Clone, Copy, Debug, Default)]
struct Link {
    /// The parent's place in the array, or [`NONE`]. Once the filter has
    /// run, the nearest shown ancestor.
    parent: u32,
    /// The row passes the view's filter and quick filter.
    passes: bool,
    /// The tree filter shows the row.
    shown: bool,
}

/// What a row's ancestors and descendants do, for the tree filter.
#[derive(Clone, Copy, Debug)]
struct Lineage {
    has_passing_descendant: bool,
    ancestors_pass: bool,
    has_passing_ancestor: bool,
    /// The row itself if shown, else its nearest shown ancestor or [`NONE`].
    nearest_shown: u32,
}

/// Children lists of a forest, compressed: the children of `node` are
/// `items[starts[node]..starts[node + 1]]`.
struct Children {
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl Children {
    /// The children of `nodes` places, each list in the order of `order`;
    /// `parent` gives a node's parent or [`NONE`].
    fn new(
        nodes: usize,
        order: impl DoubleEndedIterator<Item = u32> + Clone,
        parent: impl Fn(u32) -> u32,
    ) -> Self {
        let mut starts = vec![0u32; nodes + 1];
        let mut count = 0;
        for node in order.clone() {
            let parent = parent(node);
            if parent != NONE {
                starts[parent as usize] += 1;
                count += 1;
            }
        }
        // Each entry becomes the end of its node's list...
        let mut end = 0;
        for start in &mut starts {
            end += *start;
            *start = end;
        }
        // ...and, filled from the back, its start.
        let mut items = vec![0u32; count];
        for node in order.rev() {
            let parent = parent(node);
            if parent != NONE {
                let start = &mut starts[parent as usize];
                *start -= 1;
                items[*start as usize] = node;
            }
        }
        Self { starts, items }
    }

    /// The range of `node`'s children in `items`.
    fn range(&self, node: u32) -> (u32, u32) {
        let node = node as usize;
        (self.starts[node], self.starts[node + 1])
    }
}

/// A thread per [`MIN_ROWS_PER_THREAD`] rows, at most `budget`: a tree
/// always has work, the parent lookups.
fn parallelism(rows: usize, budget: usize) -> Parallelism {
    Parallelism {
        threads: budget.min(rows / MIN_ROWS_PER_THREAD).max(1),
        chunk_rows: CHUNK_ROWS,
    }
}

/// Builds a tree view.
pub(super) fn rebuild<R: GridRow>(
    engine: &Rebuild,
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    tree: &TreeSpec<R>,
) -> Projection {
    let parallelism = parallelism(snapshot.len(), engine.max_threads());
    let projection = build(snapshot, spec, tree, parallelism);
    let cycles = projection.tree_cycles();
    if is_new_cycle_count(&CYCLE_ROWS, cycles) {
        log::warn!(
            "gpui-kit-grid: the parents of {cycles} tree rows form cycles; those rows are shown as roots"
        );
    }
    projection
}

/// Builds a tree view with `parallelism`.
fn build<R: GridRow>(
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    tree: &TreeSpec<R>,
    parallelism: Parallelism,
) -> Projection {
    // The live rows in a dense array, and each slot's place in it; fading
    // rows join at the end.
    let mut rows = Vec::with_capacity(snapshot.len());
    rows.extend(snapshot.iter());
    let mut places = vec![NONE; snapshot.capacity()];
    for (place, (id, _)) in rows.iter().enumerate() {
        places[id.ix()] = place as u32;
    }

    // Whether each row passes, and its parent: an orphan and a row that
    // names itself are roots.
    let mut links = map_rows(
        &rows,
        parallelism,
        || VisibleRows::new(spec),
        |visible, place, row| Link {
            parent: row
                .parent_key()
                .and_then(|key| snapshot.row_id(key))
                .and_then(|id| places.get(id.ix()).copied())
                .filter(|&parent| parent != place as u32)
                .unwrap_or(NONE),
            passes: visible.contains(row),
            shown: true,
        },
    );
    let cycles = break_cycles(&mut links);
    let mut shown = if spec.filter.is_some() || spec.quick_filter.is_some() {
        apply_filter(&mut links, tree.filter())
    } else {
        links.len()
    };
    let fading_seqs = add_fading(snapshot, spec, &places, &mut rows, &mut links);
    shown += fading_seqs.len();
    drop(places);

    // Siblings in the order of the view: sorted once, dealt in order.
    let sorted = sort_shown(
        snapshot,
        spec,
        &rows,
        &links,
        &fading_seqs,
        shown,
        parallelism,
    );
    let children = Children::new(links.len(), sorted.iter().copied(), |place| {
        links[place as usize].parent
    });

    // Depth first from the sorted roots.
    let expansion = spec.expansion();
    let mut order = Vec::with_capacity(shown);
    let mut nodes = Vec::with_capacity(shown);
    // The rows being walked: (place in `order`, next child, end of
    // children); the stack's length is the depth of the next row.
    let mut stack: Vec<(u32, u32, u32)> = Vec::with_capacity(shown);
    for &root in &sorted {
        if links[root as usize].parent != NONE {
            continue;
        }
        let mut next = Some(root);
        loop {
            if let Some(place) = next.take() {
                let (id, row) = rows[place as usize];
                let (start, end) = children.range(place);
                let depth = stack.len().min(usize::from(u16::MAX)) as u16;
                stack.push((order.len() as u32, start, end));
                order.push(id);
                nodes.push(TreeNode {
                    descendants: 0,
                    children: end - start,
                    depth,
                    expanded: end > start && expansion.is_row_expanded(row.key(), depth),
                    passes: links[place as usize].passes,
                });
            }
            let Some(top) = stack.last_mut() else {
                break;
            };
            if top.1 < top.2 {
                next = Some(children.items[top.1 as usize]);
                top.1 += 1;
            } else {
                let order_ix = top.0 as usize;
                stack.pop();
                nodes[order_ix].descendants = (order.len() - order_ix - 1) as u32;
            }
        }
    }
    debug_assert_eq!(order.len(), shown, "every shown row hangs from a root");

    let data = TreeData {
        nodes,
        cycles,
        keeps_ancestors: matches!(tree.filter(), TreeFilter::KeepAncestors),
    };
    let aggregates = ViewAggregates::new(spec);
    let totals = if aggregates.is_empty() {
        AggregateValues::default()
    } else {
        let (totals, _) = data.totals(&aggregates, tree.totals(), &order, |id| {
            snapshot.row(id).map(|row| &**row)
        });
        AggregateValues::new(aggregates.columns(), totals)
    };
    let lines = lay_out(&order, &data.nodes);
    Projection::with_lines(snapshot, order, lines, BuiltStructure::Tree(data))
        .with_row_count(shown - fading_seqs.len())
        .with_totals(totals)
}

/// Adds the fading rows a tree shows to the dense array, after the live
/// ones: those that pass the view's filters, each under its parent while
/// the filter shows that, or at the root if it had no parent (or named
/// itself). A fading row is never a parent, and it keeps no ancestor for
/// context. Returns their insertion sequences, in order.
fn add_fading<'a, R: GridRow>(
    snapshot: &'a StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    places: &[u32],
    rows: &mut Vec<(RowId, &'a Arc<R>)>,
    links: &mut Vec<Link>,
) -> Vec<u64> {
    let mut fading = snapshot.fading_rows().peekable();
    if fading.peek().is_none() {
        return Vec::new();
    }
    let mut visible = VisibleRows::new(spec);
    let mut seqs = Vec::new();
    for (id, row, _, seq) in fading {
        if !visible.contains(row) {
            continue;
        }
        let parent = match row.parent_key() {
            Some(key) if key != row.key() => {
                match snapshot
                    .row_id(key)
                    .and_then(|parent| places.get(parent.ix()).copied())
                {
                    Some(parent) if parent != NONE && links[parent as usize].shown => parent,
                    // A hidden, removed or missing parent: it goes at once.
                    _ => continue,
                }
            }
            _ => NONE,
        };
        rows.push((id, row));
        links.push(Link {
            parent,
            passes: true,
            shown: true,
        });
        seqs.push(seq);
    }
    seqs
}

/// `map` over every row with its place, in order. A large array is split
/// across threads like the flat path's: each takes chunks of rows and has
/// its own `state` (the quick filter's buffers).
fn map_rows<'a, R: GridRow, S, T: Copy + Default + Send>(
    rows: &[(RowId, &'a Arc<R>)],
    parallelism: Parallelism,
    state: impl Fn() -> S + Sync,
    map: impl Fn(&mut S, usize, &'a Arc<R>) -> T + Sync,
) -> Vec<T> {
    if parallelism.threads <= 1 {
        let mut state = state();
        return rows
            .iter()
            .enumerate()
            .map(|(place, &(_, row))| map(&mut state, place, row))
            .collect();
    }

    let chunk_rows = parallelism.chunk_rows;
    let next_chunk = AtomicUsize::new(0);
    // One thread's share: the chunks it took and their values in order.
    let share = || {
        let mut state = state();
        let mut starts = Vec::new();
        let mut values = Vec::new();
        loop {
            let start = next_chunk.fetch_add(chunk_rows, AtomicOrdering::Relaxed);
            if start >= rows.len() {
                break;
            }
            let chunk = &rows[start..rows.len().min(start + chunk_rows)];
            starts.push(start);
            values.extend(
                chunk
                    .iter()
                    .enumerate()
                    .map(|(offset, &(_, row))| map(&mut state, start + offset, row)),
            );
        }
        (starts, values)
    };
    let mut mapped = vec![T::default(); rows.len()];
    for (starts, values) in run_on_threads(&vec![&share; parallelism.threads]) {
        let mut values = values.as_slice();
        for start in starts {
            let (chunk, rest) = values.split_at(chunk_rows.min(rows.len() - start));
            mapped[start..start + chunk.len()].copy_from_slice(chunk);
            values = rest;
        }
    }
    mapped
}

/// Makes every row on a cycle of parents a root and returns how many there
/// are. Their children stay their children. Each row is colored unvisited,
/// on the path or done; from each unvisited row the walk goes up until a
/// root, a done row or a row on the path, which closes a cycle: O(n).
fn break_cycles(links: &mut [Link]) -> usize {
    const UNVISITED: u8 = 0;
    const ON_PATH: u8 = 1;
    const DONE: u8 = 2;

    let mut state = vec![UNVISITED; links.len()];
    let mut path: Vec<u32> = Vec::with_capacity(links.len());
    let mut broken = 0;
    for start in 0..links.len() as u32 {
        let mut place = start;
        while state[place as usize] == UNVISITED {
            state[place as usize] = ON_PATH;
            path.push(place);
            let parent = links[place as usize].parent;
            if parent == NONE {
                break;
            }
            if state[parent as usize] == ON_PATH {
                // The cycle runs from `parent` to the end of the path.
                for &member in path.iter().rev() {
                    links[member as usize].parent = NONE;
                    broken += 1;
                    if member == parent {
                        break;
                    }
                }
                break;
            }
            place = parent;
        }
        for place in path.drain(..) {
            state[place as usize] = DONE;
        }
    }
    broken
}

/// Decides which rows the tree filter shows and hangs each shown row from
/// its nearest shown ancestor. Returns the number of shown rows.
fn apply_filter(links: &mut [Link], filter: &TreeFilter) -> usize {
    let rows = links.len();
    let all = 0..rows as u32;
    let children = Children::new(rows, all.clone(), |place| links[place as usize].parent);

    // Parents before children: depth first from the roots.
    let mut preorder = Vec::with_capacity(rows);
    let mut stack = Vec::with_capacity(rows);
    for root in all.filter(|&place| links[place as usize].parent == NONE) {
        stack.push(root);
        while let Some(place) = stack.pop() {
            preorder.push(place);
            let (start, end) = children.range(place);
            stack.extend(children.items[start as usize..end as usize].iter().rev());
        }
    }
    debug_assert_eq!(preorder.len(), rows, "cycles are broken");

    let mut lineage = vec![
        Lineage {
            has_passing_descendant: false,
            ancestors_pass: true,
            has_passing_ancestor: false,
            nearest_shown: NONE,
        };
        rows
    ];
    // Descendants: children before parents.
    for &place in preorder.iter().rev() {
        let link = links[place as usize];
        if link.parent != NONE && (link.passes || lineage[place as usize].has_passing_descendant) {
            lineage[link.parent as usize].has_passing_descendant = true;
        }
    }
    // Ancestors and the filter: parents before children. A row's own
    // `parent` still names its parent when the row is reached.
    let mut shown = 0;
    for &place in &preorder {
        let link = links[place as usize];
        let (ancestors_pass, has_passing_ancestor, parent_shown) = match link.parent {
            NONE => (true, false, NONE),
            parent => {
                let above = lineage[parent as usize];
                let passes = links[parent as usize].passes;
                (
                    above.ancestors_pass && passes,
                    above.has_passing_ancestor || passes,
                    above.nearest_shown,
                )
            }
        };
        let row = &mut lineage[place as usize];
        row.ancestors_pass = ancestors_pass;
        row.has_passing_ancestor = has_passing_ancestor;
        let is_shown = filter.is_shown(&TreeFilterContext::new(
            link.passes,
            row.has_passing_descendant,
            ancestors_pass,
            has_passing_ancestor,
        ));
        row.nearest_shown = if is_shown { place } else { parent_shown };
        shown += usize::from(is_shown);
        let link = &mut links[place as usize];
        link.shown = is_shown;
        link.parent = parent_shown;
    }
    shown
}

/// The places of the shown rows in the order of the view's sort, ties by
/// insertion. The fading rows at the end of `rows` have the insertion
/// sequences `fading_seqs`.
fn sort_shown<R: GridRow>(
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    rows: &[(RowId, &Arc<R>)],
    links: &[Link],
    fading_seqs: &[u64],
    shown: usize,
    parallelism: Parallelism,
) -> Vec<u32> {
    let live = rows.len() - fading_seqs.len();
    let seq = |place: u32, id: RowId| match (place as usize).checked_sub(live) {
        Some(fading) => fading_seqs[fading],
        None => snapshot.seq(id),
    };
    match spec.sort.split_first() {
        // Storage order is not insertion order once slots are reused.
        None => sorted_places(
            rows,
            links,
            shown,
            parallelism,
            |place, id, _| (seq(place, id), place),
            |a, b| a.0.cmp(&b.0),
            |&(_, place)| place,
        ),
        Some((&(first, first_direction), rest)) => sorted_places(
            rows,
            links,
            shown,
            parallelism,
            |place, id, row| ((row.value(first), seq(place, id), id, row), place),
            move |a, b| compare_sorted(&a.0, &b.0, first_direction, rest),
            |&(_, place)| place,
        ),
    }
}

/// The shown rows ordered by their entries, as their places. `compare` is
/// a total order under which no two rows are equal, so the result does not
/// depend on how the work is split: the flat path's sort, over the shown
/// rows of the array.
fn sorted_places<'a, R: GridRow, E: Copy + Send + Sync>(
    rows: &[(RowId, &'a Arc<R>)],
    links: &[Link],
    shown: usize,
    parallelism: Parallelism,
    entry: impl Fn(u32, RowId, &'a Arc<R>) -> E + Sync,
    compare: impl Fn(&E, &E) -> Ordering + Sync,
    place: impl Fn(&E) -> u32,
) -> Vec<u32> {
    let extend = |entries: &mut Vec<E>, start: usize, end: usize| {
        entries.extend(
            (start..end)
                .filter(|&ix| links[ix].shown)
                .map(|ix| entry(ix as u32, rows[ix].0, rows[ix].1)),
        );
    };
    if parallelism.threads <= 1 {
        let mut entries = Vec::with_capacity(shown);
        extend(&mut entries, 0, rows.len());
        entries.sort_unstable_by(&compare);
        return entries.iter().map(place).collect();
    }

    let next_chunk = AtomicUsize::new(0);
    let sorted_run = || {
        let mut entries = Vec::new();
        loop {
            let start = next_chunk.fetch_add(parallelism.chunk_rows, AtomicOrdering::Relaxed);
            if start >= rows.len() {
                break;
            }
            extend(
                &mut entries,
                start,
                rows.len().min(start + parallelism.chunk_rows),
            );
        }
        entries.sort_unstable_by(&compare);
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
    let mut places = Vec::with_capacity(shown);
    match runs.as_slice() {
        [first, second] => merge(first, second, &compare, place, &mut places),
        runs => places.extend(runs.iter().flatten().map(place)),
    }
    places
}

/// The shown lines: every row whose ancestors are all expanded.
fn lay_out(rows: &[RowId], nodes: &[TreeNode]) -> Vec<DisplayRow> {
    let mut lines = Vec::with_capacity(rows.len());
    let mut order_ix = 0;
    while let (Some(&row), Some(node)) = (rows.get(order_ix), nodes.get(order_ix)) {
        lines.push(DisplayRow::Data {
            row,
            depth: node.depth,
        });
        order_ix += 1;
        if node.children > 0 && !node.expanded {
            order_ix += node.descendants as usize;
        }
    }
    lines
}

/// Drops removed rows from a tree projection without sorting: O(n).
///
/// A removed leaf only leaves its parent with one child less. The children
/// of a removed row become roots, as `parent_key` now names a missing row:
/// each with its subtree, in its old order, right after the subtree of the
/// root that held it. Their depths shrink; every row keeps its expansion
/// flag, and the lines are laid out again. Their order among the roots may
/// not follow the sort: the view rebuilds a tree whose branch was removed,
/// and this is the fallback.
///
/// With [`TreeFilter::KeepAncestors`], a row kept only for its passing
/// descendants goes when the last of them does, as in a rebuild.
///
/// A removed row that fades stays as a leaf where a rebuild puts it: under
/// its parent if that stays, at the root if it was a root. Its children
/// become roots all the same.
pub(super) fn compact<R: GridRow>(
    previous: &Projection,
    snapshot: &StoreSnapshot<R>,
) -> Projection {
    let Some(tree) = previous.tree_data() else {
        return display::compact(previous, snapshot);
    };
    let count = previous.rows.len();
    let live: Vec<bool> = previous
        .rows
        .iter()
        .map(|&id| snapshot.row(id).is_some())
        .collect();
    // A fading row stays if it passes the filters, as in a rebuild.
    let mut kept: Vec<bool> = previous
        .rows
        .iter()
        .zip(&live)
        .zip(&tree.nodes)
        .map(|((&id, &live), node)| live || (node.passes && snapshot.fading_row(id).is_some()))
        .collect();

    // In the old order, parents come first. Each kept row's new parent is
    // its old one if kept and live, its root is where its subtree now
    // starts.
    let mut ancestors: Vec<u32> = Vec::with_capacity(count);
    let mut parents = vec![NONE; count];
    let mut roots = vec![NONE; count];
    let mut depths = vec![0u16; count];
    let mut fading = Vec::new();
    for order_ix in 0..count {
        ancestors.truncate(usize::from(tree.nodes[order_ix].depth));
        let old_parent = ancestors.last().copied().unwrap_or(NONE);
        ancestors.push(order_ix as u32);
        if !kept[order_ix] {
            continue;
        }
        let parent_stays =
            old_parent != NONE && kept[old_parent as usize] && live[old_parent as usize];
        if !live[order_ix] {
            fading.push(order_ix);
            if !is_fading_place(snapshot, previous, order_ix, old_parent, parent_stays) {
                kept[order_ix] = false;
                continue;
            }
        }
        if parent_stays {
            let parent = old_parent as usize;
            parents[order_ix] = old_parent;
            roots[order_ix] = roots[parent];
            depths[order_ix] = depths[parent].saturating_add(1);
        } else {
            roots[order_ix] = order_ix as u32;
        }
    }

    // Children before parents: rows kept for context with no passing live
    // descendant left go (with their subtrees, which hold none either),
    // then the children and descendants of the rest are counted.
    let mut children = vec![0u32; count];
    let mut descendants = vec![0u32; count];
    let mut has_passing_descendant = vec![false; count];
    for order_ix in (0..count).rev() {
        if !kept[order_ix] {
            continue;
        }
        let passes = tree.nodes[order_ix].passes;
        if tree.keeps_ancestors && !passes && !has_passing_descendant[order_ix] {
            kept[order_ix] = false;
            continue;
        }
        let parent = parents[order_ix];
        if parent != NONE {
            let parent = parent as usize;
            children[parent] += 1;
            descendants[parent] += 1 + descendants[order_ix];
            has_passing_descendant[parent] |=
                live[order_ix] && (passes || has_passing_descendant[order_ix]);
        }
    }
    // A fading row whose parent went goes too: it was counted under that
    // parent alone.
    for order_ix in fading {
        let parent = parents[order_ix];
        if parent != NONE && !kept[parent as usize] {
            kept[order_ix] = false;
        }
    }

    // Each root's subtree, in the old order of the roots; within a subtree
    // the old order is a depth-first order still.
    let mut starts = vec![0u32; count];
    let mut next = 0;
    for order_ix in 0..count {
        if kept[order_ix] && roots[order_ix] == order_ix as u32 {
            starts[order_ix] = next;
            next += 1 + descendants[order_ix];
        }
    }
    let mut placed = vec![NONE; next as usize];
    let mut row_count = 0;
    for order_ix in (0..count).filter(|&order_ix| kept[order_ix]) {
        let start = &mut starts[roots[order_ix] as usize];
        placed[*start as usize] = order_ix as u32;
        *start += 1;
        row_count += usize::from(live[order_ix]);
    }
    let rows: Vec<RowId> = placed
        .iter()
        .map(|&order_ix| previous.rows[order_ix as usize])
        .collect();
    let nodes: Vec<TreeNode> = placed
        .iter()
        .map(|&order_ix| {
            let order_ix = order_ix as usize;
            let old = tree.nodes[order_ix];
            TreeNode {
                descendants: descendants[order_ix],
                children: children[order_ix],
                depth: depths[order_ix],
                expanded: old.expanded,
                passes: old.passes,
            }
        })
        .collect();

    let lines = lay_out(&rows, &nodes);
    Projection::with_lines(
        snapshot,
        rows,
        lines,
        BuiltStructure::Tree(TreeData {
            nodes,
            cycles: tree.cycles,
            keeps_ancestors: tree.keeps_ancestors,
        }),
    )
    .with_row_count(row_count)
    .with_totals(previous.totals.clone())
}

/// Returns true if the fading row at `order_ix` of `previous` stays where it
/// is, as a rebuild would place it: under its old parent if that stays and
/// is the row its `parent_key` names, at the root if it was a root without
/// a parent (or naming itself).
fn is_fading_place<R: GridRow>(
    snapshot: &StoreSnapshot<R>,
    previous: &Projection,
    order_ix: usize,
    old_parent: u32,
    parent_stays: bool,
) -> bool {
    let Some((row, _)) = snapshot.fading_row(previous.rows[order_ix]) else {
        return false;
    };
    match row.parent_key() {
        Some(key) if key != row.key() => {
            parent_stays && snapshot.row_id(key) == previous.rows.get(old_parent as usize).copied()
        }
        _ => old_parent == NONE,
    }
}

/// A tree projection with fresh totals and row count from `snapshot`: the
/// same rows, in the same order (the aggregate job).
pub(super) fn refresh<R: GridRow>(
    previous: &Projection,
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
) -> Projection {
    let Some(tree) = previous.tree_data() else {
        return previous.duplicate().with_store_version(snapshot.version());
    };
    let aggregates = ViewAggregates::new(spec);
    let default = TreeTotals::default();
    let rule = spec.tree().map_or(&default, TreeSpec::totals);
    let (totals, live) = tree.totals(&aggregates, rule, &previous.rows, |id| {
        snapshot.row(id).map(|row| &**row)
    });
    previous
        .duplicate()
        .with_store_version(snapshot.version())
        .with_row_count(live)
        .with_totals(AggregateValues::new(aggregates.columns(), totals))
}

/// A tree projection laid out for `spec.expansion()`, by row key: the same
/// rows, depths and branches, with the expansion of each branch read again
/// and the lines laid out again. No sorting, no parent lookups: O(n). A
/// branch removed from `snapshot` since keeps its flag.
pub(super) fn relayout<R: GridRow>(
    previous: &Projection,
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
) -> Projection {
    let Some(tree) = previous.tree_data() else {
        return previous.duplicate();
    };
    let expansion = spec.expansion();
    let mut nodes = tree.nodes.clone();
    for (node, &id) in nodes.iter_mut().zip(&previous.rows) {
        if node.children > 0
            && let Some(row) = snapshot.row(id)
        {
            node.expanded = expansion.is_row_expanded(row.key(), node.depth);
        }
    }
    let lines = lay_out(&previous.rows, &nodes);
    Projection::with_lines(
        snapshot,
        previous.rows.clone(),
        lines,
        BuiltStructure::Tree(TreeData {
            nodes,
            cycles: tree.cycles,
            keeps_ancestors: tree.keeps_ancestors,
        }),
    )
    .with_store_version(previous.store_version)
    .with_row_count(previous.row_count)
    .with_totals(previous.totals.clone())
}

#[cfg(test)]
mod tests {
    use std::{panic, time::Instant};

    use super::{super::ProjectionEngine, super::SortDirection, *};
    use crate::{
        highlight::HighlightPolicy,
        model::{CellValue, ColumnIx, ColumnSet, GridColumns},
        quick_filter::{ContainsAllTerms, QuickFilter},
        store::{Ingest, Op},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Column {
        Group,
        Price,
        Name,
    }

    impl GridColumns for Column {
        fn all() -> &'static [Self] {
            &[Self::Group, Self::Price, Self::Name]
        }

        fn id(self) -> &'static str {
            match self {
                Self::Group => "group",
                Self::Price => "price",
                Self::Name => "name",
            }
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self as u16)
        }
    }

    /// Few distinct values per column, so sort keys tie often.
    struct Node {
        id: u32,
        parent: Option<u32>,
        group: i64,
        price: f64,
        name: &'static str,
    }

    impl Node {
        /// A row of a random forest of `count` keys: roots, children of
        /// earlier keys, self-parents, orphans and any key (cycles).
        fn random(rng: &mut fastrand::Rng, id: u32, count: u32) -> Self {
            let parent = match rng.u8(0..20) {
                0..=2 => None,
                3 => Some(id),
                4 => Some(count + rng.u32(0..10)),
                5 | 6 => Some(rng.u32(0..count.max(1))),
                _ => Some(rng.u32(0..id.max(1))),
            };
            Self {
                id,
                parent,
                group: rng.i64(0..4),
                price: [0.5, 1.0, f64::NAN, -0.0][rng.usize(..4)],
                name: ["", "a", "ab", "b", "я"][rng.usize(..5)],
            }
        }
    }

    impl GridRow for Node {
        type Key = u32;
        type Column = Column;

        fn key(&self) -> &u32 {
            &self.id
        }

        fn value(&self, column: Column) -> CellValue<'_> {
            match column {
                Column::Group => CellValue::Int(self.group),
                Column::Price => CellValue::Float(self.price),
                Column::Name => CellValue::Text(self.name),
            }
        }

        fn parent_key(&self) -> Option<&u32> {
            assert_ne!(self.id, u32::MAX, "parent_key failed");
            self.parent.as_ref()
        }
    }

    /// A forest of `count` rows written in a random order, with some removed
    /// and written again, so slots are reused and children often come
    /// before their parents. Removed rows fade with `fade`.
    fn random_snapshot(
        rng: &mut fastrand::Rng,
        count: u32,
        fade: bool,
    ) -> Arc<StoreSnapshot<Node>> {
        let epoch = Instant::now();
        let policy = if fade {
            HighlightPolicy::default().with_removal_fade(std::time::Duration::from_secs(3_600))
        } else {
            HighlightPolicy::default()
        };
        let mut ingest = Ingest::<Node>::new(policy, epoch);
        let mut ids: Vec<u32> = (0..count).collect();
        rng.shuffle(&mut ids);
        ingest.apply(
            ids.iter()
                .map(|&id| Op::UpsertSilent(Arc::new(Node::random(rng, id, count)))),
            epoch,
        );
        ingest.apply(
            (0..count / 4).map(|_| Op::Remove(rng.u32(0..count.max(1)))),
            epoch,
        );
        ingest.apply(
            (0..count / 4).map(|_| {
                let id = rng.u32(0..count.max(1));
                Op::UpsertSilent(Arc::new(Node::random(rng, id, count)))
            }),
            epoch,
        );
        ingest.snapshot()
    }

    /// A rule that shows rows that pass and their whole subtrees.
    struct ShowsSubtree;

    impl TreeFilterRule for ShowsSubtree {
        fn is_shown(&self, row: &TreeFilterContext) -> bool {
            row.passes() || row.has_passing_descendant() || row.has_passing_ancestor()
        }
    }

    fn random_spec(rng: &mut fastrand::Rng) -> ViewSpec<Node> {
        let columns = Column::all();
        let direction = |rng: &mut fastrand::Rng| {
            [SortDirection::Ascending, SortDirection::Descending][rng.usize(..2)]
        };
        let sort: Vec<_> = (0..rng.usize(0..=2))
            .map(|_| (columns[rng.usize(..columns.len())], direction(rng)))
            .collect();
        let filter = [
            TreeFilter::KeepAncestors,
            TreeFilter::HideSubtree,
            TreeFilter::Custom(Arc::new(ShowsSubtree)),
        ][rng.usize(..3)]
        .clone();
        let mut name = ColumnSet::EMPTY;
        name.insert(Column::Name.ix());
        let text = ["", "a", "b", "я"][rng.usize(..4)];
        let depth = [
            ExpandDepth::All,
            ExpandDepth::Levels(0),
            ExpandDepth::Levels(1),
            ExpandDepth::Levels(2),
        ][rng.usize(..4)];
        let toggled: Vec<u32> = (0..rng.usize(0..8)).map(|_| rng.u32(0..300)).collect();
        let spec = ViewSpec::new()
            .with_sort(sort)
            .with_quick_filter(QuickFilter::new(text, name, &ContainsAllTerms))
            .with_tree(TreeSpec::new().with_filter(filter))
            .with_tree_expansion(depth, toggled);
        if rng.bool() {
            spec.with_filter(ColumnSet::first(columns.len()), |node: &Node| {
                node.group != 1
            })
        } else {
            spec
        }
    }

    /// Everything a tree projection shows, to compare two of them.
    type Shape = (
        Vec<RowId>,
        Vec<DisplayRow>,
        Vec<Option<Branch>>,
        usize,
        usize,
    );

    fn shape(projection: &Projection) -> Shape {
        (
            projection.rows().to_vec(),
            (0..projection.len())
                .filter_map(|ix| projection.line(ix))
                .collect(),
            projection
                .rows()
                .iter()
                .map(|&id| projection.branch(id))
                .collect(),
            projection.tree_cycles(),
            projection.row_count(),
        )
    }

    #[test]
    fn test_parallel_tree_matches_sequential() {
        // Small chunks and odd thread counts put chunk and run boundaries
        // everywhere: the tree must not depend on them. Removed rows fade
        // in odd seeds.
        let mut fading = 0;
        for seed in 0..40 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let count = [0, 1, 2, 7, 100, 1_000, 3_000][rng.usize(..7)];
            let snapshot = random_snapshot(&mut rng, count, seed % 2 == 1);
            for _ in 0..6 {
                let spec = random_spec(&mut rng);
                let Structure::Tree(tree) = spec.structure() else {
                    unreachable!("a tree view")
                };
                let sequential = build(&snapshot, &spec, tree, parallelism(0, 1));
                let parallelism = Parallelism {
                    threads: rng.usize(2..=7),
                    chunk_rows: rng.usize(1..=97),
                };
                let parallel = build(&snapshot, &spec, tree, parallelism);
                assert!(parallel.is_tree());
                assert_eq!(
                    shape(&parallel),
                    shape(&sequential),
                    "seed {seed}, {count} rows, {parallelism:?}, sort {:?}, {:?}",
                    spec.sort(),
                    tree.filter(),
                );
                fading += parallel.rows().len() - parallel.row_count();
            }
        }
        assert!(fading > 100, "{fading} fading rows");
    }

    /// A rule that fails on a row with a single shown descendant.
    struct Failing;

    impl TreeFilterRule for Failing {
        fn is_shown(&self, row: &TreeFilterContext) -> bool {
            assert!(row.ancestors_pass(), "tree filter failed");
            row.passes()
        }
    }

    #[test]
    fn test_tree_panic_resumes() {
        // A panicking `parent_key` on a worker thread and a panicking rule
        // fail the rebuild on the calling thread with their own messages.
        let epoch = Instant::now();
        let mut rng = fastrand::Rng::with_seed(5);
        let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), epoch);
        ingest.apply(
            (0..2_000).map(|id| Op::UpsertSilent(Arc::new(Node::random(&mut rng, id, 2_000)))),
            epoch,
        );
        let mut failing = Node::random(&mut rng, 0, 1);
        failing.id = u32::MAX;
        ingest.apply([Op::UpsertSilent(Arc::new(failing))], epoch);
        let snapshot = ingest.snapshot();
        let parallelism = Parallelism {
            threads: 4,
            chunk_rows: 16,
        };
        let message = |panic: Box<dyn std::any::Any + Send>| {
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                .unwrap_or_default()
        };
        let spec = ViewSpec::new().with_tree(TreeSpec::new());
        let panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            build(&snapshot, &spec, &TreeSpec::new(), parallelism)
        }))
        .expect_err("parent_key panics");
        assert!(message(panic).contains("parent_key failed"));

        let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), epoch);
        ingest.apply(
            (0..2_000).map(|id| {
                Op::UpsertSilent(Arc::new(Node {
                    id,
                    parent: id.checked_sub(1),
                    group: 0,
                    price: 1.0,
                    name: "a",
                }))
            }),
            epoch,
        );
        let snapshot = ingest.snapshot();
        let tree = TreeSpec::new().with_filter(TreeFilter::Custom(Arc::new(Failing)));
        let spec = ViewSpec::new()
            .with_filter(ColumnSet::first(3), |node: &Node| node.id % 2 == 0)
            .with_tree(tree.clone());
        let panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            build(&snapshot, &spec, &tree, parallelism)
        }))
        .expect_err("the rule panics");
        assert!(message(panic).contains("tree filter failed"));
    }

    #[test]
    fn test_cycle_warning_once() {
        // The warning is written when the count of rows in cycles changes,
        // not on every rebuild, and not for a tree without cycles.
        let last = AtomicUsize::new(0);
        let warnings: Vec<bool> = [0, 2, 2, 2, 3, 0, 0, 3, 3]
            .into_iter()
            .map(|cycles| is_new_cycle_count(&last, cycles))
            .collect();
        assert_eq!(
            warnings,
            [false, true, false, false, true, false, false, true, false]
        );
    }

    /// A rule that counts the roots alone.
    struct Roots;

    impl TreeTotalsRule for Roots {
        fn is_counted(&self, row: &TreeTotalsContext) -> bool {
            row.depth() == 0
        }
    }

    #[test]
    fn test_tree_totals_rows() {
        // 0 ─┬─ 1 ─── 3 "b"
        //    └─ 2 "b"
        // 4 "b"
        // 5 ───── 6
        let epoch = Instant::now();
        let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), epoch);
        let node = |id, parent, name| {
            Op::UpsertSilent(Arc::new(Node {
                id,
                parent,
                group: 0,
                price: 1.0,
                name,
            }))
        };
        ingest.apply(
            [
                node(0, None, ""),
                node(1, Some(0), ""),
                node(2, Some(0), "b"),
                node(3, Some(1), "b"),
                node(4, None, "b"),
                node(5, None, ""),
                node(6, Some(5), ""),
            ],
            epoch,
        );
        let snapshot = ingest.snapshot();
        let keys = |projection: &Projection, totals: &TreeTotals| -> Vec<u32> {
            let tree = projection.tree_data().expect("a tree");
            let live: Vec<Option<&Node>> = projection
                .rows()
                .iter()
                .map(|&id| snapshot.row(id).map(|row| &**row))
                .collect();
            tree.counted(totals, &live).map(|row| row.id).collect()
        };
        let engine = Rebuild::default();
        let all = engine.rebuild(&snapshot, &ViewSpec::new().with_tree(TreeSpec::new()));
        assert_eq!(keys(&all, &TreeTotals::Leaves), [3, 2, 4, 6]);
        assert_eq!(keys(&all, &TreeTotals::AllRows), [0, 1, 3, 2, 4, 5, 6]);
        assert_eq!(keys(&all, &TreeTotals::Custom(Arc::new(Roots))), [0, 4, 5]);

        // Filtered: a row whose matches are all filtered out is a leaf, and
        // a rule tells the rows kept for context.
        let mut name = ColumnSet::EMPTY;
        name.insert(Column::Name.ix());
        let filtered = engine.rebuild(
            &snapshot,
            &ViewSpec::new()
                .with_quick_filter(QuickFilter::new("b", name, &ContainsAllTerms))
                .with_tree(TreeSpec::new()),
        );
        assert_eq!(keys(&filtered, &TreeTotals::Leaves), [3, 2, 4]);
        struct Passing;
        impl TreeTotalsRule for Passing {
            fn is_counted(&self, row: &TreeTotalsContext) -> bool {
                row.passes() && !row.is_branch()
            }
        }
        assert_eq!(
            keys(&filtered, &TreeTotals::Custom(Arc::new(Passing))),
            [3, 2, 4]
        );
        assert_eq!(keys(&filtered, &TreeTotals::AllRows), [0, 1, 3, 2, 4]);
        // The depth of every row, for the export of everything.
        let tree = filtered.tree_data().unwrap();
        let depths: Vec<u16> = (0..filtered.rows().len())
            .map(|ix| tree.depth(ix))
            .collect();
        assert_eq!(depths, [0, 1, 2, 1, 0]);
    }
}
