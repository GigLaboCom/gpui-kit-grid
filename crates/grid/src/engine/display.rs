//! The shown lines of a projection and what describes them: display
//! rows, group identity, the group table, aggregates and the expansion.
//! No GPUI types.
//!
//! A flat projection keeps no list of lines: its lines are its rows at
//! depth 0, so a view without grouping or a tree pays for none of this. A
//! grouped or tree projection lists its lines ([`Lines::Built`]); grouping
//! and the tree build them with the constructors here.

use std::{
    fmt,
    hash::Hash,
    iter::{self, FusedIterator},
    ops::Range,
    slice,
    sync::{Arc, OnceLock},
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use super::{Projection, ViewSpec, expand, places, shared_place, tree::TreeData};
use crate::{
    highlight::{ChangeDirection, HighlightPolicy, RowChange, Stamp},
    model::{ColumnId, ColumnIx, ColumnSet, GridRow, OwnedValue},
    store::{RowId, StoreSnapshot},
};

/// A group's position in one projection's group table. Transient, like a
/// display position: the identity of a group is its [`GroupPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GroupId(u32);

impl GroupId {
    pub(crate) fn new(ix: usize) -> Self {
        Self(ix as u32)
    }

    /// The position in the group table.
    pub fn ix(self) -> usize {
        self.0 as usize
    }
}

/// One line of the shown order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DisplayRow {
    /// A data row; `depth` is its tree depth, or the number of group levels
    /// above it.
    Data {
        /// The row.
        row: RowId,
        /// How deep it is.
        depth: u16,
    },
    /// The caption of a group.
    GroupCaption {
        /// The group.
        group: GroupId,
        /// The group's depth: 0 for a top-level group.
        depth: u16,
    },
    /// The footer of an expanded group.
    GroupFooter {
        /// The group.
        group: GroupId,
        /// The group's depth: 0 for a top-level group.
        depth: u16,
    },
}

impl DisplayRow {
    /// How deep the line is.
    pub fn depth(self) -> u16 {
        match self {
            Self::Data { depth, .. }
            | Self::GroupCaption { depth, .. }
            | Self::GroupFooter { depth, .. } => depth,
        }
    }

    /// The data row, for a data line.
    pub fn row(self) -> Option<RowId> {
        match self {
            Self::Data { row, .. } => Some(row),
            Self::GroupCaption { .. } | Self::GroupFooter { .. } => None,
        }
    }

    /// The group, for a caption or a footer.
    pub fn group(self) -> Option<GroupId> {
        match self {
            Self::GroupCaption { group, .. } | Self::GroupFooter { group, .. } => Some(group),
            Self::Data { .. } => None,
        }
    }
}

/// A group's identity: the id of each level (a column's [`ColumnId`] or a
/// `GroupKey` id) with the group's key value there, from the top level
/// down. Never positions, so a group keeps its identity across rebuilds,
/// sorts and snapshots.
///
/// The empty path is the root: it names no group and has no caption.
/// Cloning is O(1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct GroupPath(Arc<[(ColumnId, OwnedValue)]>);

impl GroupPath {
    /// The path of these levels, the top level first.
    pub fn new(levels: impl IntoIterator<Item = (ColumnId, OwnedValue)>) -> Self {
        Self(levels.into_iter().collect())
    }

    /// The depth of the group the path names: 0 for a top-level group, one
    /// more per level below. The root reads 0 too; [`Self::is_root`] tells
    /// it apart.
    pub fn depth(&self) -> u16 {
        self.0.len().saturating_sub(1) as u16
    }

    /// Returns true for the empty path, the root.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The levels, the top level first.
    pub fn levels(&self) -> &[(ColumnId, OwnedValue)] {
        &self.0
    }

    /// The id of the level at `depth`.
    pub fn level_id(&self, depth: u16) -> Option<&ColumnId> {
        self.0.get(usize::from(depth)).map(|(id, _)| id)
    }

    /// The key value at `depth`.
    pub fn value(&self, depth: u16) -> Option<&OwnedValue> {
        self.0.get(usize::from(depth)).map(|(_, value)| value)
    }

    /// The path of the enclosing group: the root for a top-level group,
    /// `None` for the root.
    pub fn parent(&self) -> Option<GroupPath> {
        let (_, parent) = self.0.split_last()?;
        Some(Self(parent.into()))
    }

    /// Returns true if `other` is this path or one of its ancestors.
    pub fn starts_with(&self, other: &GroupPath) -> bool {
        self.0.starts_with(&other.0)
    }
}

/// How deep groups and tree rows are expanded by default. The groups and
/// rows the user toggled against it are kept apart ([`Expansion`]).
///
/// Serialized as `"all"`, `"captions"` or the number of levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpandDepth {
    /// Everything is expanded: the default.
    #[default]
    All,
    /// The groups and tree rows of the first `n` levels are expanded, the
    /// deeper ones collapsed: `Levels(0)` collapses everything.
    Levels(u16),
    /// Every group level but the last is expanded: captions only, no data
    /// rows. A tree has no captions: there it is `All`.
    Captions,
}

impl ExpandDepth {
    /// Whether a group of `depth` in a grouping of `levels` levels is
    /// expanded by default.
    fn expands_group(self, depth: u16, levels: usize) -> bool {
        match self {
            Self::All => true,
            Self::Levels(n) => depth < n,
            Self::Captions => usize::from(depth) + 1 < levels,
        }
    }

    /// Whether a tree row of `depth` is expanded by default.
    fn expands_row(self, depth: u16) -> bool {
        match self {
            Self::All | Self::Captions => true,
            Self::Levels(n) => depth < n,
        }
    }
}

impl Serialize for ExpandDepth {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::All => serializer.serialize_str("all"),
            Self::Levels(levels) => serializer.serialize_u16(*levels),
            Self::Captions => serializer.serialize_str("captions"),
        }
    }
}

impl<'de> Deserialize<'de> for ExpandDepth {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DepthVisitor;

        impl de::Visitor<'_> for DepthVisitor {
            type Value = ExpandDepth;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(r#""all", "captions" or a number of levels"#)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<ExpandDepth, E> {
                match value {
                    "all" => Ok(ExpandDepth::All),
                    "captions" => Ok(ExpandDepth::Captions),
                    _ => Err(E::invalid_value(de::Unexpected::Str(value), &self)),
                }
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<ExpandDepth, E> {
                u16::try_from(value)
                    .map(ExpandDepth::Levels)
                    .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(value), &self))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<ExpandDepth, E> {
                u64::try_from(value)
                    .map_err(|_| E::invalid_value(de::Unexpected::Signed(value), &self))
                    .and_then(|value| self.visit_u64(value))
            }
        }

        deserializer.deserialize_any(DepthVisitor)
    }
}

/// The expansion a projection is laid out for: the default depth, and the
/// groups and tree rows the user toggled against it. The grid owns it, as
/// it owns the quick filter, and sets it into each view
/// ([`ViewSpec::expansion`]).
///
/// Toggles are kept by [`GroupPath`] and by row key, so they survive
/// rebuilds and a change of grouping: going back to an earlier grouping
/// brings its expansion back. Cloning is O(1).
pub struct Expansion<K> {
    depth: ExpandDepth,
    /// Groups toggled against `depth`; `None` for none.
    groups: Option<Arc<FxHashSet<GroupPath>>>,
    /// Tree rows toggled against `depth`; `None` for none.
    rows: Option<Arc<FxHashSet<K>>>,
}

impl<K> Clone for Expansion<K> {
    fn clone(&self) -> Self {
        Self {
            depth: self.depth,
            groups: self.groups.clone(),
            rows: self.rows.clone(),
        }
    }
}

impl<K> Default for Expansion<K> {
    fn default() -> Self {
        Self {
            depth: ExpandDepth::default(),
            groups: None,
            rows: None,
        }
    }
}

impl<K: fmt::Debug> fmt::Debug for Expansion<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Expansion")
            .field("depth", &self.depth)
            .field("groups", &self.groups.as_deref())
            .field("rows", &self.rows.as_deref())
            .finish()
    }
}

impl<K: Eq + Hash> PartialEq for Expansion<K> {
    fn eq(&self, other: &Self) -> bool {
        fn same<T: Eq + Hash>(a: Option<&FxHashSet<T>>, b: Option<&FxHashSet<T>>) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => a == b,
                (Some(set), None) | (None, Some(set)) => set.is_empty(),
                (None, None) => true,
            }
        }
        self.depth == other.depth
            && same(self.groups.as_deref(), other.groups.as_deref())
            && same(self.rows.as_deref(), other.rows.as_deref())
    }
}

impl<K: Eq + Hash> Eq for Expansion<K> {}

/// Defines a method that is public with `test-support`, so engine tests and
/// benchmarks outside the crate can lay groups out for an expansion, and
/// crate-visible otherwise: the grid owns the expansion.
macro_rules! pub_for_tests {
    ($(#[$meta:meta])* fn $($rest:tt)*) => {
        $(#[$meta])*
        #[cfg(any(test, feature = "test-support"))]
        pub fn $($rest)*

        $(#[$meta])*
        #[cfg(not(any(test, feature = "test-support")))]
        pub(crate) fn $($rest)*
    };
}

impl<K: Clone + Eq + Hash> Expansion<K> {
    pub_for_tests! {
        /// `depth` with nothing toggled.
        fn new(depth: ExpandDepth) -> Self {
            Self {
                depth,
                ..Self::default()
            }
        }
    }

    pub_for_tests! {
        /// Expands or collapses the group at `path` in a grouping of `levels`
        /// levels: toggled when that differs from the default depth.
        fn with_group(mut self, path: GroupPath, expanded: bool, levels: usize) -> Self {
            let toggled = expanded != self.depth.expands_group(path.depth(), levels);
            toggle(&mut self.groups, path, toggled);
            self
        }
    }

    pub_for_tests! {
        /// Expands or collapses the tree row with `key` at `depth`: toggled
        /// when that differs from the default depth.
        fn with_row(mut self, key: K, expanded: bool, depth: u16) -> Self {
            let toggled = expanded != self.depth.expands_row(depth);
            toggle(&mut self.rows, key, toggled);
            self
        }
    }

    /// The groups toggled against the default depth, in no order.
    pub(crate) fn toggled_groups(&self) -> impl Iterator<Item = &GroupPath> {
        self.groups.iter().flat_map(|groups| groups.iter())
    }

    /// The tree rows toggled against the default depth, in no order.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "a tree's expansion is not saved in layouts (9.4)")
    )]
    pub(crate) fn toggled_rows(&self) -> impl Iterator<Item = &K> {
        self.rows.iter().flat_map(|rows| rows.iter())
    }
}

/// Adds `item` to a set of toggles or removes it, cloning a shared set.
fn toggle<T: Clone + Eq + Hash>(set: &mut Option<Arc<FxHashSet<T>>>, item: T, toggled: bool) {
    if toggled {
        Arc::make_mut(set.get_or_insert_with(Default::default)).insert(item);
    } else if let Some(items) = set {
        if items.contains(&item) {
            Arc::make_mut(items).remove(&item);
        }
    }
}

impl<K: Eq + Hash> Expansion<K> {
    /// The default depth.
    pub fn depth(&self) -> ExpandDepth {
        self.depth
    }

    /// Whether the group at `path`, in a grouping of `levels` levels, is
    /// expanded: by the default depth, inverted if the user toggled it.
    pub fn is_group_expanded(&self, path: &GroupPath, levels: usize) -> bool {
        let toggled = self
            .groups
            .as_ref()
            .is_some_and(|groups| groups.contains(path));
        self.depth.expands_group(path.depth(), levels) != toggled
    }

    /// Whether the tree row with `key` at `depth` is expanded: by the
    /// default depth (`Captions` is `All` in a tree), inverted if the user
    /// toggled it.
    pub fn is_row_expanded(&self, key: &K, depth: u16) -> bool {
        let toggled = self.rows.as_ref().is_some_and(|rows| rows.contains(key));
        self.depth.expands_row(depth) != toggled
    }
}

impl<R: GridRow> ViewSpec<R> {
    /// The expansion the view is laid out for. The grid sets it: groups and
    /// tree rows are expanded and collapsed through the grid's state.
    pub fn expansion(&self) -> &Expansion<R::Key> {
        &self.expansion
    }

    pub_for_tests! {
        /// Replaces the expansion; the grid owns it.
        fn with_expansion(mut self, expansion: Expansion<R::Key>) -> Self {
            self.expansion = expansion;
            self
        }
    }
}

/// Aggregated values over a set of columns: one value per column of the
/// set, in schema order; for a group table, one such run per group.
#[derive(Clone, Debug, Default)]
pub(crate) struct AggregateValues {
    columns: ColumnSet,
    values: Vec<OwnedValue>,
}

impl AggregateValues {
    /// `values` of `columns`, a run of `columns.len()` values per row of
    /// aggregates.
    pub(crate) fn new(columns: ColumnSet, values: Vec<OwnedValue>) -> Self {
        debug_assert!(
            columns.is_empty() || values.len() % columns.len() == 0,
            "a value per column"
        );
        Self { columns, values }
    }

    /// The run of aggregates at `ix`, of `row_count` rows.
    fn row(&self, ix: usize, row_count: usize) -> AggregateRow<'_> {
        let width = self.columns.len();
        AggregateRow {
            columns: self.columns,
            values: self
                .values
                .get(ix * width..(ix + 1) * width)
                .unwrap_or_default(),
            row_count,
        }
    }
}

/// The aggregates of one group, or of the whole view for the totals: a
/// view into the projection.
#[derive(Clone, Copy, Debug)]
pub struct AggregateRow<'a> {
    columns: ColumnSet,
    values: &'a [OwnedValue],
    row_count: usize,
}

impl<'a> AggregateRow<'a> {
    /// The aggregates `values` of `columns` (one per column, in schema
    /// order), over `row_count` rows.
    pub(crate) fn new(columns: ColumnSet, values: &'a [OwnedValue], row_count: usize) -> Self {
        debug_assert_eq!(values.len(), columns.len(), "a value per column");
        Self {
            columns,
            values,
            row_count,
        }
    }

    /// The aggregate of a column, if the view aggregates it.
    pub fn get(&self, column: ColumnIx) -> Option<&'a OwnedValue> {
        if !self.columns.contains(column) {
            return None;
        }
        self.values.get(self.columns.rank(column))
    }

    /// The aggregated columns.
    pub fn columns(&self) -> ColumnSet {
        self.columns
    }

    /// The live shown rows of the view or group; fading rows are not
    /// counted. For a group and flat or grouped totals these are the rows
    /// the aggregates are over. A tree's totals count every live shown row,
    /// nodes included, while their aggregates follow [`TreeTotals`](super::TreeTotals)
    /// (`Leaves` by default: only rows without live shown children).
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// Returns true if no column is aggregated.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// One group of a grouped projection.
#[derive(Clone, Debug)]
pub(crate) struct Group {
    pub(super) path: GroupPath,
    pub(super) parent: Option<GroupId>,
    /// The group past the last of its subgroups, which follow it in
    /// caption order: they are `id + 1..end`.
    pub(super) end: u32,
    /// The group's place in key order, before `GroupOrder` sorted the
    /// siblings: aggregates combine subgroups in this order, so they do not
    /// depend on the group order or on a later job.
    pub(super) key_ix: u32,
    /// The group's rows in `Projection::rows`, its subgroups' included.
    pub(super) rows: Range<u32>,
    /// Live rows: fading ones left out.
    pub(super) live: u32,
    /// The caption line; `u32::MAX` while a collapsed ancestor hides it.
    pub(super) line: u32,
    pub(super) expanded: bool,
    pub(super) footer: bool,
}

impl Group {
    /// The group's depth: 0 for a top-level group.
    pub(super) fn depth(&self) -> u16 {
        self.path.depth()
    }

    /// The group's rows, as a range of `Projection::rows`.
    pub(super) fn row_range(&self) -> Range<usize> {
        self.rows.start as usize..self.rows.end as usize
    }
}

/// The groups of a grouped projection, in caption order: a group, then its
/// subgroups, siblings in the group order.
#[derive(Clone, Debug, Default)]
pub(crate) struct GroupData {
    pub(super) groups: Vec<Group>,
    /// Each group's aggregates, a run per group in `GroupId` order.
    pub(super) aggregates: AggregateValues,
    /// The number of grouping levels: groups of the last one hold the data
    /// rows, the others hold groups.
    pub(super) levels: u16,
    /// `GroupTable::find`, built on first use: the frame does not need it.
    by_path: OnceLock<FxHashMap<GroupPath, GroupId>>,
}

/// The group table of a projection without groups.
static NO_GROUPS: GroupData = GroupData {
    groups: Vec::new(),
    aggregates: AggregateValues {
        columns: ColumnSet::EMPTY,
        values: Vec::new(),
    },
    levels: 0,
    by_path: OnceLock::new(),
};

impl GroupData {
    /// `groups` of a grouping of `levels` levels, in caption order, with
    /// their aggregates: a run per group.
    pub(super) fn new(levels: u16, groups: Vec<Group>, aggregates: AggregateValues) -> Self {
        debug_assert_eq!(
            aggregates.values.len(),
            groups.len() * aggregates.columns.len(),
            "a value per aggregated column"
        );
        Self {
            groups,
            aggregates,
            levels,
            by_path: OnceLock::new(),
        }
    }

    /// Returns true if `group` is of the last level: it holds data rows.
    pub(super) fn is_leaf(&self, group: &Group) -> bool {
        usize::from(group.depth()) + 1 == usize::from(self.levels)
    }

    /// The level ids from the top down, if there is a group: the first
    /// group of the last level comes right after its ancestors.
    fn level_ids(&self) -> Option<impl Iterator<Item = &ColumnId>> {
        let first_leaf = self.groups.get(usize::from(self.levels).checked_sub(1)?)?;
        Some(first_leaf.path.levels().iter().map(|(id, _)| id))
    }

    /// The table over `rows`, the projection's rows.
    pub(super) fn table<'a>(&'a self, rows: &'a [RowId]) -> GroupTable<'a> {
        GroupTable { data: self, rows }
    }
}

/// The groups of a projection: a view into it.
#[derive(Clone, Copy)]
pub struct GroupTable<'a> {
    data: &'a GroupData,
    /// `Projection::rows`, which groups take their rows from.
    rows: &'a [RowId],
}

impl<'a> GroupTable<'a> {
    /// The number of groups, at every level.
    pub fn len(&self) -> usize {
        self.data.groups.len()
    }

    /// Returns true if the projection has no groups.
    pub fn is_empty(&self) -> bool {
        self.data.groups.is_empty()
    }

    /// The group with `id`.
    pub fn get(&self, id: GroupId) -> Option<GroupInfo<'a>> {
        (id.ix() < self.len()).then_some(GroupInfo { table: *self, id })
    }

    /// Every group in caption order: a group, then its subgroups.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = GroupInfo<'a>> + 'a {
        let table = *self;
        (0..self.len()).map(move |ix| GroupInfo {
            table,
            id: GroupId::new(ix),
        })
    }

    /// The group at `path`, if the projection has it. The first call builds
    /// a map of the paths.
    pub fn find(&self, path: &GroupPath) -> Option<GroupInfo<'a>> {
        let by_path = self.data.by_path.get_or_init(|| {
            self.data
                .groups
                .iter()
                .enumerate()
                .map(|(ix, group)| (group.path.clone(), GroupId::new(ix)))
                .collect()
        });
        self.get(*by_path.get(path)?)
    }
}

impl fmt::Debug for GroupTable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// One group of a projection: a view into it.
#[derive(Clone, Copy)]
pub struct GroupInfo<'a> {
    table: GroupTable<'a>,
    id: GroupId,
}

impl<'a> GroupInfo<'a> {
    fn group(&self) -> &'a Group {
        &self.table.data.groups[self.id.ix()]
    }

    /// The group's position in this projection's table.
    pub fn id(&self) -> GroupId {
        self.id
    }

    /// The group's identity.
    pub fn path(&self) -> &'a GroupPath {
        &self.group().path
    }

    /// The id of the group's level: a column's id or a `GroupKey` id.
    pub fn level_id(&self) -> &'a ColumnId {
        let path = self.path();
        path.level_id(path.depth())
            .expect("a group's path is not the root")
    }

    /// The group's key value.
    pub fn value(&self) -> &'a OwnedValue {
        let path = self.path();
        path.value(path.depth())
            .expect("a group's path is not the root")
    }

    /// The group's depth: 0 for a top-level group.
    pub fn depth(&self) -> u16 {
        self.path().depth()
    }

    /// The enclosing group; `None` at the top level.
    pub fn parent(&self) -> Option<GroupId> {
        self.group().parent
    }

    /// The group's live rows, its subgroups' included; fading rows are
    /// not counted.
    pub fn row_count(&self) -> usize {
        self.group().live as usize
    }

    /// The group's rows in order, its subgroups' included: a slice of
    /// [`Projection::rows`]. Fading rows are among them.
    pub fn rows(&self) -> &'a [RowId] {
        let rows = &self.group().rows;
        &self.table.rows[rows.start as usize..rows.end as usize]
    }

    /// Returns true if the group is expanded.
    pub fn is_expanded(&self) -> bool {
        self.group().expanded
    }

    /// Returns true if the group has a footer (`GroupFooter`).
    pub fn has_footer(&self) -> bool {
        self.group().footer
    }

    /// The group's aggregates.
    pub fn aggregates(&self) -> AggregateRow<'a> {
        self.table
            .data
            .aggregates
            .row(self.id.ix(), self.row_count())
    }

    /// The caption line, unless a collapsed ancestor hides it.
    fn line(&self) -> Option<usize> {
        let line = self.group().line;
        (line != u32::MAX).then_some(line as usize)
    }
}

impl fmt::Debug for GroupInfo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GroupInfo")
            .field("id", &self.id)
            .field("path", self.path())
            .field("row_count", &self.row_count())
            .field("is_expanded", &self.is_expanded())
            .field("has_footer", &self.has_footer())
            .finish()
    }
}

/// The shown lines of a projection.
#[derive(Clone, Debug, Default)]
pub(crate) enum Lines {
    /// No grouping, no tree: the lines are `Projection::rows` at depth 0.
    #[default]
    Flat,
    /// Groups or a tree: the lines are listed. Boxed, so a flat projection
    /// stays small.
    Built(Box<BuiltLines>),
}

/// The listed lines of a grouped or tree projection.
#[derive(Clone, Debug)]
pub(crate) struct BuiltLines {
    lines: Vec<DisplayRow>,
    /// The line of each shown data row, by slot; `u32::MAX` for none.
    row_to_display: Vec<u32>,
    /// The lines of the shown data rows whose slot an earlier line holds in
    /// `row_to_display`: a fading row and the new row that reused its slot.
    shared_display: FxHashMap<RowId, u32>,
    structure: BuiltStructure,
}

impl BuiltLines {
    /// `lines` of `structure`, over slots below `capacity`.
    fn new(lines: Vec<DisplayRow>, capacity: usize, structure: BuiltStructure) -> Box<Self> {
        let (row_to_display, shared_display) = places(
            lines
                .iter()
                .enumerate()
                .filter_map(|(display_ix, line)| line.row().map(|id| (display_ix, id))),
            capacity,
        );
        Box::new(Self {
            lines,
            row_to_display,
            shared_display,
            structure,
        })
    }
}

/// What a listed projection is built of.
#[derive(Clone, Debug)]
pub(crate) enum BuiltStructure {
    /// Groups.
    Groups(GroupData),
    /// A tree.
    Tree(TreeData),
}

impl Projection {
    /// A projection whose lines are listed: `rows` in order as if all were
    /// expanded, the shown `lines` and what they are built of. Every data
    /// line's row must be in `rows`.
    pub(crate) fn with_lines<R: GridRow>(
        snapshot: &StoreSnapshot<R>,
        rows: Vec<RowId>,
        lines: Vec<DisplayRow>,
        structure: BuiltStructure,
    ) -> Self {
        let mut projection = Self::new(snapshot, rows);
        projection.lines = Lines::Built(BuiltLines::new(
            lines,
            projection.row_to_order.len(),
            structure,
        ));
        projection
    }

    /// Sets the live rows: fewer than `rows` when fading rows are among
    /// them, or when the snapshot no longer has some.
    pub(crate) fn with_row_count(mut self, row_count: usize) -> Self {
        self.row_count = row_count;
        self
    }

    /// A grouped projection: `rows` in order as if all were expanded, the
    /// rows of each group following each other, and the groups over them
    /// with their flags. Lays the lines out by those flags.
    pub(crate) fn with_groups<R: GridRow>(
        snapshot: &StoreSnapshot<R>,
        rows: Vec<RowId>,
        mut groups: GroupData,
    ) -> Self {
        let lines = expand::lay_out(&mut groups, &rows);
        Self::with_lines(snapshot, rows, lines, BuiltStructure::Groups(groups))
    }

    /// This projection's rows, version and totals over `groups`, its own
    /// groups with new expansion flags, laid out anew: the expand job.
    pub(super) fn with_group_layout(&self, mut groups: GroupData) -> Self {
        let lines = expand::lay_out(&mut groups, &self.rows);
        Self {
            store_version: self.store_version,
            rows: self.rows.clone(),
            row_to_order: self.row_to_order.clone(),
            shared_order: self.shared_order.clone(),
            row_count: self.row_count,
            lines: Lines::Built(BuiltLines::new(
                lines,
                self.row_to_order.len(),
                BuiltStructure::Groups(groups),
            )),
            totals: self.totals.clone(),
            summary_stamps: None,
        }
    }

    /// This grouped projection's order and lines over `groups`, its own
    /// groups with fresh counts and aggregates, as of `store_version`: the
    /// aggregate job.
    pub(super) fn with_group_values(
        &self,
        groups: GroupData,
        totals: AggregateValues,
        row_count: usize,
        store_version: u64,
    ) -> Self {
        let built = self.built().expect("a grouped projection lists its lines");
        Self {
            store_version,
            rows: self.rows.clone(),
            row_to_order: self.row_to_order.clone(),
            shared_order: self.shared_order.clone(),
            row_count,
            lines: Lines::Built(Box::new(BuiltLines {
                lines: built.lines.clone(),
                row_to_display: built.row_to_display.clone(),
                shared_display: built.shared_display.clone(),
                structure: BuiltStructure::Groups(groups),
            })),
            totals,
            summary_stamps: None,
        }
    }

    /// The groups of a grouped projection.
    pub(super) fn group_data(&self) -> Option<&GroupData> {
        match self.built().map(|built| &built.structure) {
            Some(BuiltStructure::Groups(groups)) => Some(groups),
            _ => None,
        }
    }

    /// Sets the aggregates of the whole view: one run over their columns.
    pub(crate) fn with_totals(mut self, totals: AggregateValues) -> Self {
        self.totals = totals;
        self
    }

    /// Sets the store version the projection reflects.
    pub(crate) fn with_store_version(mut self, store_version: u64) -> Self {
        self.store_version = store_version;
        self
    }

    /// The listed lines, if not flat.
    fn built(&self) -> Option<&BuiltLines> {
        match &self.lines {
            Lines::Flat => None,
            Lines::Built(built) => Some(built),
        }
    }

    /// The number of shown lines: group captions, data rows and group
    /// footers. In a flat view, the number of displayed rows.
    pub fn len(&self) -> usize {
        match &self.lines {
            Lines::Flat => self.rows.len(),
            Lines::Built(built) => built.lines.len(),
        }
    }

    /// Returns true if no line is shown.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The line at a display position.
    pub fn line(&self, display_ix: usize) -> Option<DisplayRow> {
        match &self.lines {
            Lines::Flat => self
                .rows
                .get(display_ix)
                .map(|&row| DisplayRow::Data { row, depth: 0 }),
            Lines::Built(built) => built.lines.get(display_ix).copied(),
        }
    }

    /// The data row at a display position; `None` on a group caption or
    /// footer.
    pub fn row(&self, display_ix: usize) -> Option<RowId> {
        match &self.lines {
            Lines::Flat => self.rows.get(display_ix).copied(),
            Lines::Built(built) => built.lines.get(display_ix).and_then(|line| line.row()),
        }
    }

    /// The display position of a data row; `None` if the view filters it
    /// out or a collapsed group or tree row hides it.
    pub fn display_ix(&self, id: RowId) -> Option<usize> {
        match &self.lines {
            Lines::Flat => self.order_ix(id),
            Lines::Built(built) => match built.row_to_display.get(id.ix()) {
                Some(&ix) if ix != u32::MAX && built.lines[ix as usize].row() == Some(id) => {
                    Some(ix as usize)
                }
                _ => shared_place(&built.shared_display, id),
            },
        }
    }

    /// The position of a row in [`Self::rows`], shown or in a collapsed
    /// group; `None` if the view filters it out.
    pub fn order_ix(&self, id: RowId) -> Option<usize> {
        match self.row_to_order.get(id.ix()) {
            Some(&ix) if ix != u32::MAX && self.rows[ix as usize] == id => Some(ix as usize),
            _ => shared_place(&self.shared_order, id),
        }
    }

    /// The live rows of [`Self::rows`] when the projection was built: fading
    /// rows are not counted. In a flat view without fading rows, the number
    /// of displayed rows.
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// The data rows of the shown lines, in display order.
    // The copy walks the lines; only tests read the data rows alone.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn shown_rows(&self) -> ShownRowIds<'_> {
        match &self.lines {
            Lines::Flat => ShownRowIds::Flat(self.rows.iter()),
            Lines::Built(built) => ShownRowIds::Built(built.lines.iter()),
        }
    }

    /// The group table; empty without grouping.
    pub fn groups(&self) -> GroupTable<'_> {
        self.group_data().unwrap_or(&NO_GROUPS).table(&self.rows)
    }

    /// The display position of a group's caption; `None` while a collapsed
    /// ancestor hides it.
    pub fn group_line(&self, group: GroupId) -> Option<usize> {
        self.groups().get(group)?.line()
    }

    /// The aggregates of the whole view, for the totals row; empty without
    /// aggregates.
    pub fn totals(&self) -> AggregateRow<'_> {
        self.totals.row(0, self.row_count)
    }

    /// Returns true if the view is grouped.
    pub fn is_grouped(&self) -> bool {
        matches!(
            self.built().map(|built| &built.structure),
            Some(BuiltStructure::Groups(_))
        )
    }

    /// Returns true if the view is a tree.
    pub fn is_tree(&self) -> bool {
        matches!(
            self.built().map(|built| &built.structure),
            Some(BuiltStructure::Tree(_))
        )
    }

    /// The tree data of a tree projection.
    pub(crate) fn tree_data(&self) -> Option<&TreeData> {
        match self.built().map(|built| &built.structure) {
            Some(BuiltStructure::Tree(tree)) => Some(tree),
            _ => None,
        }
    }

    /// The lines as if every group and tree row were expanded, as an export
    /// of everything writes them: group footers by
    /// [`GroupInfo::has_footer`], tree rows at their depths. In a flat view,
    /// the shown lines. Walks the groups or the tree without listing the
    /// lines.
    pub fn expanded_lines(&self) -> impl Iterator<Item = DisplayRow> + '_ {
        match &self.lines {
            Lines::Flat => ExpandedLines::Flat(self.rows.iter()),
            Lines::Built(built) => match &built.structure {
                BuiltStructure::Groups(groups) => {
                    ExpandedLines::Groups(expand::ExpandedGroupLines::new(groups, &self.rows))
                }
                BuiltStructure::Tree(tree) => {
                    ExpandedLines::Tree(self.rows.iter().enumerate(), tree)
                }
            },
        }
    }
}

/// The data rows of the shown lines (`Projection::shown_rows`).
// Only tests use it: see `Projection::shown_rows`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum ShownRowIds<'a> {
    Flat(slice::Iter<'a, RowId>),
    Built(slice::Iter<'a, DisplayRow>),
}

impl Iterator for ShownRowIds<'_> {
    type Item = RowId;

    #[inline]
    fn next(&mut self) -> Option<RowId> {
        match self {
            Self::Flat(rows) => rows.next().copied(),
            Self::Built(lines) => lines.find_map(|line| line.row()),
        }
    }
}

impl FusedIterator for ShownRowIds<'_> {}

/// `Projection::expanded_lines`.
enum ExpandedLines<'a> {
    Flat(slice::Iter<'a, RowId>),
    Groups(expand::ExpandedGroupLines<'a>),
    /// The rows of a tree with their places, for their depths.
    Tree(iter::Enumerate<slice::Iter<'a, RowId>>, &'a TreeData),
}

impl Iterator for ExpandedLines<'_> {
    type Item = DisplayRow;

    fn next(&mut self) -> Option<DisplayRow> {
        match self {
            Self::Flat(rows) => rows.next().map(|&row| DisplayRow::Data { row, depth: 0 }),
            Self::Groups(lines) => lines.next(),
            Self::Tree(rows, tree) => rows.next().map(|(order_ix, &row)| DisplayRow::Data {
                row,
                depth: tree.depth(order_ix),
            }),
        }
    }
}

/// Drops rows removed since `previous` was built, from a flat or grouped
/// projection, and keeps the order. A removed row that fades stays while the
/// snapshot has it ([`StoreSnapshot::fading_row`]), not counted as live.
///
/// A flat projection keeps its totals. A grouped one recounts its groups,
/// drops the emptied ones and lays its lines out again with each group's
/// expansion and footer as they were; the aggregates stay as they were too,
/// as the aggregate job refreshes them. A group left with fading rows
/// alone goes with them, as a rebuild makes no group of fading rows.
pub(super) fn compact<R: GridRow>(
    previous: &Projection,
    snapshot: &StoreSnapshot<R>,
) -> Projection {
    if let Some(groups) = previous.group_data() {
        return compact_groups(previous, groups, snapshot);
    }
    let mut live = 0;
    let rows = previous
        .rows
        .iter()
        .copied()
        .filter(|&id| is_kept(snapshot, id, &mut live))
        .collect();
    Projection::new(snapshot, rows)
        .with_row_count(live as usize)
        .with_totals(previous.totals.clone())
}

/// Returns true if `snapshot` still has the row, live or fading, and counts
/// it in `live` if live.
#[inline]
fn is_kept<R: GridRow>(snapshot: &StoreSnapshot<R>, id: RowId, live: &mut u32) -> bool {
    if snapshot.row(id).is_some() {
        *live += 1;
        true
    } else {
        snapshot.fading_row(id).is_some()
    }
}

/// [`compact`] of a grouped projection: O(rows + groups), no sorting.
fn compact_groups<R: GridRow>(
    previous: &Projection,
    data: &GroupData,
    snapshot: &StoreSnapshot<R>,
) -> Projection {
    let old = &data.groups;
    // The rows of the last level's groups follow each other in caption
    // order and cover every row, so one pass over them keeps the order.
    let mut rows = Vec::with_capacity(previous.rows.len());
    let mut ranges: Vec<Range<u32>> = vec![0..0; old.len()];
    let mut live = vec![0u32; old.len()];
    let mut row_count = 0;
    for (ix, group) in old.iter().enumerate() {
        if data.is_leaf(group) {
            let start = rows.len();
            let mut group_live = 0;
            rows.extend(
                previous.rows[group.row_range()]
                    .iter()
                    .copied()
                    .filter(|&id| is_kept(snapshot, id, &mut group_live)),
            );
            // Fading rows alone make no group.
            if group_live == 0 {
                rows.truncate(start);
            }
            ranges[ix] = start as u32..rows.len() as u32;
            live[ix] = group_live;
            row_count += group_live as usize;
        }
    }
    // An upper group spans its subgroups: from its first one to its last
    // descendant, a group of the last level. Later groups come first, so
    // the subgroups are done.
    for (ix, group) in old.iter().enumerate().rev() {
        if !data.is_leaf(group) {
            let end = group.end as usize;
            ranges[ix] = ranges[ix + 1].start..ranges[end - 1].end;
            let mut child = ix + 1;
            while child < end {
                live[ix] += live[child];
                child = old[child].end as usize;
            }
        }
    }

    // Emptied groups go, with their subgroups: the ids of the others close
    // up. `kept_before[ix]` is the new id of the group at `ix`, if kept.
    let mut kept_before = Vec::with_capacity(old.len() + 1);
    let mut kept = 0u32;
    for range in &ranges {
        kept_before.push(kept);
        kept += u32::from(!range.is_empty());
    }
    kept_before.push(kept);
    let width = data.aggregates.columns.len();
    let mut groups = Vec::with_capacity(kept as usize);
    let mut values = Vec::with_capacity(kept as usize * width);
    for (ix, group) in old.iter().enumerate() {
        if ranges[ix].is_empty() {
            continue;
        }
        groups.push(Group {
            path: group.path.clone(),
            parent: group.parent.map(|parent| GroupId(kept_before[parent.ix()])),
            end: kept_before[group.end as usize],
            key_ix: group.key_ix,
            rows: ranges[ix].clone(),
            live: live[ix],
            line: u32::MAX,
            expanded: group.expanded,
            footer: group.footer,
        });
        values.extend_from_slice(&data.aggregates.values[ix * width..(ix + 1) * width]);
    }
    let groups = GroupData::new(
        data.levels,
        groups,
        AggregateValues::new(data.aggregates.columns, values),
    );
    Projection::with_groups(snapshot, rows, groups)
        .with_row_count(row_count)
        .with_totals(previous.totals.clone())
}

/// Change stamps of a projection's aggregates and group captions: what
/// changed against the projection before, matched by [`GroupPath`]. Kept
/// while any stamp is hot.
#[derive(Clone, Debug)]
pub(crate) struct SummaryStamps {
    /// Stamps per run: one per column up to the last aggregated one, so a
    /// run is indexed by `ColumnIx`.
    width: usize,
    /// A run per group, in `GroupId` order, then the totals' run.
    aggregates: Vec<Stamp>,
    /// Each group's caption stamp, in `GroupId` order.
    captions: Vec<Stamp>,
}

impl SummaryStamps {
    /// The run of stamps at `ix`, if any is set.
    fn run(&self, ix: usize) -> Option<&[Stamp]> {
        let run = self
            .aggregates
            .get(ix * self.width..(ix + 1) * self.width)?;
        run.iter().any(|stamp| stamp.is_set()).then_some(run)
    }
}

impl Projection {
    /// The change stamps of a group's aggregates while any is hot, indexed
    /// by `ColumnIx`: one per column up to the last aggregated one,
    /// [`Stamp::NONE`] for a column whose aggregate did not change. The view
    /// sets them as aggregates change; a new group has none, its caption
    /// shows it ([`Self::caption_stamp`]).
    pub fn group_stamps(&self, group: GroupId) -> Option<&[Stamp]> {
        let stamps = self.summary_stamps.as_deref()?;
        if group.ix() >= stamps.captions.len() {
            return None;
        }
        stamps.run(group.ix())
    }

    /// The change stamps of the totals while any is hot, indexed by
    /// `ColumnIx` as [`Self::group_stamps`].
    pub fn totals_stamps(&self) -> Option<&[Stamp]> {
        let stamps = self.summary_stamps.as_deref()?;
        stamps.run(stamps.captions.len())
    }

    /// The stamp of a group's caption while it is hot: [`RowChange::Inserted`]
    /// for a new group, [`RowChange::Updated`] without a direction when its
    /// live row count changed.
    pub fn caption_stamp(&self, group: GroupId) -> Option<Stamp> {
        let stamps = self.summary_stamps.as_deref()?;
        stamps
            .captions
            .get(group.ix())
            .copied()
            .filter(|stamp| stamp.is_set())
    }

    pub_for_tests! {
        /// Stamps the aggregates and captions that differ from `previous`,
        /// the projection the view showed or held before this one, matched
        /// by [`GroupPath`], at `now_ms` (the store clock, milliseconds
        /// since the snapshot epoch), and carries the stamps of `previous`
        /// that are still hot by `policy` where nothing changed. A changed
        /// aggregate is [`RowChange::Updated`], with the direction of two
        /// numbers; a new group's caption [`RowChange::Inserted`], a caption
        /// whose live row count changed `Updated`.
        ///
        /// A `previous` of another structure or other aggregated columns (a
        /// new grouping, flat against grouped) gives no stamps: the view
        /// changed, not the data. O(groups × aggregated columns).
        fn carry_summary_stamps(
            &mut self,
            previous: &Projection,
            now_ms: u32,
            policy: HighlightPolicy,
        ) {
            self.summary_stamps = None;
            if !self.has_summaries_of(previous) {
                return;
            }
            let columns = self.totals.columns;
            let width = (0..128u16)
                .rev()
                .find(|&ix| columns.contains(ColumnIx::new(ix)))
                .map_or(0, |ix| usize::from(ix) + 1);
            let hot = |stamp: Option<Stamp>| {
                stamp.filter(|&stamp| !policy.is_expired(stamp, now_ms))
                    .unwrap_or(Stamp::NONE)
            };
            // One run: a stamp for each changed aggregate, the hot ones
            // carried for the rest.
            let stamp_run = |run: &mut [Stamp],
                             new: AggregateRow<'_>,
                             old: AggregateRow<'_>,
                             old_stamps: Option<&[Stamp]>| {
                for (ix, stamp) in run.iter_mut().enumerate() {
                    let column = ColumnIx::new(ix as u16);
                    *stamp = match (new.get(column), old.get(column)) {
                        (Some(new), Some(old)) if new != old => Stamp::with_direction(
                            now_ms,
                            RowChange::Updated,
                            direction(new, old),
                        ),
                        (Some(_), Some(_)) => hot(old_stamps.and_then(|old| old.get(ix).copied())),
                        _ => Stamp::NONE,
                    };
                }
            };

            let groups = self.groups();
            let old_groups = previous.groups();
            let mut aggregates = vec![Stamp::NONE; (groups.len() + 1) * width];
            let mut captions = vec![Stamp::NONE; groups.len()];
            for group in groups.iter() {
                let ix = group.id().ix();
                captions[ix] = match old_groups.find(group.path()) {
                    Some(old) => {
                        stamp_run(
                            &mut aggregates[ix * width..(ix + 1) * width],
                            group.aggregates(),
                            old.aggregates(),
                            previous.group_stamps(old.id()),
                        );
                        if group.row_count() == old.row_count() {
                            hot(previous.caption_stamp(old.id()))
                        } else {
                            Stamp::new(now_ms, RowChange::Updated)
                        }
                    }
                    None => Stamp::new(now_ms, RowChange::Inserted),
                };
            }
            stamp_run(
                &mut aggregates[groups.len() * width..],
                self.totals(),
                previous.totals(),
                previous.totals_stamps(),
            );
            if aggregates.iter().chain(&captions).any(|stamp| stamp.is_set()) {
                self.summary_stamps = Some(Box::new(SummaryStamps {
                    width,
                    aggregates,
                    captions,
                }));
            }
        }
    }

    /// Returns true if `other` has the summaries of this projection: the
    /// same aggregated columns and the same structure, flat, a tree or
    /// groups of the same levels.
    fn has_summaries_of(&self, other: &Projection) -> bool {
        if self.totals.columns != other.totals.columns {
            return false;
        }
        fn structure(projection: &Projection) -> Option<&BuiltStructure> {
            projection.built().map(|built| &built.structure)
        }
        match (structure(self), structure(other)) {
            (None, None) | (Some(BuiltStructure::Tree(_)), Some(BuiltStructure::Tree(_))) => true,
            (Some(BuiltStructure::Groups(a)), Some(BuiltStructure::Groups(b))) => {
                a.levels == b.levels
                    && match (a.level_ids(), b.level_ids()) {
                        (Some(a), Some(b)) => a.eq(b),
                        _ => true,
                    }
            }
            _ => false,
        }
    }
}

/// The direction from `old` to `new`, when both are numbers.
fn direction(new: &OwnedValue, old: &OwnedValue) -> Option<ChangeDirection> {
    let (new, old) = (new.as_cell(), old.as_cell());
    if !(new.is_number() && old.is_number()) {
        return None;
    }
    match new.cmp_total(&old) {
        std::cmp::Ordering::Greater => Some(ChangeDirection::Up),
        std::cmp::Ordering::Less => Some(ChangeDirection::Down),
        std::cmp::Ordering::Equal => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::{
        highlight::HighlightPolicy,
        model::{CellValue, GridColumns},
        store::{Ingest, Op},
    };

    fn path(values: &[i64]) -> GroupPath {
        const LEVELS: [&str; 3] = ["a", "b", "c"];
        GroupPath::new(
            values
                .iter()
                .zip(LEVELS)
                .map(|(value, id)| (ColumnId::new_static(id), OwnedValue::Int(*value))),
        )
    }

    #[test]
    fn test_group_path() {
        let root = GroupPath::default();
        assert!(root.is_root() && root.levels().is_empty());
        assert_eq!(root.parent(), None);
        let top = path(&[1]);
        let inner = path(&[1, 2]);
        assert_eq!((top.depth(), inner.depth()), (0, 1));
        assert_eq!(inner.level_id(1).map(ColumnId::as_str), Some("b"));
        assert_eq!(inner.value(0), Some(&OwnedValue::Int(1)));
        assert_eq!(inner.value(2), None);
        assert_eq!(inner.parent(), Some(top.clone()));
        assert_eq!(top.parent(), Some(root.clone()));
        assert!(inner.starts_with(&top) && inner.starts_with(&root) && inner.starts_with(&inner));
        assert!(!top.starts_with(&inner) && !path(&[2, 2]).starts_with(&top));
        // Values equal by `cmp_total` name one group: 1 and 1.0.
        let float = GroupPath::new([(ColumnId::new_static("a"), OwnedValue::Float(1.0))]);
        assert_eq!(float, top);
        assert_eq!(
            FxHashSet::from_iter([float])
                .get(&top)
                .map(GroupPath::depth),
            Some(0)
        );
    }

    #[test]
    fn test_expansion_rule() {
        use ExpandDepth::{All, Captions, Levels};
        // (depth, levels, expanded by default) for each default depth.
        let cases: [(ExpandDepth, &[(u16, usize, bool)]); 6] = [
            (All, &[(0, 1, true), (1, 2, true), (2, 3, true)]),
            (Levels(0), &[(0, 1, false), (0, 3, false), (1, 3, false)]),
            (Levels(1), &[(0, 1, true), (0, 2, true), (1, 2, false)]),
            (Levels(2), &[(0, 3, true), (1, 3, true), (2, 3, false)]),
            (Levels(3), &[(0, 3, true), (1, 3, true), (2, 3, true)]),
            // Every level but the last: captions only.
            (
                Captions,
                &[
                    (0, 1, false),
                    (0, 2, true),
                    (1, 2, false),
                    (0, 3, true),
                    (1, 3, true),
                    (2, 3, false),
                ],
            ),
        ];
        for (depth, groups) in cases {
            let expansion = Expansion::<u32>::new(depth);
            assert_eq!(expansion.depth(), depth);
            for &(group_depth, levels, expanded) in groups {
                let values: Vec<i64> = (0..=i64::from(group_depth)).collect();
                let group = path(&values);
                assert_eq!(
                    expansion.is_group_expanded(&group, levels),
                    expanded,
                    "{depth:?}, depth {group_depth} of {levels}"
                );
                // Toggling the path inverts the default; toggling it back
                // to the default forgets it.
                let toggled = expansion
                    .clone()
                    .with_group(group.clone(), !expanded, levels);
                assert_eq!(toggled.is_group_expanded(&group, levels), !expanded);
                assert_eq!(toggled.toggled_groups().count(), 1);
                assert_ne!(toggled, expansion);
                let back = toggled.with_group(group.clone(), expanded, levels);
                assert_eq!(back.is_group_expanded(&group, levels), expanded);
                assert_eq!(back.toggled_groups().count(), 0);
                assert_eq!(back, expansion);
            }
            // Tree rows: `Captions` is `All`.
            for row_depth in 0..4u16 {
                let expected = match depth {
                    All | Captions => true,
                    Levels(n) => row_depth < n,
                };
                assert_eq!(expansion.is_row_expanded(&7, row_depth), expected);
                let toggled = expansion.clone().with_row(7, !expected, row_depth);
                assert_eq!(toggled.is_row_expanded(&7, row_depth), !expected);
                assert!(expansion.is_row_expanded(&8, row_depth) == expected);
                assert_eq!(toggled.toggled_rows().collect::<Vec<_>>(), [&7]);
            }
        }

        // A toggle survives a change of grouping: the same path is toggled
        // against whatever default the new grouping gives it.
        let collapsed = Expansion::<u32>::default().with_group(path(&[1, 2]), false, 2);
        assert!(!collapsed.is_group_expanded(&path(&[1, 2]), 3));
        assert!(collapsed.is_group_expanded(&path(&[1, 3]), 3));
        // Clones share the toggles until one changes.
        let clone = collapsed.clone();
        let changed = clone.with_group(path(&[1, 2]), true, 2);
        assert!(!collapsed.is_group_expanded(&path(&[1, 2]), 2));
        assert!(changed.is_group_expanded(&path(&[1, 2]), 2));
    }

    #[test]
    fn test_expand_depth_serde() {
        for (depth, json) in [
            (ExpandDepth::All, r#""all""#),
            (ExpandDepth::Captions, r#""captions""#),
            (ExpandDepth::Levels(0), "0"),
            (ExpandDepth::Levels(2), "2"),
        ] {
            assert_eq!(serde_json::to_string(&depth).unwrap(), json);
            assert_eq!(serde_json::from_str::<ExpandDepth>(json).unwrap(), depth);
        }
        for bad in [r#""deep""#, r#""All""#, "-1", "65536", "1.5", "null"] {
            assert!(serde_json::from_str::<ExpandDepth>(bad).is_err(), "{bad}");
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct Column;

    impl GridColumns for Column {
        fn all() -> &'static [Self] {
            &[Self]
        }

        fn id(self) -> &'static str {
            "value"
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(0)
        }
    }

    struct Row(u32);

    impl GridRow for Row {
        type Key = u32;
        type Column = Column;

        fn key(&self) -> &u32 {
            &self.0
        }

        fn value(&self, _: Column) -> CellValue<'_> {
            CellValue::Int(self.0.into())
        }
    }

    #[test]
    fn test_listed_lines() {
        // Two groups of a hand-built projection: {0, 1} expanded with a
        // footer, {2} collapsed. Rows of the collapsed group pass the view
        // but are not shown.
        let epoch = Instant::now();
        let mut ingest = Ingest::<Row>::new(HighlightPolicy::default(), epoch);
        ingest.apply((0..4).map(|id| Op::UpsertSilent(Arc::new(Row(id)))), epoch);
        let snapshot = ingest.snapshot();
        let ids: Vec<RowId> = (0..4).map(|key| snapshot.row_id(&key).unwrap()).collect();
        let mut columns = ColumnSet::EMPTY;
        columns.insert(ColumnIx::new(0));
        let group =
            |path: GroupPath, end: u32, rows: Range<u32>, expanded: bool, footer: bool| Group {
                path,
                parent: None,
                end,
                key_ix: end - 1,
                live: rows.end - rows.start,
                rows,
                line: u32::MAX,
                expanded,
                footer,
            };
        let groups = GroupData::new(
            1,
            vec![
                group(path(&[1]), 1, 0..2, true, true),
                group(path(&[2]), 2, 2..3, false, false),
            ],
            AggregateValues::new(columns, vec![OwnedValue::Int(1), OwnedValue::Int(2)]),
        );
        let (first, second) = (GroupId::new(0), GroupId::new(1));
        let lines = vec![
            DisplayRow::GroupCaption {
                group: first,
                depth: 0,
            },
            DisplayRow::Data {
                row: ids[0],
                depth: 1,
            },
            DisplayRow::Data {
                row: ids[1],
                depth: 1,
            },
            DisplayRow::GroupFooter {
                group: first,
                depth: 0,
            },
            DisplayRow::GroupCaption {
                group: second,
                depth: 0,
            },
        ];
        // The lines are laid out from the groups' flags.
        let projection = Projection::with_groups(&snapshot, ids[..3].to_vec(), groups)
            .with_totals(AggregateValues::new(columns, vec![OwnedValue::Int(3)]));
        assert!(projection.is_grouped() && !projection.is_tree());
        assert_eq!(projection.len(), 5);
        assert_eq!(
            (0..5)
                .map(|ix| projection.line(ix).unwrap())
                .collect::<Vec<_>>(),
            lines
        );
        assert_eq!(projection.row(0), None);
        assert_eq!(projection.row(2), Some(ids[1]));
        assert_eq!(projection.display_ix(ids[1]), Some(2));
        assert_eq!(projection.display_ix(ids[2]), None, "in a collapsed group");
        assert_eq!(projection.order_ix(ids[2]), Some(2));
        assert!(projection.contains(ids[2]) && !projection.contains(ids[3]));
        assert_eq!(projection.row_count(), 3);
        assert_eq!(projection.shown_rows().collect::<Vec<_>>(), ids[..2]);
        assert_eq!(projection.group_line(first), Some(0));
        assert_eq!(projection.group_line(second), Some(4));
        assert_eq!(
            projection.totals().get(ColumnIx::new(0)),
            Some(&OwnedValue::Int(3))
        );
        assert_eq!(projection.totals().get(ColumnIx::new(1)), None);
        assert_eq!(projection.branch(ids[0]), None);

        let table = projection.groups();
        assert_eq!(table.len(), 2);
        let group = table.find(&path(&[2])).expect("found by path");
        assert_eq!(group.id(), second);
        assert_eq!(group.rows(), &ids[2..3]);
        assert_eq!((group.row_count(), group.is_expanded()), (1, false));
        assert_eq!(
            group.aggregates().get(ColumnIx::new(0)),
            Some(&OwnedValue::Int(2))
        );
        let first = table.get(first).unwrap();
        assert_eq!(first.value(), &OwnedValue::Int(1));
        assert_eq!(first.level_id().as_str(), "a");
        assert_eq!(
            (first.depth(), first.parent(), first.has_footer()),
            (0, None, true)
        );
        assert!(table.find(&path(&[3])).is_none());
        assert_eq!(
            table
                .iter()
                .map(|group| group.id().ix())
                .collect::<Vec<_>>(),
            [0, 1]
        );

        // Copies for jobs keep everything.
        let copy = projection.duplicate();
        assert_eq!(copy.len(), 5);
        assert_eq!(copy.groups().len(), 2);
    }
}
