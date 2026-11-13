//! Grouping: the levels of a view, the order of sibling groups, group
//! footers and the grouped rebuild. No GPUI types.

use std::{array, cmp::Ordering, fmt, iter, mem, ops::Range, sync::Arc};

use super::{
    AggregateRow, FadingIds, GroupInfo, GroupPath, Parallelism, Projection, Rebuild, SortDirection,
    Structure, ViewSpec,
    aggregate::{GroupValues, KeyedGroup, ViewAggregates, aggregate_groups},
    directed,
    display::{AggregateValues, Group, GroupData, GroupId},
    sorted_rows,
};
use crate::{
    model::{CellValue, ColumnId, ColumnIx, ColumnSet, GridColumns, GridRow, OwnedValue},
    store::{RowId, StoreSnapshot},
};

/// The key function of a [`GroupKey`].
type KeyFn<R> = Arc<dyn Fn(&R) -> CellValue<'_> + Send + Sync>;

/// Grouping by a function of the row rather than a column: a bucket of
/// prices, the first letter of a name.
pub struct GroupKey<R: GridRow> {
    id: &'static str,
    title: Arc<str>,
    depends_on: ColumnSet,
    key: KeyFn<R>,
}

impl<R: GridRow> Clone for GroupKey<R> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            title: self.title.clone(),
            depends_on: self.depends_on,
            key: self.key.clone(),
        }
    }
}

impl<R: GridRow> fmt::Debug for GroupKey<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GroupKey")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("depends_on", &self.depends_on)
            .finish_non_exhaustive()
    }
}

impl<R: GridRow> GroupKey<R> {
    /// A level keyed by `key`.
    ///
    /// `id` names the level in group paths and must differ from every
    /// column id of the schema. `depends_on` lists the columns `key` reads;
    /// pass every column when unsure. `key` runs in the background, from
    /// several threads of a large rebuild, once or twice per row: it must
    /// not allocate, so it returns text borrowed from the row.
    pub fn new(
        id: &'static str,
        depends_on: ColumnSet,
        key: impl Fn(&R) -> CellValue<'_> + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            title: id.into(),
            depends_on,
            key: Arc::new(key),
        }
    }

    /// The level's title in captions and the group panel; the id by
    /// default.
    pub fn with_title(mut self, title: impl Into<Arc<str>>) -> Self {
        self.title = title.into();
        self
    }

    /// The level's id.
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// The level's title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The columns the key reads.
    pub fn depends_on(&self) -> ColumnSet {
        self.depends_on
    }

    /// The key of a row.
    pub(crate) fn value<'r>(&self, row: &'r R) -> CellValue<'r> {
        (self.key)(row)
    }
}

/// What a level groups by.
enum GroupLevel<R: GridRow> {
    Column(R::Column),
    Key(GroupKey<R>),
}

impl<R: GridRow> Clone for GroupLevel<R> {
    fn clone(&self) -> Self {
        match self {
            Self::Column(column) => Self::Column(*column),
            Self::Key(key) => Self::Key(key.clone()),
        }
    }
}

/// One level of a grouping: a column or a [`GroupKey`], and the order of
/// its groups by key.
pub struct GroupBy<R: GridRow> {
    level: GroupLevel<R>,
    direction: SortDirection,
}

impl<R: GridRow> Clone for GroupBy<R> {
    fn clone(&self) -> Self {
        Self {
            level: self.level.clone(),
            direction: self.direction,
        }
    }
}

impl<R: GridRow> fmt::Debug for GroupBy<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("GroupBy");
        match &self.level {
            GroupLevel::Column(column) => debug.field("column", column),
            GroupLevel::Key(key) => debug.field("key", key),
        };
        debug.field("direction", &self.direction).finish()
    }
}

impl<R: GridRow> GroupBy<R> {
    /// Groups by a column's value, ascending.
    pub fn column(column: R::Column) -> Self {
        Self {
            level: GroupLevel::Column(column),
            direction: SortDirection::Ascending,
        }
    }

    /// Groups by a function of the row, ascending.
    pub fn key(key: GroupKey<R>) -> Self {
        Self {
            level: GroupLevel::Key(key),
            direction: SortDirection::Ascending,
        }
    }

    /// The order of this level's groups by key.
    pub fn with_direction(mut self, direction: SortDirection) -> Self {
        self.direction = direction;
        self
    }

    /// The order of this level's groups by key.
    pub fn direction(&self) -> SortDirection {
        self.direction
    }

    /// The column, for a column level.
    pub fn as_column(&self) -> Option<R::Column> {
        match &self.level {
            GroupLevel::Column(column) => Some(*column),
            GroupLevel::Key(_) => None,
        }
    }

    /// The key, for a level by a function of the row.
    pub fn as_key(&self) -> Option<&GroupKey<R>> {
        match &self.level {
            GroupLevel::Column(_) => None,
            GroupLevel::Key(key) => Some(key),
        }
    }

    /// The level's id in a [`GroupPath`]: the column's id, or the key's.
    pub fn id(&self) -> ColumnId {
        match &self.level {
            GroupLevel::Column(column) => column.column_id(),
            GroupLevel::Key(key) => ColumnId::new_static(key.id),
        }
    }

    /// The level's key of a row.
    pub(crate) fn value<'r>(&self, row: &'r R) -> CellValue<'r> {
        match &self.level {
            GroupLevel::Column(column) => row.value(*column),
            GroupLevel::Key(key) => key.value(row),
        }
    }
}

/// The grouping of a view: its levels, the top one first.
pub(crate) struct GroupSpec<R: GridRow> {
    levels: Arc<[GroupBy<R>]>,
}

impl<R: GridRow> Clone for GroupSpec<R> {
    fn clone(&self) -> Self {
        Self {
            levels: self.levels.clone(),
        }
    }
}

impl<R: GridRow> GroupSpec<R> {
    /// The levels, the top one first.
    pub(crate) fn levels(&self) -> &[GroupBy<R>] {
        &self.levels
    }
}

/// What [`GroupOrder`] knows about a group when it orders siblings.
pub struct GroupOrderContext<'a> {
    path: &'a GroupPath,
    row_count: usize,
    aggregates: AggregateRow<'a>,
}

impl<'a> GroupOrderContext<'a> {
    /// The group at `path`, with its live rows and aggregates.
    pub(crate) fn new(path: &'a GroupPath, row_count: usize, aggregates: AggregateRow<'a>) -> Self {
        Self {
            path,
            row_count,
            aggregates,
        }
    }

    /// The group's identity.
    pub fn path(&self) -> &'a GroupPath {
        self.path
    }

    /// The group's key value.
    pub fn value(&self) -> &'a OwnedValue {
        self.path
            .value(self.path.depth())
            .expect("a group's path is not the root")
    }

    /// The group's depth: 0 for a top-level group.
    pub fn depth(&self) -> u16 {
        self.path.depth()
    }

    /// The group's live rows.
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// The group's aggregate of a column, if the view aggregates it.
    pub fn aggregate(&self, column: ColumnIx) -> Option<&'a OwnedValue> {
        self.aggregates.get(column)
    }
}

/// The order of sibling groups. Groups are first in the order of their
/// keys, each level in its direction; `compare` sorts siblings stably from
/// there, so `Equal` keeps the key order.
///
/// Called in the background, from several threads of a large rebuild, once
/// per comparison of two groups.
pub trait GroupOrder<R: GridRow>: Send + Sync + 'static {
    /// Compares two sibling groups.
    fn compare(&self, a: &GroupOrderContext<'_>, b: &GroupOrderContext<'_>) -> Ordering;

    /// The columns whose changes can reorder groups: the order's
    /// aggregates. Empty for an order that reads keys alone.
    fn depends_on(&self) -> ColumnSet {
        ColumnSet::EMPTY
    }
}

/// Groups in the order of their keys, each level in its direction: the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyOrder;

impl<R: GridRow> GroupOrder<R> for KeyOrder {
    fn compare(&self, _: &GroupOrderContext<'_>, _: &GroupOrderContext<'_>) -> Ordering {
        Ordering::Equal
    }
}

/// Groups by the aggregate of a column: the column needs an aggregate in
/// the view. Ties, and groups without the aggregate, keep the key order.
#[derive(Clone, Copy, Debug)]
pub struct AggregateOrder {
    column: ColumnIx,
    direction: SortDirection,
}

impl AggregateOrder {
    /// Orders groups by their aggregate of `column`.
    pub fn new(column: impl GridColumns, direction: SortDirection) -> Self {
        Self {
            column: column.ix(),
            direction,
        }
    }

    /// The column whose aggregate orders the groups.
    pub fn column(&self) -> ColumnIx {
        self.column
    }

    /// The direction.
    pub fn direction(&self) -> SortDirection {
        self.direction
    }
}

impl<R: GridRow> GroupOrder<R> for AggregateOrder {
    fn compare(&self, a: &GroupOrderContext<'_>, b: &GroupOrderContext<'_>) -> Ordering {
        match (a.aggregate(self.column), b.aggregate(self.column)) {
            (Some(a), Some(b)) => directed(a.cmp_total(b), self.direction),
            _ => Ordering::Equal,
        }
    }

    fn depends_on(&self) -> ColumnSet {
        let mut columns = ColumnSet::EMPTY;
        columns.insert(self.column);
        columns
    }
}

/// Which groups have a footer row under their rows. Called in the
/// background, once per group.
pub trait GroupFooter: Send + Sync + 'static {
    /// Returns true if `group` has a footer.
    fn has_footer(&self, group: &GroupInfo<'_>) -> bool;
}

/// No group has a footer: the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGroupFooter;

impl GroupFooter for NoGroupFooter {
    fn has_footer(&self, _: &GroupInfo<'_>) -> bool {
        false
    }
}

/// Every group has a footer.
#[derive(Clone, Copy, Debug, Default)]
pub struct EveryGroupFooter;

impl GroupFooter for EveryGroupFooter {
    fn has_footer(&self, _: &GroupInfo<'_>) -> bool {
        true
    }
}

impl<R: GridRow> ViewSpec<R> {
    /// Groups the rows by these levels, the first one outermost. Repeated
    /// level ids are dropped, the first one stays; an empty list turns
    /// grouping off. Replaces a tree, as in AG Grid and DevExpress: the last
    /// of `with_group_by` and [`Self::with_tree`] wins.
    pub fn with_group_by(mut self, levels: impl IntoIterator<Item = GroupBy<R>>) -> Self {
        let mut unique: Vec<GroupBy<R>> = Vec::new();
        for level in levels {
            debug_assert!(
                level
                    .as_key()
                    .is_none_or(|key| R::Column::all().iter().all(|column| column.id() != key.id)),
                "the group key id {:?} is a column id",
                level.id()
            );
            if !unique.iter().any(|kept| kept.id() == level.id()) {
                unique.push(level);
            }
        }
        self.structure = if unique.is_empty() {
            Structure::Flat
        } else {
            Structure::Groups(GroupSpec {
                levels: unique.into(),
            })
        };
        self
    }

    /// The grouping levels, the top one first; empty without grouping.
    pub fn group_by(&self) -> &[GroupBy<R>] {
        match self.structure() {
            Structure::Groups(groups) => groups.levels(),
            Structure::Flat | Structure::Tree(_) => &[],
        }
    }

    /// Sets the order of sibling groups (default [`KeyOrder`]).
    pub fn with_group_order(mut self, order: impl GroupOrder<R>) -> Self {
        self.group_order = Some(Arc::new(order));
        self
    }

    /// The order of sibling groups.
    pub fn group_order(&self) -> &dyn GroupOrder<R> {
        match &self.group_order {
            Some(order) => &**order,
            None => &KeyOrder,
        }
    }

    /// Sets which groups have a footer (default [`NoGroupFooter`]).
    pub fn with_group_footer(mut self, footer: impl GroupFooter) -> Self {
        self.group_footer = Some(Arc::new(footer));
        self
    }

    /// Which groups have a footer.
    pub(crate) fn group_footer(&self) -> &dyn GroupFooter {
        match &self.group_footer {
            Some(footer) => &**footer,
            None => &NoGroupFooter,
        }
    }
}

/// Builds a grouped view, splitting the sort across threads as a flat
/// rebuild does.
pub(super) fn rebuild<R: GridRow>(
    engine: &Rebuild,
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    grouping: &GroupSpec<R>,
) -> Projection {
    let parallelism = Parallelism::for_rows(snapshot.len(), engine.max_threads());
    rebuild_with(snapshot, spec, grouping, parallelism)
}

/// [`rebuild`] with the work split as given.
///
/// 1. Filter and sort, as a flat rebuild: by the levels' keys, each in its
///    direction, then the view's sort, then insertion. Fading rows take
///    their places by their last values; those of a group of the last level
///    without a live row go, as a fading row makes no group.
/// 2. Find the groups in one pass over the sorted entries, by the keys
///    they cache: a group ends where a key of its level or above changes.
/// 3. Aggregate the live rows from the bottom up, in key order, and total.
/// 4. Sort the siblings of each level by the group order, stably, and lay
///    the rows out again so each group's rows follow each other.
/// 5. Ask the group footer about each group; lay the lines out by the
///    expansion.
///
/// Allocates per group and per job, not per row: keys are borrowed from
/// the snapshot's rows.
pub(super) fn rebuild_with<R: GridRow>(
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    grouping: &GroupSpec<R>,
    parallelism: Parallelism,
) -> Projection {
    let levels = grouping.levels();
    let keys = sort_keys(spec, levels);
    // As many keys cached as the view has, up to `MAX_CACHED_KEYS`; never
    // fewer than two, so small views share one instance.
    match keys.len() {
        0..=2 => build::<R, 2>(snapshot, spec, levels, &keys, parallelism),
        3 => build::<R, 3>(snapshot, spec, levels, &keys, parallelism),
        _ => build::<R, MAX_CACHED_KEYS>(snapshot, spec, levels, &keys, parallelism),
    }
}

/// [`rebuild_with`] with `N` keys cached in each sort entry.
fn build<R: GridRow, const N: usize>(
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    levels: &[GroupBy<R>],
    keys: &[SortKey<'_, R>],
    parallelism: Parallelism,
) -> Projection {
    let level_count = levels.len() as u16;
    let (mut sorted, fading) = sort_rows::<R, N>(snapshot, spec, keys, parallelism);
    let fading = if fading > 0 {
        FadingIds::new(snapshot)
    } else {
        FadingIds::default()
    };
    let mut table = find_groups(levels, &sorted);
    if !fading.is_empty() && drop_fading_groups(&mut sorted, &table, level_count, &fading) {
        table = find_groups(levels, &sorted);
    }

    let aggregates = ViewAggregates::new(spec);
    #[cfg(debug_assertions)]
    warn_unaggregated_order(spec, aggregates.columns());
    let mut computed =
        aggregate_groups(&aggregates, &table.groups, level_count, &sorted, |entry| {
            (!fading.contains(entry.2)).then_some(&**entry.3)
        });
    let columns = aggregates.columns();
    let width = columns.len();
    let order = order_groups(spec.group_order(), &table, &computed, columns);

    // The groups in caption order. A group's rows start where the rows laid
    // out so far end: its first descendant of the last level comes next.
    let expansion = spec.expansion();
    let mut caption_ids = vec![0u32; order.len()];
    let mut groups = Vec::with_capacity(order.len());
    let mut values = Vec::with_capacity(order.len() * width);
    let mut rows = Vec::with_capacity(sorted.len());
    for (id, &key_ix) in order.iter().enumerate() {
        let key_ix = key_ix as usize;
        caption_ids[key_ix] = id as u32;
        let keyed = &table.groups[key_ix];
        let start = rows.len() as u32;
        let len = keyed.rows.end - keyed.rows.start;
        if usize::from(keyed.depth) + 1 == levels.len() {
            rows.extend(
                sorted[keyed.rows.start as usize..keyed.rows.end as usize]
                    .iter()
                    .map(|entry| entry.2),
            );
        }
        let path = mem::take(&mut table.paths[key_ix]);
        let expanded = expansion.is_group_expanded(&path, levels.len());
        groups.push(Group {
            parent: table.parents[key_ix].map(|parent| GroupId::new(caption_ids[parent] as usize)),
            // A subtree keeps its size when siblings move.
            end: (id + keyed.end as usize - key_ix) as u32,
            key_ix: key_ix as u32,
            rows: start..start + len,
            live: computed.live[key_ix],
            line: u32::MAX,
            expanded,
            footer: false,
            path,
        });
        values.extend(
            computed.values[key_ix * width..(key_ix + 1) * width]
                .iter_mut()
                .map(|value| mem::replace(value, OwnedValue::Empty)),
        );
    }

    let mut groups = GroupData::new(level_count, groups, AggregateValues::new(columns, values));
    let footer = spec.group_footer();
    let footers: Vec<bool> = groups
        .table(&rows)
        .iter()
        .map(|group| footer.has_footer(&group))
        .collect();
    for (group, footer) in groups.groups.iter_mut().zip(footers) {
        group.footer = footer;
    }
    Projection::with_groups(snapshot, rows, groups)
        .with_row_count(computed.total_live)
        .with_totals(AggregateValues::new(columns, computed.totals))
}

/// The most keys a sort entry caches: the levels' keys first, then the
/// view's sort keys. The rest are read only when the cached ones tie, as
/// later sort keys of a flat rebuild are; reading one takes a row from
/// memory, which costs more than the larger entry (S16b of `spike_bench`:
/// two levels and a sort key).
const MAX_CACHED_KEYS: usize = 4;

/// A key of the grouped sort.
struct SortKey<'s, R: GridRow> {
    source: KeySource<'s, R>,
    direction: SortDirection,
}

enum KeySource<'s, R: GridRow> {
    Level(&'s GroupBy<R>),
    Column(R::Column),
}

impl<R: GridRow> SortKey<'_, R> {
    #[inline]
    fn value<'r>(&self, row: &'r R) -> CellValue<'r> {
        match &self.source {
            KeySource::Level(level) => level.value(row),
            KeySource::Column(column) => row.value(*column),
        }
    }
}

/// The keys of a grouped sort: the levels' keys, each in its direction,
/// then the view's sort.
fn sort_keys<'s, R: GridRow>(
    spec: &'s ViewSpec<R>,
    levels: &'s [GroupBy<R>],
) -> Vec<SortKey<'s, R>> {
    levels
        .iter()
        .map(|level| SortKey {
            source: KeySource::Level(level),
            direction: level.direction(),
        })
        .chain(spec.sort().iter().map(|&(column, direction)| SortKey {
            source: KeySource::Column(column),
            direction,
        }))
        .collect()
}

/// A row of the grouped sort: its first `N` keys (`Empty` past the last
/// key), insertion sequence, id and row. The levels' keys come first, so
/// the groups are found from the entries without reading the rows again.
type GroupEntry<'a, R, const N: usize> = ([CellValue<'a>; N], u64, RowId, &'a Arc<R>);

/// Compares by the keys, then by insertion; see [`super::compare_sorted`].
#[inline(always)]
fn compare_grouped<R: GridRow, const N: usize>(
    a: &GroupEntry<'_, R, N>,
    b: &GroupEntry<'_, R, N>,
    cached: &[SortDirection; N],
    rest: &[SortKey<'_, R>],
) -> Ordering {
    for ix in 0..N {
        let ordering = directed(a.0[ix].cmp_total(&b.0[ix]), cached[ix]);
        if ordering.is_ne() {
            return ordering;
        }
    }
    rest.iter()
        .map(|key| directed(key.value(a.3).cmp_total(&key.value(b.3)), key.direction))
        .find(|ordering| ordering.is_ne())
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.1.cmp(&b.1))
}

/// The visible rows of `snapshot` in key order, by `keys` and insertion,
/// with the number of fading rows among them.
fn sort_rows<'a, R: GridRow, const N: usize>(
    snapshot: &'a StoreSnapshot<R>,
    spec: &ViewSpec<R>,
    keys: &[SortKey<'_, R>],
    parallelism: Parallelism,
) -> (Vec<GroupEntry<'a, R, N>>, usize) {
    let cached: [SortDirection; N] = array::from_fn(|ix| {
        keys.get(ix)
            .map_or(SortDirection::Ascending, |key| key.direction)
    });
    let rest = keys.get(N..).unwrap_or_default();
    sorted_rows(
        snapshot,
        spec,
        parallelism,
        // A missing key is `Empty` in every row: it ties.
        |id, row, seq| {
            let keys =
                array::from_fn(|ix| keys.get(ix).map_or(CellValue::Empty, |key| key.value(row)));
            (keys, seq, id, row)
        },
        |a, b| compare_grouped(a, b, &cached, rest),
        |entry| *entry,
    )
}

/// Drops the fading rows of the groups of the last level in `table` that
/// have no live row, keeping the order: a fading row makes no group, or it
/// would go at once. Returns true if it dropped any: the groups are to be
/// found again. Usually the first row of a group is live, so this is O(groups).
fn drop_fading_groups<R: GridRow, const N: usize>(
    sorted: &mut Vec<GroupEntry<'_, R, N>>,
    table: &KeyTable,
    levels: u16,
    fading: &FadingIds,
) -> bool {
    let is_fading = |group: &KeyedGroup| {
        usize::from(group.depth) + 1 == usize::from(levels)
            && sorted[group.rows.start as usize..group.rows.end as usize]
                .iter()
                .all(|entry| fading.contains(entry.2))
    };
    if !table.groups.iter().any(is_fading) {
        return false;
    }
    // The groups of the last level cover the rows in order.
    let dropped: Vec<Range<usize>> = table
        .groups
        .iter()
        .filter(|group| is_fading(group))
        .map(|group| group.rows.start as usize..group.rows.end as usize)
        .collect();
    let mut kept = 0;
    let mut next = 0;
    for range in dropped.into_iter().chain([sorted.len()..sorted.len()]) {
        sorted.copy_within(next..range.start, kept);
        kept += range.start - next;
        next = range.end;
    }
    sorted.truncate(kept);
    true
}

/// The groups of sorted rows, in key order: a group, then its subgroups,
/// siblings in the order of their keys.
#[derive(Default)]
struct KeyTable {
    groups: Vec<KeyedGroup>,
    paths: Vec<GroupPath>,
    /// The enclosing group, by place in `groups`.
    parents: Vec<Option<usize>>,
}

/// Finds the groups of `sorted` in one pass: on each row, the first level
/// whose key differs from the previous row's (by `cmp_total`) closes the
/// groups of that level and below and opens new ones. A group's key is its
/// first row's: `Int(1)` and `Float(1.0)` are one group.
fn find_groups<'a, R: GridRow, const N: usize>(
    levels: &[GroupBy<R>],
    sorted: &[GroupEntry<'a, R, N>],
) -> KeyTable {
    let ids: Vec<ColumnId> = levels.iter().map(GroupBy::id).collect();
    let mut table = KeyTable::default();
    let mut previous: Vec<CellValue<'a>> = Vec::with_capacity(levels.len());
    let mut keys: Vec<CellValue<'a>> = Vec::with_capacity(levels.len());
    // The open group of each level down to the current row's.
    let mut open: Vec<usize> = Vec::with_capacity(levels.len());
    let close = |table: &mut KeyTable, open: &mut Vec<usize>, depth: usize, row_ix: usize| {
        while open.len() > depth {
            let group = open.pop().expect("open groups");
            table.groups[group].end = table.groups.len() as u32;
            table.groups[group].rows.end = row_ix as u32;
        }
    };
    for (row_ix, entry) in sorted.iter().enumerate() {
        // The first levels' keys are cached in the entry.
        keys.clear();
        keys.extend(
            levels
                .iter()
                .enumerate()
                .map(|(depth, level)| match entry.0.get(depth) {
                    Some(&key) => key,
                    None => level.value(entry.3),
                }),
        );
        let changed = if row_ix == 0 {
            0
        } else {
            keys.iter()
                .zip(&previous)
                .position(|(key, previous)| key.cmp_total(previous).is_ne())
                .unwrap_or(levels.len())
        };
        close(&mut table, &mut open, changed, row_ix);
        for depth in changed..levels.len() {
            let parent = open.last().copied();
            let level = (ids[depth].clone(), OwnedValue::from_cell(keys[depth]));
            let path = match parent {
                Some(parent) => GroupPath::new(
                    table.paths[parent]
                        .levels()
                        .iter()
                        .cloned()
                        .chain(iter::once(level)),
                ),
                None => GroupPath::new([level]),
            };
            open.push(table.groups.len());
            table.groups.push(KeyedGroup {
                depth: depth as u16,
                end: 0,
                rows: row_ix as u32..row_ix as u32,
            });
            table.paths.push(path);
            table.parents.push(parent);
        }
        mem::swap(&mut previous, &mut keys);
    }
    close(&mut table, &mut open, 0, sorted.len());
    table
}

/// The groups in caption order, as places in `table`: the siblings of each
/// level sorted by `order`, stably, so ties keep the key order.
fn order_groups<R: GridRow>(
    order: &dyn GroupOrder<R>,
    table: &KeyTable,
    computed: &GroupValues,
    columns: ColumnSet,
) -> Vec<u32> {
    let width = columns.len();
    let context = |ix: u32| {
        let ix = ix as usize;
        let live = computed.live[ix] as usize;
        GroupOrderContext::new(
            &table.paths[ix],
            live,
            AggregateRow::new(
                columns,
                &computed.values[ix * width..(ix + 1) * width],
                live,
            ),
        )
    };
    let compare = |a: u32, b: u32| order.compare(&context(a), &context(b));
    let mut ordered = Vec::with_capacity(table.groups.len());
    let mut siblings = Vec::new();
    order_siblings(
        &table.groups,
        0..table.groups.len(),
        0,
        &compare,
        &mut siblings,
        &mut ordered,
    );
    ordered
}

/// Appends the groups of `span` to `ordered` in caption order: the top ones
/// of the span sorted by `compare`, each followed by its subgroups.
/// `siblings` keeps a buffer per depth across calls.
fn order_siblings(
    groups: &[KeyedGroup],
    span: Range<usize>,
    depth: usize,
    compare: &impl Fn(u32, u32) -> Ordering,
    siblings: &mut Vec<Vec<u32>>,
    ordered: &mut Vec<u32>,
) {
    if siblings.len() <= depth {
        siblings.resize_with(depth + 1, Vec::new);
    }
    let mut level = mem::take(&mut siblings[depth]);
    level.clear();
    let mut next = span.start;
    while next < span.end {
        level.push(next as u32);
        next = groups[next].end as usize;
    }
    level.sort_by(|&a, &b| compare(a, b));
    for &group in &level {
        ordered.push(group);
        let group = group as usize;
        let subgroups = group + 1..groups[group].end as usize;
        if !subgroups.is_empty() {
            order_siblings(groups, subgroups, depth + 1, compare, siblings, ordered);
        }
    }
    siblings[depth] = level;
}

/// Warns once per rebuild of a debug build when the group order reads a
/// column the view does not aggregate: such groups keep the key order.
#[cfg(debug_assertions)]
fn warn_unaggregated_order<R: GridRow>(spec: &ViewSpec<R>, aggregated: ColumnSet) {
    let order_columns = spec.group_order().depends_on();
    if let Some(column) = R::Column::all()
        .iter()
        .find(|column| order_columns.contains(column.ix()) && !aggregated.contains(column.ix()))
    {
        log::warn!(
            "the group order reads column {:?}, which the view does not aggregate: groups keep the key order",
            column.id()
        );
    }
}
