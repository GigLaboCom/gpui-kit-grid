//! Saved layouts on a grid: the view id, the layout codec and merge, the
//! schema's own defaults, and the methods that export a layout and restore a
//! saved one.
//!
//! A saved layout names columns by [`ColumnId`]; the index built here once
//! from the schema turns an id into a [`ColumnIx`]. Restoring asks the
//! [`LayoutMerge`] for one entry per schema column, validates the result
//! whatever the merge returned, and applies it through the column layout's
//! own operations, so every invariant of the column layout holds afterwards.
//!
//! A layout also holds the grouping by columns and the expansion of groups.
//! They are restored after the sort, on the one grouping path and the one
//! expansion path of the state (`apply_group_by`, `apply_expansion`), with
//! their events.

use std::{cmp::Ordering, hash::Hash};

use gpui_kit::{Context, Pixels, SharedString, Window, px};
use rustc_hash::FxHashMap;

use super::{
    ColumnOf, KeyOf, LiveGridDelegate, LiveGridState,
    columns::{ColumnLayout, ColumnZone, GridSchema, LayoutDelta, column_of},
    events::{GroupOrigin, LayoutOrigin, LayoutPart, SortOrigin},
};
use crate::{
    actions::ResetLayout,
    engine::{ExpandDepth, Expansion, GroupBy, GroupPath, SortDirection, TreeSpec, ViewSpec},
    model::{ColumnId, ColumnIx, ColumnSet, GridColumns, GridRow, OwnedValue},
    persist::{
        AppendNewColumns, GridLayout, JsonLayoutCodec, LayoutCodec, LayoutColumn, LayoutError,
        LayoutExpansion, LayoutGroup, LayoutMerge, LayoutMergeContext, LayoutReport, LayoutSort,
        ResolvedGrouping, check_version,
    },
    policy::{ColumnMovePolicy, ColumnOrder, FreeMove},
};

/// What a grid keeps for saved layouts: its view id, the codec and the
/// merge, its schema as declared, and the view and expansion depth given to
/// the builders.
pub(super) struct LayoutSlot<R: GridRow> {
    view_id: Option<SharedString>,
    codec: Box<dyn LayoutCodec>,
    merge: Box<dyn LayoutMerge>,
    /// `ColumnId` → place in `schema`, for every schema column. Built once,
    /// with the schema.
    index: FxHashMap<&'static str, usize>,
    /// Schema columns in the schema's own display order: declaration order
    /// split by zone, before any move policy.
    schema: Vec<ColumnIx>,
    /// The same columns with their default width, visibility and zone: what
    /// a [`LayoutMerge`] sees.
    defaults: Vec<LayoutColumn>,
    /// The bands of every column, by `ColumnIx`.
    bands: Vec<Box<[&'static str]>>,
    /// Schema columns the grid may not group by
    /// (`GridColumn::with_groupable(false)`): a saved level on one is
    /// dropped.
    ungroupable: Vec<ColumnId>,
    /// The sort given to `with_view`: the sort of the default layout.
    view_sort: Vec<(ColumnIx, SortDirection)>,
    /// How the view given to `with_view` arranges rows: the grouping of the
    /// default layout, and what `reset_layout` restores, a level by a
    /// function or a tree included.
    view_arrangement: Arrangement<R>,
    /// The depth given to `with_expand_depth`: the expansion of the default
    /// layout.
    view_depth: ExpandDepth,
    /// The layout given to `with_layout`, applied again by every builder it
    /// depends on. Only builders read it.
    initial: Option<GridLayout>,
}

impl<R: GridRow> LayoutSlot<R> {
    pub(super) fn new(schema: &GridSchema<R::Column>) -> Self {
        let declared = ColumnLayout::new(schema.clone());
        let columns = declared.order().to_vec();
        let ungroupable = columns
            .iter()
            .filter(|ix| !declared.column(**ix).is_groupable())
            .map(|ix| column_of::<R::Column>(*ix).column_id())
            .collect();
        Self {
            view_id: None,
            codec: Box::new(JsonLayoutCodec::default()),
            merge: Box::new(AppendNewColumns),
            index: columns
                .iter()
                .enumerate()
                .map(|(place, ix)| (column_of::<R::Column>(*ix).id(), place))
                .collect(),
            defaults: columns
                .iter()
                .map(|ix| {
                    layout_column::<R::Column>(
                        *ix,
                        declared.width(*ix),
                        declared.is_visible(*ix),
                        declared.zone_of(*ix),
                    )
                })
                .collect(),
            schema: columns,
            bands: R::Column::all()
                .iter()
                .map(|column| declared.bands_of(column.ix()).into())
                .collect(),
            ungroupable,
            view_sort: Vec::new(),
            view_arrangement: Arrangement::Flat,
            view_depth: ExpandDepth::default(),
            initial: None,
        }
    }

    /// Records the sort and the grouping or tree given to `with_view`, for
    /// the default layout.
    pub(super) fn set_view(&mut self, view: &ViewSpec<R>) {
        self.view_sort = view
            .sort()
            .iter()
            .map(|(column, direction)| (column.ix(), *direction))
            .collect();
        self.view_arrangement = Arrangement::of(view);
    }

    /// Records the depth given to `with_expand_depth`, for the default
    /// layout.
    pub(super) fn set_view_depth(&mut self, depth: ExpandDepth) {
        self.view_depth = depth;
    }

    /// The schema as a [`LayoutMerge`] and the report see it.
    fn context(&self) -> LayoutMergeContext<'_> {
        LayoutMergeContext::new(&self.defaults).with_ungroupable(&self.ungroupable)
    }

    /// The schema column with this id.
    fn column_ix(&self, id: &str) -> Option<ColumnIx> {
        self.index.get(id).map(|place| self.schema[*place])
    }

    /// An empty layout with the grid's view id.
    fn empty_layout(&self) -> GridLayout {
        match &self.view_id {
            Some(view_id) => GridLayout::new().with_view_id(view_id.to_string()),
            None => GridLayout::new(),
        }
    }

    /// Refuses a layout this build does not read, or one saved for another
    /// view. A layout or a grid without a view id is accepted.
    fn check(&self, layout: &GridLayout) -> Result<(), LayoutError> {
        check_version(layout.version())?;
        match (&self.view_id, layout.view_id()) {
            (Some(expected), Some(found)) if expected.as_ref() != found => {
                Err(LayoutError::ViewMismatch {
                    expected: expected.to_string(),
                    found: found.to_owned(),
                })
            }
            _ => Ok(()),
        }
    }

    /// Resolves `saved` against the schema: the merge, the validation, the
    /// sort, the grouping and the expansion, and the report of what did not
    /// fit.
    fn resolve(
        &self,
        columns: &ColumnLayout<R::Column>,
        policy: &dyn ColumnMovePolicy,
        saved: &GridLayout,
    ) -> (Resolved<R>, LayoutReport) {
        let context = self.context();
        let merged = self.merge.merge(&context, saved);
        let mut resolved = self.validate(columns, policy, &merged);
        resolved.sort = self.sort_keys(saved.sort());
        // The report reads the same resolution, so it names exactly what is
        // left out here.
        let grouping = ResolvedGrouping::new(&context, saved);
        let levels: Vec<GroupBy<R>> = grouping
            .levels()
            .iter()
            .filter_map(|level| {
                let ix = self.column_ix(level.column().as_str())?;
                Some(GroupBy::column(column_of(ix)).with_direction(level.direction()))
            })
            .collect();
        debug_assert_eq!(levels.len(), grouping.levels().len(), "kept levels group");
        // A saved path holds values only: the kept levels name their columns,
        // the top level first.
        resolved.toggled = grouping
            .toggled()
            .iter()
            .map(|path| {
                GroupPath::new(
                    levels
                        .iter()
                        .zip(path.iter())
                        .map(|(level, value)| (level.id(), value.clone())),
                )
            })
            .collect();
        resolved.depth = grouping.depth();
        resolved.arrangement = Arrangement::groups(levels);
        (resolved, LayoutReport::resolve(&context, saved))
    }

    /// Makes merged columns a valid column layout, whatever the merge
    /// returned: every schema column once (unknown ids and repeats are
    /// dropped, missing columns end their zone in schema order with schema
    /// defaults), widths within the column's limits (a width that is not a
    /// positive number becomes the schema width), the order split by zone
    /// and normalized by the move policy, and one column visible. No sort,
    /// no grouping, the default expansion.
    fn validate(
        &self,
        columns: &ColumnLayout<R::Column>,
        policy: &dyn ColumnMovePolicy,
        merged: &[LayoutColumn],
    ) -> Resolved<R> {
        let count = self.bands.len();
        let mut order = Vec::with_capacity(self.schema.len());
        let mut zones = vec![ColumnZone::Scrolling; count];
        let mut widths = vec![px(0.); count];
        let mut visible = ColumnSet::EMPTY;
        let mut seen = ColumnSet::EMPTY;
        // Missing columns come after the merged ones in schema order; the
        // sort by zone below puts each at the end of its zone.
        for entry in merged.iter().chain(&self.defaults) {
            let Some(place) = self.index.get(entry.id().as_str()).copied() else {
                continue;
            };
            let ix = self.schema[place];
            if seen.contains(ix) {
                continue;
            }
            seen.insert(ix);
            let width = match entry.width() {
                width if width.is_finite() && width > 0. => width,
                _ => self.defaults[place].width(),
            };
            widths[ix.get()] = columns.column(ix).clamp_width(px(width));
            zones[ix.get()] = entry.zone();
            // An internal column stays hidden whatever the layout says.
            if entry.is_visible() && !columns.is_internal(ix) {
                visible.insert(ix);
            }
            order.push(ix);
        }
        sort_by_zone(&mut order, &zones);
        policy.normalize(&mut ColumnOrder::new(&mut order, &mut zones, &self.bands));
        sort_by_zone(&mut order, &zones);
        if visible.is_empty()
            && let Some(&first) = order.iter().find(|ix| !columns.is_internal(**ix))
        {
            visible.insert(first);
        }
        Resolved {
            order,
            zones,
            widths,
            visible,
            sort: Vec::new(),
            arrangement: Arrangement::Flat,
            depth: ExpandDepth::default(),
            toggled: Vec::new(),
        }
    }

    /// The schema's own layout, resolved: its columns normalized by
    /// `policy`, the sort and the grouping or tree of the view given to
    /// `with_view`, and the depth given to `with_expand_depth` with no group
    /// toggled.
    fn default_resolved(
        &self,
        columns: &ColumnLayout<R::Column>,
        policy: &dyn ColumnMovePolicy,
    ) -> Resolved<R> {
        let mut resolved = self.validate(columns, policy, &self.defaults);
        resolved.sort = self.view_sort.clone();
        resolved.arrangement = self.view_arrangement.clone();
        resolved.depth = self.view_depth;
        resolved
    }

    /// Saved sort keys on schema columns, each column once (the first key
    /// wins).
    fn sort_keys(&self, sort: &[LayoutSort]) -> Vec<(ColumnIx, SortDirection)> {
        let mut sorted = ColumnSet::EMPTY;
        let mut keys = Vec::with_capacity(sort.len());
        for key in sort {
            if let Some(ix) = self.column_ix(key.column().as_str())
                && !sorted.contains(ix)
            {
                sorted.insert(ix);
                keys.push((ix, key.direction()));
            }
        }
        keys
    }
}

/// How a restored layout arranges rows.
enum Arrangement<R: GridRow> {
    /// No grouping levels: grouping is off, and a tree stays (a layout saved
    /// before grouping existed does not turn the application's tree off).
    Flat,
    /// Grouped by these levels, the top one first: they replace a tree (a
    /// tree and a grouping exclude each other).
    Groups(Vec<GroupBy<R>>),
    /// The tree of the view given to `with_view`: no saved layout holds one
    /// (row keys are not serializable, and the application sets up its
    /// tree), only the default layout restores it.
    Tree(TreeSpec<R>),
}

impl<R: GridRow> Clone for Arrangement<R> {
    fn clone(&self) -> Self {
        match self {
            Self::Flat => Self::Flat,
            Self::Groups(levels) => Self::Groups(levels.clone()),
            Self::Tree(tree) => Self::Tree(tree.clone()),
        }
    }
}

impl<R: GridRow> Arrangement<R> {
    /// The arrangement of a view.
    fn of(view: &ViewSpec<R>) -> Self {
        match view.tree() {
            Some(tree) => Self::Tree(tree.clone()),
            None => Self::groups(view.group_by().to_vec()),
        }
    }

    /// Grouped by `levels`; flat without levels.
    fn groups(levels: Vec<GroupBy<R>>) -> Self {
        if levels.is_empty() {
            Self::Flat
        } else {
            Self::Groups(levels)
        }
    }

    /// The grouping levels; none when flat or a tree.
    fn levels(&self) -> &[GroupBy<R>] {
        match self {
            Self::Groups(levels) => levels,
            Self::Flat | Self::Tree(_) => &[],
        }
    }
}

/// A saved layout resolved against the schema and validated: what restoring
/// applies.
struct Resolved<R: GridRow> {
    /// Every schema column in display order, split by zone.
    order: Vec<ColumnIx>,
    /// By `ColumnIx`.
    zones: Vec<ColumnZone>,
    /// By `ColumnIx`, within each column's limits.
    widths: Vec<Pixels>,
    visible: ColumnSet,
    /// Schema columns only, each once.
    sort: Vec<(ColumnIx, SortDirection)>,
    /// From a saved layout, levels on groupable schema columns only, each
    /// once; for the default layout, the view's own.
    arrangement: Arrangement<R>,
    /// The default depth of the expansion.
    depth: ExpandDepth,
    /// Groups toggled against `depth`, each a group of the levels of
    /// `arrangement`.
    toggled: Vec<GroupPath>,
}

impl<R: GridRow> Resolved<R> {
    /// The resolved layout as a `GridLayout` of `slot`'s view.
    fn to_layout(&self, slot: &LayoutSlot<R>) -> GridLayout {
        let layout = self
            .order
            .iter()
            .map(|ix| {
                layout_column::<R::Column>(
                    *ix,
                    self.widths[ix.get()],
                    self.visible.contains(*ix),
                    self.zones[ix.get()],
                )
            })
            .fold(slot.empty_layout(), GridLayout::with_column)
            .with_sort(self.sort.iter().map(|(ix, direction)| {
                LayoutSort::new(column_of::<R::Column>(*ix).column_id(), *direction)
            }));
        if let Arrangement::Tree(_) = self.arrangement {
            return layout;
        }
        let levels = self.arrangement.levels();
        let expansion = toggle_groups(
            Expansion::<R::Key>::new(self.depth),
            self.toggled.iter().cloned(),
            levels.len(),
        );
        with_saved_grouping(layout, levels, &expansion)
    }

    /// Sets `columns` to the resolved order, zones, widths and visibility,
    /// one column at a time through the column layout's operations. Returns
    /// what changed; the sort, the grouping and the expansion are not
    /// applied here.
    fn apply(&self, columns: &mut ColumnLayout<R::Column>) -> LayoutDelta {
        fn mark(delta: &mut LayoutDelta, part: LayoutPart, ix: ColumnIx) {
            delta.parts = delta.parts.with(part);
            delta.columns.insert(ix);
        }

        let before = columns.order().to_vec();
        let mut delta = LayoutDelta::default();
        for &ix in &self.order {
            let zone = self.zones[ix.get()];
            if columns.zone_of(ix) != zone {
                // One column at a time: the order within zones is set next.
                columns.set_zone(ix, zone, &FreeMove);
                mark(&mut delta, LayoutPart::Zone, ix);
            }
        }
        for zone in ColumnZone::ALL {
            let members = self.order.iter().filter(|ix| self.zones[ix.get()] == zone);
            for (gap, &ix) in members.enumerate() {
                // The columns before `gap` are in place: `ix` is at `gap` or
                // later, and lands at `gap`.
                columns.move_members(zone, &[ix], gap);
            }
        }
        let moved = moved_columns(&before, columns.order());
        if !moved.is_empty() {
            delta.parts = delta.parts.with(LayoutPart::Order);
            delta.columns.union_with(moved);
        }
        for &ix in &self.order {
            if columns.set_width(ix, self.widths[ix.get()]) {
                mark(&mut delta, LayoutPart::Width, ix);
            }
        }
        // Show before hiding: the last visible column cannot be hidden.
        for show in [true, false] {
            for &ix in &self.order {
                if self.visible.contains(ix) == show && columns.set_visible(ix, show) == Some(true)
                {
                    mark(&mut delta, LayoutPart::Visibility, ix);
                }
            }
        }
        debug_assert_eq!(columns.order(), self.order, "the restored order");
        delta
    }
}

/// `layout` with the grouping `levels` and the expansion of their groups,
/// as a layout saves them: the levels, the default depth, and the groups of
/// these levels toggled against it in the order of their values
/// (`OwnedValue::cmp_total`), so that a file does not change with the order
/// of a hash set. Toggles of other groupings are not saved.
///
/// A grouping with a level by a function of the row (`GroupKey`) is not
/// saved, nor its expansion: a function cannot be read back from a file, and
/// a part of the grouping would be a wrong one.
fn with_saved_grouping<R: GridRow>(
    layout: GridLayout,
    levels: &[GroupBy<R>],
    expansion: &Expansion<R::Key>,
) -> GridLayout {
    let Some(groups) = levels
        .iter()
        .map(|level| {
            let column = level.as_column()?;
            Some(LayoutGroup::new(column.column_id(), level.direction()))
        })
        .collect::<Option<Vec<_>>>()
    else {
        return layout;
    };
    let mut toggled: Vec<Vec<OwnedValue>> = expansion
        .toggled_groups()
        .filter(|path| is_group_of(path, levels))
        .map(|path| {
            path.levels()
                .iter()
                .map(|(_, value)| value.clone())
                .collect()
        })
        .collect();
    toggled.sort_by(|a, b| cmp_values(a, b));
    let expansion = toggled.into_iter().fold(
        LayoutExpansion::new(expansion.depth()),
        |expansion, path| expansion.with_toggled(path),
    );
    layout.with_group_by(groups).with_expansion(expansion)
}

/// Returns true if `path` names a group of the grouping `levels`: a group of
/// one of its levels, whose level ids are the first ids of `levels`.
fn is_group_of<R: GridRow>(path: &GroupPath, levels: &[GroupBy<R>]) -> bool {
    !path.is_root()
        && path.levels().len() <= levels.len()
        && path
            .levels()
            .iter()
            .zip(levels)
            .all(|((id, _), level)| *id == level.id())
}

/// Orders key values of saved paths: value by value (`cmp_total`), then a
/// shorter path first.
fn cmp_values(a: &[OwnedValue], b: &[OwnedValue]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.cmp_total(b))
        .find(|order| order.is_ne())
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}

/// Toggles each group of `paths` against the depth of `expansion`, in a
/// grouping of `levels` levels. A group given twice stays toggled.
fn toggle_groups<K: Clone + Eq + Hash>(
    expansion: Expansion<K>,
    paths: impl IntoIterator<Item = GroupPath>,
    levels: usize,
) -> Expansion<K> {
    paths.into_iter().fold(expansion, |expansion, path| {
        set_toggled(expansion, path, true, levels)
    })
}

/// Toggles a group against the depth of `expansion`, or clears its toggle.
fn set_toggled<K: Clone + Eq + Hash>(
    expansion: Expansion<K>,
    path: GroupPath,
    toggled: bool,
    levels: usize,
) -> Expansion<K> {
    // Expanded by the depth alone, before any toggle.
    let by_default = Expansion::<()>::new(expansion.depth()).is_group_expanded(&path, levels);
    expansion.with_group(path, by_default != toggled, levels)
}

/// One column of a layout, by the column's id.
fn layout_column<C: GridColumns>(
    ix: ColumnIx,
    width: Pixels,
    visible: bool,
    zone: ColumnZone,
) -> LayoutColumn {
    LayoutColumn::new(column_of::<C>(ix).column_id(), width)
        .with_visible(visible)
        .with_zone(zone)
}

/// Every column of a column layout in display order, hidden ones included.
fn layout_columns<C: GridColumns>(
    columns: &ColumnLayout<C>,
) -> impl Iterator<Item = LayoutColumn> + '_ {
    columns.order().iter().map(|ix| {
        layout_column::<C>(
            *ix,
            columns.width(*ix),
            columns.is_visible(*ix),
            columns.zone_of(*ix),
        )
    })
}

/// A stable sort by zone, left zone first.
fn sort_by_zone(order: &mut [ColumnIx], zones: &[ColumnZone]) {
    order.sort_by_key(|ix| zones[ix.get()].slot());
}

/// The columns that moved between two orders of the same columns: those
/// outside a longest run that kept its relative order.
fn moved_columns(before: &[ColumnIx], after: &[ColumnIx]) -> ColumnSet {
    let mut moved = ColumnSet::EMPTY;
    if before == after {
        return moved;
    }
    let mut position = [usize::MAX; 128];
    for (place, ix) in before.iter().enumerate() {
        position[ix.get()] = place;
    }
    let places: Vec<usize> = after.iter().map(|ix| position[ix.get()]).collect();
    // The longest increasing run of old places, by patience sorting.
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; places.len()];
    for (i, place) in places.iter().enumerate() {
        let k = tails.partition_point(|&tail| places[tail] < *place);
        previous[i] = k.checked_sub(1).map(|k| tails[k]);
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut kept = ColumnSet::EMPTY;
    let mut at = tails.last().copied();
    while let Some(i) = at {
        kept.insert(after[i]);
        at = previous[i];
    }
    for ix in after {
        if !kept.contains(*ix) {
            moved.insert(*ix);
        }
    }
    moved
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Names the kind of grid ("trades") for saved layouts and the layout
    /// library. Every grid of a view shares the view id; a layout saved for
    /// another view is refused ([`LayoutError::ViewMismatch`]).
    pub fn with_view_id(mut self, view_id: impl Into<SharedString>) -> Self {
        self.layout.view_id = Some(view_id.into());
        self.apply_initial_layout()
    }

    /// Applies a saved layout at construction, without events: its columns,
    /// its sort in place of the sort of `with_view` (an empty sort too; the
    /// filter stays), its grouping in place of the grouping of `with_view`
    /// (no grouping too; a tree of `with_view` stays when the layout has no
    /// grouping), and its expansion in place of the depth of
    /// `with_expand_depth` (a tree keeps that depth). A layout this build
    /// does not read, or one saved for another view, is ignored; call
    /// [`Self::set_layout`] to see why. Called again, the new layout
    /// replaces the old one.
    ///
    /// The builders it depends on (`with_view`, `with_view_id`,
    /// `with_expand_depth`, `with_column_move_policy` and
    /// `with_layout_merge`) may come before or after it: each applies the
    /// layout again after its own change.
    pub fn with_layout(mut self, layout: GridLayout) -> Self {
        self.layout.initial = Some(layout);
        self.apply_initial_layout()
    }

    /// Applies the layout given to [`Self::with_layout`] to the columns, the
    /// sort, the grouping and the expansion, or the default layout if it is
    /// refused; does nothing without one. Every builder the result depends
    /// on calls it after its own change, so the builder order does not
    /// matter. Each part is set from the layout and what the builders
    /// recorded (`LayoutSlot`), never from an earlier application, so
    /// nothing of one is left.
    pub(super) fn apply_initial_layout(mut self) -> Self {
        let Some(layout) = &self.layout.initial else {
            return self;
        };
        let resolved = match self.layout.check(layout) {
            Ok(()) => {
                self.layout
                    .resolve(&self.columns, &*self.column_move, layout)
                    .0
            }
            Err(_) => self
                .layout
                .default_resolved(&self.columns, &*self.column_move),
        };
        // No scroll anchor: a grid being built shows its columns from the
        // start.
        resolved.apply(&mut self.columns);
        let sort: Vec<(ColumnOf<D>, SortDirection)> = resolved
            .sort
            .iter()
            .map(|(ix, direction)| (column_of(*ix), *direction))
            .collect();
        let view = std::mem::take(&mut self.view).with_sort(sort);
        let view = match resolved.arrangement {
            Arrangement::Groups(levels) => view.with_group_by(levels),
            Arrangement::Tree(tree) => view.with_tree(tree),
            // No levels keep the tree of `with_view`.
            Arrangement::Flat => match &self.layout.view_arrangement {
                Arrangement::Tree(tree) => view.with_tree(tree.clone()),
                Arrangement::Flat | Arrangement::Groups(_) => view.with_group_by([]),
            },
        };
        // The expansion of a tree is the tree's: a layout does not hold it,
        // as row keys are not serializable.
        let expansion = match view.tree() {
            Some(_) => Expansion::new(self.layout.view_depth),
            None => toggle_groups(
                Expansion::new(resolved.depth),
                resolved.toggled,
                view.group_by().len(),
            ),
        };
        // Nothing was laid out yet: no expansion revision to count.
        self.view = view.with_expansion(expansion);
        // A quick filter of the visible columns follows them.
        self.recompile_quick_filter();
        self
    }

    /// Sets the text format of [`Self::export_layout`] and
    /// [`Self::import_layout`] (default [`JsonLayoutCodec`]).
    pub fn with_layout_codec(mut self, codec: impl LayoutCodec) -> Self {
        self.layout.codec = Box::new(codec);
        self
    }

    /// Sets how a saved layout meets the current schema (default
    /// [`AppendNewColumns`]). The grid validates what the merge returns.
    pub fn with_layout_merge(mut self, merge: impl LayoutMerge) -> Self {
        self.layout.merge = Box::new(merge);
        self.apply_initial_layout()
    }

    /// The view id given to [`Self::with_view_id`].
    pub fn view_id(&self) -> Option<&SharedString> {
        self.layout.view_id.as_ref()
    }

    /// The current layout: every schema column in display order (hidden ones
    /// at their place) with its width in logical pixels, visibility and
    /// zone, the sort, the grouping levels, and the expansion of groups: the
    /// default depth ([`Self::expand_depth`]) and the groups of the current
    /// grouping toggled against it. The quick filter, scrolling, selection
    /// and focus are not part of it.
    ///
    /// A grouping with a level by a function of the row (`GroupKey`) is not
    /// saved, nor the expansion: a function cannot be read back from a file.
    /// A tree is not saved either, nor its expansion: the application sets
    /// its tree, and row keys are the application's own type. Toggled groups
    /// of another grouping are kept by the grid until it is dropped, not
    /// saved.
    ///
    /// Reads state only, so it works for a grid that was never painted, such
    /// as one in a hidden dock tab.
    pub fn layout(&self) -> GridLayout {
        let layout = layout_columns(&self.columns)
            .fold(self.layout.empty_layout(), GridLayout::with_column)
            .with_sort(
                self.view
                    .sort()
                    .iter()
                    .map(|(column, direction)| LayoutSort::new(column.column_id(), *direction)),
            );
        match self.view.tree() {
            Some(_) => layout,
            None => with_saved_grouping(layout, self.view.group_by(), self.view.expansion()),
        }
    }

    /// The layout of the schema: its order normalized by the column move
    /// policy, its widths, visibility and zones, the sort and the grouping
    /// of the view given to `with_view`, and the depth given to
    /// `with_expand_depth` with no group toggled. Saved as [`Self::layout`]
    /// saves: without a grouping by a function, or a tree.
    /// [`Self::reset_layout`] restores it.
    pub fn default_layout(&self) -> GridLayout {
        self.layout
            .default_resolved(&self.columns, &*self.column_move)
            .to_layout(&self.layout)
    }

    /// The current layout as text, through the layout codec.
    pub fn export_layout(&self) -> Result<String, LayoutError> {
        self.layout.codec.encode(&self.layout())
    }

    /// Restores a saved layout: the columns, then the sort, the grouping and
    /// the expansion. Saved columns the schema no longer has are ignored and
    /// new schema columns are added (by the layout merge, default
    /// [`AppendNewColumns`]); the report lists both. Widths are kept within
    /// the columns' limits.
    ///
    /// Grouping levels on columns the schema no longer has, cannot group or
    /// repeat are dropped, and the levels below move up; toggled groups
    /// deeper than the levels left are dropped (the report lists both). A
    /// layout without grouping turns grouping off and leaves a tree alone; a
    /// layout with grouping replaces a tree. The expansion of a tree is not
    /// restored: a layout does not hold it. The toggles of other groupings
    /// stay while the depth is the same.
    ///
    /// Rebuilds the order once if the sort or the grouping changed, or the
    /// visible columns that a quick filter searches; a new expansion alone
    /// only lays the lines out again. Emits, when everything has changed:
    /// `OrderHeldChange { held: false }` if that rebuild drops a held order,
    /// `SortChange` if the sort changed, `GroupChange` if the grouping did,
    /// `ExpansionChange { target: None }` if the expansion did,
    /// `LayoutChange` if the columns changed (all `Programmatic`), then
    /// `FocusChange` if the focused column is now hidden. A layout equal to
    /// the current one emits nothing. Fails, changing nothing, for a version
    /// this build does not read or a layout of another view.
    pub fn set_layout(
        &mut self,
        layout: &GridLayout,
        cx: &mut Context<Self>,
    ) -> Result<LayoutReport, LayoutError> {
        self.restore_layout(layout, LayoutOrigin::Programmatic, cx)
    }

    /// Reads a layout through the layout codec and restores it with
    /// [`Self::set_layout`].
    pub fn import_layout(
        &mut self,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Result<LayoutReport, LayoutError> {
        let layout = self.layout.codec.decode(text)?;
        self.set_layout(&layout, cx)
    }

    /// Restores [`Self::default_layout`] as [`Self::set_layout`] does, with
    /// the grouping or tree of the view given to `with_view` as it was given:
    /// a level by a function too. The `ResetLayout` Action does the same with
    /// the `User` origin.
    pub fn reset_layout(&mut self, cx: &mut Context<Self>) {
        self.reset_layout_from(LayoutOrigin::Programmatic, cx);
    }

    fn reset_layout_from(&mut self, origin: LayoutOrigin, cx: &mut Context<Self>) {
        let resolved = self
            .layout
            .default_resolved(&self.columns, &*self.column_move);
        self.apply_resolved(resolved, origin, cx);
    }

    fn restore_layout(
        &mut self,
        layout: &GridLayout,
        origin: LayoutOrigin,
        cx: &mut Context<Self>,
    ) -> Result<LayoutReport, LayoutError> {
        self.layout.check(layout)?;
        let (resolved, report) = self
            .layout
            .resolve(&self.columns, &*self.column_move, layout);
        self.apply_resolved(resolved, origin, cx);
        Ok(report)
    }

    /// Applies a resolved layout in this order: the columns without an
    /// event, the sort, the grouping, the expansion, last the
    /// `LayoutChange`. Each event comes when everything has changed, and one
    /// rebuild covers all of it.
    fn apply_resolved(
        &mut self,
        resolved: Resolved<D::Row>,
        origin: LayoutOrigin,
        cx: &mut Context<Self>,
    ) {
        let visible = self.columns.visible_set();
        // The columns first, without an event, keeping the scroll anchor.
        let delta = self.relayout(|columns, _| resolved.apply(columns));
        // A quick filter of the visible columns follows them. Compiled
        // before the sort, so that one rebuild covers both.
        let refilter = self.columns.visible_set() != visible && self.follow_visible_columns();
        // Then the sort and the grouping, as user actions of their paths.
        let (sort_origin, group_origin) = match origin {
            LayoutOrigin::User => (SortOrigin::User, GroupOrigin::User),
            LayoutOrigin::Programmatic => (SortOrigin::Programmatic, GroupOrigin::Programmatic),
        };
        let sorted = self.apply_sort(
            resolved
                .sort
                .iter()
                .map(|(ix, direction)| (column_of(*ix), *direction)),
            sort_origin,
            cx,
        );
        let grouped = self.apply_arrangement(resolved.arrangement, group_origin, cx);
        if refilter && !sorted && !grouped {
            // The rebuild a sort change would have started. The scroll
            // position stays: the columns changed, not the view.
            self.restart_projection(cx);
        }
        // Then the expansion, in one change: the pending rebuild lays it out.
        // The expansion of a tree is the tree's.
        if self.view.tree().is_none() {
            let expansion = self.restored_expansion(resolved.depth, resolved.toggled);
            self.apply_expansion(expansion, None, group_origin, cx);
        }
        if !delta.is_empty() {
            // A focus on a column the layout hid moves off it after the
            // `LayoutChange`.
            self.emit_layout_change(delta, origin, cx);
            cx.notify();
        }
    }

    /// Restores a grouping on the one grouping path (`apply_group_by`) or
    /// a tree on the one tree path (`apply_tree`): `GroupChange` if it
    /// changed, after an `OrderHeldChange { held: false }` if a held order
    /// is dropped. Returns true if it changed.
    fn apply_arrangement(
        &mut self,
        arrangement: Arrangement<D::Row>,
        origin: GroupOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        match arrangement {
            Arrangement::Flat => self.apply_group_by(Vec::new(), origin, cx),
            Arrangement::Groups(levels) => self.apply_group_by(levels, origin, cx),
            // The tree of `with_view`, back after a grouping replaced it: only
            // `reset_layout` restores a tree, on the one tree path.
            Arrangement::Tree(tree) => {
                if self
                    .view
                    .tree()
                    .is_some_and(|shown| shown.column() == tree.column())
                {
                    return false;
                }
                self.apply_tree(Some(tree), origin, cx)
            }
        }
    }

    /// The expansion a restored layout leaves on the grouping now in the
    /// view: the groups of `toggled` toggled against `depth`. The same depth
    /// keeps the toggles of other groupings and of tree rows, so going back
    /// to a grouping still brings its expansion back; a new depth clears
    /// them, as `set_expand_depth` does.
    fn restored_expansion(
        &self,
        depth: ExpandDepth,
        toggled: Vec<GroupPath>,
    ) -> Expansion<KeyOf<D>> {
        let current = self.view.expansion();
        let levels = self.view.group_by();
        let kept = if current.depth() == depth {
            let own: Vec<GroupPath> = current
                .toggled_groups()
                .filter(|path| is_group_of(path, levels))
                .cloned()
                .collect();
            own.into_iter().fold(current.clone(), |expansion, path| {
                set_toggled(expansion, path, false, levels.len())
            })
        } else {
            Expansion::new(depth)
        };
        toggle_groups(kept, toggled, levels.len())
    }

    pub(super) fn on_reset_layout(
        &mut self,
        _: &ResetLayout,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_layout_from(LayoutOrigin::User, cx);
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::px;

    use super::{Arrangement, LayoutSlot, cmp_values, moved_columns, with_saved_grouping};
    use crate::{
        engine::{ExpandDepth, Expansion, GroupBy, GroupKey, GroupPath, SortDirection},
        model::{CellValue, ColumnIx, ColumnSet, GridColumns, GridRow, OwnedValue},
        persist::{
            GridLayout, LayoutColumn, LayoutExpansion, LayoutGroup, LayoutMerge,
            LayoutMergeContext, LayoutSort,
        },
        policy::{BandedMove, ColumnMovePolicy, FreeMove},
        state::{
            ColumnZone, MIN_COLUMN_WIDTH,
            columns::{ColumnBand, ColumnLayout, GridColumn, GridSchema},
            events::LayoutPart,
        },
    };

    /// Up to 16 columns `c0`..`c15`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct TestColumn(u16);

    static ALL: [TestColumn; 16] = {
        let mut all = [TestColumn(0); 16];
        let mut ix = 0;
        while ix < 16 {
            all[ix] = TestColumn(ix as u16);
            ix += 1;
        }
        all
    };

    const IDS: [&str; 16] = [
        "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12", "c13",
        "c14", "c15",
    ];
    const BANDS: [&str; 12] = [
        "b0", "b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8", "b9", "b10", "b11",
    ];

    impl GridColumns for TestColumn {
        fn all() -> &'static [Self] {
            &ALL
        }

        fn id(self) -> &'static str {
            IDS[self.0 as usize]
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self.0)
        }
    }

    /// Rows of [`TestColumn`]s: a slot reads the row's types only.
    struct TestRow;

    impl GridRow for TestRow {
        type Key = u32;
        type Column = TestColumn;

        fn key(&self) -> &u32 {
            &0
        }

        fn value(&self, _: TestColumn) -> CellValue<'_> {
            CellValue::Empty
        }
    }

    fn ix(i: u16) -> ColumnIx {
        ColumnIx::new(i)
    }

    fn column(i: u16) -> GridColumn<TestColumn> {
        GridColumn::new(TestColumn(i), IDS[i as usize])
    }

    /// A column layout as a grid builds it: the schema normalized by
    /// `policy`.
    fn built(
        schema: &GridSchema<TestColumn>,
        policy: &dyn ColumnMovePolicy,
    ) -> ColumnLayout<TestColumn> {
        let mut columns = ColumnLayout::new(schema.clone());
        columns.normalize(policy);
        columns
    }

    fn exported(columns: &ColumnLayout<TestColumn>) -> Vec<LayoutColumn> {
        super::layout_columns(columns).collect()
    }

    fn order(columns: &ColumnLayout<TestColumn>) -> Vec<&'static str> {
        columns
            .order()
            .iter()
            .map(|ix| IDS[ix.get()])
            .collect::<Vec<_>>()
    }

    fn ids(ids: &[crate::model::ColumnId]) -> Vec<&str> {
        ids.iter().map(|id| id.as_str()).collect()
    }

    /// A merge that ignores the rules: it returns `self.0` as it is.
    struct Verbatim(Vec<LayoutColumn>);

    impl LayoutMerge for Verbatim {
        fn merge(&self, _: &LayoutMergeContext<'_>, _: &GridLayout) -> Vec<LayoutColumn> {
            self.0.clone()
        }
    }

    #[test]
    fn test_layout_validation() {
        // c0 frozen left; band b0 over c1, c2 (c2 has limits); c3; band b1
        // over c4, c5; c6 frozen right; c7.
        let schema = GridSchema::new()
            .column(
                column(0)
                    .with_width(px(50.))
                    .with_zone(ColumnZone::FrozenLeft),
            )
            .band(
                ColumnBand::new("b0", "B0").column(column(1)).column(
                    column(2)
                        .with_width(px(80.))
                        .with_min_width(px(40.))
                        .with_max_width(px(120.)),
                ),
            )
            .column(column(3).with_width(px(70.)))
            .band(
                ColumnBand::new("b1", "B1")
                    .column(column(4))
                    .column(column(5)),
            )
            .column(
                column(6)
                    .with_width(px(60.))
                    .with_zone(ColumnZone::FrozenRight),
            )
            .column(column(7).with_width(px(40.)));
        let mut columns = built(&schema, &BandedMove);
        let mut slot = LayoutSlot::<TestRow>::new(&schema);
        assert_eq!(slot.column_ix("c4"), Some(ix(4)));
        assert_eq!(slot.column_ix("c8"), None, "not in the schema");

        // A merge that breaks every rule: an unknown id, repeats, c0, c1, c3
        // and c7 missing, bands split across zones and broken up, widths out
        // of the limits or not numbers, every column hidden.
        let hidden =
            |id: &'static str, width: f32| LayoutColumn::new(id, width).with_visible(false);
        slot.merge = Box::new(Verbatim(vec![
            hidden("c5", f32::NAN).with_zone(ColumnZone::FrozenLeft),
            hidden("gone", 70.),
            hidden("c2", 500.),
            hidden("c4", 3.),
            hidden("c2", 90.),
            hidden("c6", -5.).with_zone(ColumnZone::FrozenRight),
            hidden("c5", 77.),
        ]));
        let saved = GridLayout::new()
            .with_column(LayoutColumn::new("c3", 70.))
            .with_column(LayoutColumn::new("gone", 70.))
            .with_sort([
                LayoutSort::new("c4", SortDirection::Descending),
                LayoutSort::new("gone", SortDirection::Ascending),
                LayoutSort::new("c4", SortDirection::Ascending),
                LayoutSort::new("c0", SortDirection::Ascending),
            ]);
        let (resolved, report) = slot.resolve(&columns, &BandedMove, &saved);

        // Every schema column once; the missing ones end their zone in
        // schema order (c3 before c7). Band b1 takes the zone of its first
        // column (c5, frozen left) and gathers there; band b0 gathers at c2.
        let resolved_order: Vec<&str> = resolved.order.iter().map(|ix| IDS[ix.get()]).collect();
        assert_eq!(
            resolved_order,
            ["c5", "c4", "c0", "c2", "c1", "c3", "c7", "c6"]
        );
        assert_eq!(resolved.zones[4], ColumnZone::FrozenLeft);
        // Widths within the limits; NaN and negative give the schema width.
        assert_eq!(resolved.widths[2], px(120.), "max width");
        assert_eq!(resolved.widths[4], MIN_COLUMN_WIDTH, "min width");
        assert_eq!(resolved.widths[5], px(100.), "NaN: schema width");
        assert_eq!(resolved.widths[6], px(60.), "negative: schema width");
        // The missing columns come with their schema visibility.
        assert_eq!(resolved.visible, set([0, 1, 3, 7]));
        // Sort keys on schema columns, the first key of a column wins.
        assert_eq!(
            resolved.sort,
            [
                (ix(4), SortDirection::Descending),
                (ix(0), SortDirection::Ascending)
            ]
        );
        // The report compares the saved layout with the schema, not the
        // merge's result.
        assert_eq!(ids(report.ignored_columns()), ["gone"]);
        assert_eq!(
            ids(report.added_columns()),
            ["c0", "c1", "c2", "c4", "c5", "c7", "c6"]
        );
        assert_eq!(ids(report.dropped_sort()), ["gone", "c4"]);

        // Applying gives exactly the resolved layout.
        let delta = resolved.apply(&mut columns);
        assert_eq!(order(&columns), resolved_order);
        assert_eq!(columns.width(ix(2)), px(120.));
        assert_eq!(columns.zone_of(ix(4)), ColumnZone::FrozenLeft);
        assert_eq!(columns.visible_set(), set([0, 1, 3, 7]));
        for part in [
            LayoutPart::Width,
            LayoutPart::Order,
            LayoutPart::Visibility,
            LayoutPart::Zone,
        ] {
            assert!(delta.parts.contains(part), "{part:?}");
        }

        // With every column hidden and none missing, the first in display
        // order is shown.
        slot.merge = Box::new(Verbatim(
            (0..8)
                .map(|i| hidden(IDS[i], 50.))
                .collect::<Vec<LayoutColumn>>(),
        ));
        let (resolved, _) = slot.resolve(&columns, &FreeMove, &GridLayout::new());
        assert_eq!(resolved.order[0], ix(0));
        assert_eq!(resolved.visible, set([0]));
        resolved.apply(&mut columns);
        assert_eq!(columns.visible_set(), set([0]));

        // A layout equal to the current one changes nothing.
        let (resolved, _) = slot.resolve(&columns, &FreeMove, &GridLayout::new());
        assert!(resolved.apply(&mut columns).is_empty());
    }

    fn set(columns: impl IntoIterator<Item = u16>) -> ColumnSet {
        columns.into_iter().fold(ColumnSet::EMPTY, |mut set, i| {
            set.insert(ix(i));
            set
        })
    }

    #[test]
    fn test_moved_columns() {
        let order = |ids: &[u16]| ids.iter().map(|i| ix(*i)).collect::<Vec<_>>();
        let before = order(&[0, 1, 2, 3, 4]);
        assert!(moved_columns(&before, &before).is_empty());
        // One column dragged to the front: only it moved.
        assert_eq!(moved_columns(&before, &order(&[4, 0, 1, 2, 3])), set([4]));
        assert_eq!(moved_columns(&before, &order(&[1, 2, 0, 3, 4])), set([0]));
        // Two swapped: one of them moved.
        assert_eq!(moved_columns(&before, &order(&[0, 3, 2, 1, 4])).len(), 2);
        assert_eq!(
            moved_columns(&before, &order(&[4, 3, 2, 1, 0])).len(),
            4,
            "a reversal keeps one column"
        );
    }

    /// A random schema of up to 12 columns `c0`.. in bands up to two deep,
    /// with random zones, widths, limits and visibility.
    fn random_schema(rng: &mut fastrand::Rng) -> (GridSchema<TestColumn>, usize) {
        fn items(
            rng: &mut fastrand::Rng,
            level: usize,
            next_column: &mut u16,
            next_band: &mut usize,
            limit: u16,
        ) -> Vec<Result<GridColumn<TestColumn>, ColumnBand<TestColumn>>> {
            let mut out = Vec::new();
            for _ in 0..rng.usize(1..=4) {
                if *next_column >= limit {
                    break;
                }
                if level < 2 && *next_band < BANDS.len() && rng.u8(..3) == 0 {
                    let id = BANDS[*next_band];
                    *next_band += 1;
                    let children = items(rng, level + 1, next_column, next_band, limit);
                    out.push(Err(children.into_iter().fold(
                        ColumnBand::new(id, id),
                        |band, child| match child {
                            Ok(column) => band.column(column),
                            Err(nested) => band.band(nested),
                        },
                    )));
                } else {
                    let i = *next_column;
                    *next_column += 1;
                    out.push(Ok(random_column(rng, i)));
                }
            }
            out
        }
        let limit = rng.u16(1..=12);
        let (mut next_column, mut next_band) = (0, 0);
        let mut schema = GridSchema::new();
        while next_column < limit {
            for item in items(rng, 0, &mut next_column, &mut next_band, limit) {
                schema = match item {
                    Ok(column) => schema.column(column),
                    Err(band) => schema.band(band),
                };
            }
        }
        (schema, next_band)
    }

    fn random_column(rng: &mut fastrand::Rng, i: u16) -> GridColumn<TestColumn> {
        let zone = match rng.u8(..6) {
            0 => ColumnZone::FrozenLeft,
            1 => ColumnZone::FrozenRight,
            _ => ColumnZone::Scrolling,
        };
        let column = column(i)
            .with_width(px(rng.u16(10..200) as f32))
            .with_zone(zone)
            .with_visible(rng.u8(..5) != 0);
        match rng.u8(..4) {
            0 => column.with_min_width(px(rng.u16(10..80) as f32)),
            1 => column.with_max_width(px(rng.u16(80..250) as f32)),
            _ => column,
        }
    }

    /// The naive model of restoring `saved` on a schema without bands:
    /// saved columns the schema has, in order, the first of a repeat;
    /// then the schema's other columns with their defaults; split by zone;
    /// one column visible. Widths within the schema's limits. A schema that
    /// hides every column shows its first one (in its own display order) by
    /// default, as a grid builds it.
    fn reference_restore(
        schema: &[GridColumn<TestColumn>],
        saved: &GridLayout,
    ) -> Vec<LayoutColumn> {
        let find = |id: &str| schema.iter().find(|c| c.column().id() == id);
        let mut schema_order: Vec<&GridColumn<TestColumn>> = schema.iter().collect();
        schema_order.sort_by_key(|c| c.zone().slot());
        let shown_by_default =
            (!schema.iter().any(GridColumn::is_visible)).then(|| schema_order[0].column());
        let mut out: Vec<LayoutColumn> = Vec::new();
        let mut push = |entry: LayoutColumn, column: &GridColumn<TestColumn>| {
            if out.iter().any(|c| c.id() == entry.id()) {
                return;
            }
            let min = f32::from(column.min_width().max(MIN_COLUMN_WIDTH));
            let width = entry.width().min(f32::from(column.max_width())).max(min);
            out.push(entry.with_width(width));
        };
        for entry in saved.columns() {
            if let Some(column) = find(entry.id().as_str()) {
                push(entry.clone(), column);
            }
        }
        for column in schema {
            let visible = column.is_visible() || shown_by_default == Some(column.column());
            let entry = LayoutColumn::new(column.column().id(), column.width())
                .with_visible(visible)
                .with_zone(column.zone());
            push(entry, column);
        }
        out.sort_by_key(|c| c.zone().slot());
        if !out.iter().any(LayoutColumn::is_visible) {
            out[0] = out[0].clone().with_visible(true);
        }
        out
    }

    #[test]
    fn test_layout_round_trip_matches_reference() {
        for seed in 0..60 {
            for banded in [true, false] {
                let mut rng = fastrand::Rng::with_seed(seed);
                let policy: &dyn ColumnMovePolicy = if banded { &BandedMove } else { &FreeMove };
                let (schema, _) = random_schema(&mut rng);
                let slot = LayoutSlot::<TestRow>::new(&schema);
                let mut columns = built(&schema, policy);
                let all: Vec<ColumnIx> = columns.order().to_vec();
                for step in 0..40 {
                    // A random operation of the column layout.
                    let target = all[rng.usize(..all.len())];
                    let zone = columns.zone_of(target);
                    match rng.u8(..5) {
                        0 => {
                            let before = Some(all[rng.usize(..all.len())])
                                .filter(|before| columns.zone_of(*before) == zone);
                            columns.move_before(zone, vec![target], before, policy);
                        }
                        1 => {
                            columns.set_zone(target, ColumnZone::ALL[rng.usize(..3)], policy);
                        }
                        2 => {
                            columns.set_visible(target, !columns.is_visible(target));
                        }
                        3 => {
                            columns.set_width(target, px(rng.u16(0..300) as f32));
                        }
                        _ => {
                            if let Some(&band) = columns.bands_of(target).first() {
                                let members = columns.band_members(zone, band);
                                let before = Some(all[rng.usize(..all.len())])
                                    .filter(|before| columns.zone_of(*before) == zone);
                                columns.move_before(zone, members, before, policy);
                            }
                        }
                    }

                    // Export, apply to a new grid of the schema, export: the
                    // same layout, and an exact report.
                    let saved = exported(&columns)
                        .into_iter()
                        .fold(GridLayout::new(), GridLayout::with_column);
                    let mut restored = built(&schema, policy);
                    let (resolved, report) = slot.resolve(&restored, policy, &saved);
                    resolved.apply(&mut restored);
                    assert_eq!(
                        exported(&restored),
                        saved.columns(),
                        "seed {seed} step {step} banded {banded}"
                    );
                    assert!(report.is_exact());
                    // Applied to itself, the layout changes nothing.
                    let (resolved, _) = slot.resolve(&columns, policy, &saved);
                    assert!(resolved.apply(&mut columns).is_empty());
                }

                // A changed schema without bands: some columns removed, new
                // ones (`c12`..) added, in a random order with new defaults.
                let saved = exported(&columns)
                    .into_iter()
                    .fold(GridLayout::new(), GridLayout::with_column)
                    .with_sort(
                        (0..rng.usize(..4))
                            .map(|_| {
                                let i = rng.u16(..16);
                                LayoutSort::new(IDS[i as usize], SortDirection::Ascending)
                            })
                            .collect::<Vec<_>>(),
                    );
                let mut kept: Vec<u16> = (0..16)
                    .filter(|i| {
                        let kept = all.contains(&ix(*i)) && rng.u8(..4) != 0;
                        kept || (*i >= 12 && rng.bool())
                    })
                    .collect();
                if kept.is_empty() {
                    kept.push(12);
                }
                let mut changed: Vec<GridColumn<TestColumn>> = kept
                    .into_iter()
                    .map(|i| random_column(&mut rng, i))
                    .collect();
                rng.shuffle(&mut changed);
                let new_schema = changed
                    .iter()
                    .cloned()
                    .fold(GridSchema::new(), GridSchema::column);
                let new_slot = LayoutSlot::<TestRow>::new(&new_schema);
                let mut restored = built(&new_schema, policy);
                let (resolved, report) = new_slot.resolve(&restored, policy, &saved);
                resolved.apply(&mut restored);
                assert_eq!(
                    exported(&restored),
                    reference_restore(&changed, &saved),
                    "seed {seed} banded {banded}"
                );
                let known = |id: &str| changed.iter().any(|c| c.column().id() == id);
                let mut ignored: Vec<&str> = Vec::new();
                for column in saved.columns() {
                    let id = column.id().as_str();
                    if !known(id) && !ignored.contains(&id) {
                        ignored.push(id);
                    }
                }
                assert_eq!(ids(report.ignored_columns()), ignored);
                let mut sorted: Vec<ColumnIx> = Vec::new();
                for key in saved.sort() {
                    let i = IDS.iter().position(|id| *id == key.column().as_str());
                    let i = ix(i.expect("a test id") as u16);
                    if known(key.column().as_str()) && !sorted.contains(&i) {
                        sorted.push(i);
                    }
                }
                let resolved_sort: Vec<ColumnIx> =
                    resolved.sort.iter().map(|(ix, _)| *ix).collect();
                assert_eq!(resolved_sort, sorted);
            }
        }
    }

    fn text(value: &str) -> OwnedValue {
        OwnedValue::Text(value.into())
    }

    /// A group path on columns of [`TestColumn`].
    fn path(levels: &[(u16, OwnedValue)]) -> GroupPath {
        GroupPath::new(
            levels
                .iter()
                .map(|(i, value)| (TestColumn(*i).column_id(), value.clone())),
        )
    }

    fn by(i: u16, direction: SortDirection) -> GroupBy<TestRow> {
        GroupBy::column(TestColumn(i)).with_direction(direction)
    }

    #[test]
    fn test_layout_grouping_resolution() {
        use SortDirection::{Ascending, Descending};

        // `c4` refuses grouping.
        let schema = (0..6).fold(GridSchema::new(), |schema, i| {
            schema.column(column(i).with_groupable(i != 4))
        });
        let columns = built(&schema, &FreeMove);
        let mut slot = LayoutSlot::<TestRow>::new(&schema);

        // An unknown level, a level that cannot group and a repeat are
        // dropped; the levels below move up. Paths deeper than the two kept
        // levels and empty ones are dropped; the rest keep their values.
        let saved = GridLayout::new()
            .with_group_by([
                LayoutGroup::new("c1", Ascending),
                LayoutGroup::new("gone", Ascending),
                LayoutGroup::new("c4", Descending),
                LayoutGroup::new("c2", Descending),
                LayoutGroup::new("c1", Descending),
            ])
            .with_expansion(
                LayoutExpansion::new(ExpandDepth::Levels(1))
                    .with_toggled([text("b")])
                    .with_toggled([text("a"), OwnedValue::Int(12)])
                    .with_toggled([text("b"), text("y"), text("z")])
                    .with_toggled([])
                    .with_toggled([text("a")]),
            );
        let (resolved, report) = slot.resolve(&columns, &FreeMove, &saved);
        let levels: Vec<(Option<TestColumn>, SortDirection)> = resolved
            .arrangement
            .levels()
            .iter()
            .map(|level| (level.as_column(), level.direction()))
            .collect();
        assert_eq!(
            levels,
            [
                (Some(TestColumn(1)), Ascending),
                (Some(TestColumn(2)), Descending)
            ]
        );
        assert_eq!(
            resolved.toggled,
            [
                path(&[(1, text("b"))]),
                path(&[(1, text("a")), (2, OwnedValue::Int(12))]),
                path(&[(1, text("a"))]),
            ]
        );
        assert_eq!(resolved.depth, ExpandDepth::Levels(1));
        assert_eq!(ids(report.dropped_group_by()), ["gone", "c4", "c1"]);
        assert_eq!(report.dropped_expansion(), 2);

        // Written back: the kept levels, the paths in the order of their
        // values.
        let layout = resolved.to_layout(&slot);
        assert_eq!(
            layout.group_by(),
            [
                LayoutGroup::new("c1", Ascending),
                LayoutGroup::new("c2", Descending)
            ]
        );
        assert_eq!(
            layout.expansion(),
            Some(
                &LayoutExpansion::new(ExpandDepth::Levels(1))
                    .with_toggled([text("a")])
                    .with_toggled([text("a"), OwnedValue::Int(12)])
                    .with_toggled([text("b")])
            )
        );

        // Without levels a layout keeps its depth and no toggled group.
        let flat = GridLayout::new()
            .with_expansion(LayoutExpansion::new(ExpandDepth::Captions).with_toggled([text("a")]));
        let (resolved, report) = slot.resolve(&columns, &FreeMove, &flat);
        assert!(matches!(resolved.arrangement, Arrangement::Flat));
        assert_eq!(resolved.depth, ExpandDepth::Captions);
        assert!(resolved.toggled.is_empty());
        assert_eq!(report.dropped_expansion(), 1);
        let layout = resolved.to_layout(&slot);
        assert!(layout.group_by().is_empty());
        assert_eq!(
            layout.expansion(),
            Some(&LayoutExpansion::new(ExpandDepth::Captions))
        );

        // The default layout: the grouping of `with_view` and the depth of
        // `with_expand_depth`, nothing toggled.
        slot.set_view(&crate::engine::ViewSpec::new().with_group_by([by(3, Descending)]));
        slot.set_view_depth(ExpandDepth::Levels(0));
        let default = slot.default_resolved(&columns, &FreeMove).to_layout(&slot);
        assert_eq!(default.group_by(), [LayoutGroup::new("c3", Descending)]);
        assert_eq!(
            default.expansion(),
            Some(&LayoutExpansion::new(ExpandDepth::Levels(0)))
        );
        // A level by a function: neither the grouping nor the expansion.
        let bucket = GroupKey::new("bucket", ColumnSet::EMPTY, |_: &TestRow| CellValue::Empty);
        slot.set_view(
            &crate::engine::ViewSpec::new().with_group_by([by(3, Ascending), GroupBy::key(bucket)]),
        );
        let default = slot.default_resolved(&columns, &FreeMove).to_layout(&slot);
        assert!(default.group_by().is_empty());
        assert_eq!(default.expansion(), None);
        // A tree: no expansion either.
        slot.set_view(&crate::engine::ViewSpec::new().with_tree(crate::engine::TreeSpec::new()));
        let default = slot.default_resolved(&columns, &FreeMove).to_layout(&slot);
        assert!(default.group_by().is_empty());
        assert_eq!(default.expansion(), None);
    }

    #[test]
    fn test_saved_grouping() {
        use SortDirection::{Ascending, Descending};

        let levels = [by(1, Ascending), by(2, Descending)];
        // Toggles of this grouping in a hash set, of a deeper one (c1, c2,
        // c3) and of another one (c2 first).
        let expansion = [
            path(&[(1, text("b"))]),
            path(&[(1, OwnedValue::Float(2.5))]),
            path(&[(1, OwnedValue::Int(2)), (2, text("x"))]),
            path(&[(1, OwnedValue::Int(2))]),
            path(&[(1, OwnedValue::Empty)]),
            path(&[(1, OwnedValue::Bool(true)), (2, text("x")), (3, text("y"))]),
            path(&[(2, text("x"))]),
        ]
        .into_iter()
        .fold(
            Expansion::<u32>::new(ExpandDepth::All),
            |expansion, path| expansion.with_group(path, false, 3),
        );
        let layout = with_saved_grouping(GridLayout::new(), &levels, &expansion);
        assert_eq!(
            layout.group_by(),
            [
                LayoutGroup::new("c1", Ascending),
                LayoutGroup::new("c2", Descending)
            ]
        );
        // Empty, then numbers, then text; a path before its children.
        let toggled: Vec<Vec<OwnedValue>> = vec![
            vec![OwnedValue::Empty],
            vec![OwnedValue::Int(2)],
            vec![OwnedValue::Int(2), text("x")],
            vec![OwnedValue::Float(2.5)],
            vec![text("b")],
        ];
        assert_eq!(
            layout.expansion().map(LayoutExpansion::toggled),
            Some(toggled.as_slice())
        );

        // No grouping: the depth alone, which a layout omits when it is the
        // default.
        let flat: [GroupBy<TestRow>; 0] = [];
        let layout = with_saved_grouping(GridLayout::new(), &flat, &expansion);
        assert!(layout.group_by().is_empty());
        assert_eq!(layout.expansion(), None);
        let layout = with_saved_grouping(
            GridLayout::new(),
            &flat,
            &Expansion::<u32>::new(ExpandDepth::Levels(2)),
        );
        assert_eq!(
            layout.expansion(),
            Some(&LayoutExpansion::new(ExpandDepth::Levels(2)))
        );
    }

    #[test]
    fn test_cmp_values() {
        use std::cmp::Ordering::{Equal, Greater, Less};

        let int = OwnedValue::Int;
        assert_eq!(cmp_values(&[], &[]), Equal);
        assert_eq!(cmp_values(&[], &[text("a")]), Less);
        assert_eq!(cmp_values(&[int(1)], &[int(1), text("a")]), Less);
        assert_eq!(
            cmp_values(&[int(2), text("a")], &[int(1), text("b")]),
            Greater
        );
        assert_eq!(
            cmp_values(&[OwnedValue::Float(1.5)], &[int(2)]),
            Less,
            "numbers by value"
        );
        assert_eq!(cmp_values(&[text("a")], &[int(9)]), Greater);
    }

    /// Microseconds of the UI-thread part of restoring a layout of 128
    /// columns with three grouping levels and ten toggled groups: resolve,
    /// apply the columns, build the expansion, and write the layout back.
    /// Run with `cargo test --release -p gpui-kit-grid --lib -- --ignored
    /// --nocapture test_restore_timing`.
    #[test]
    #[ignore = "timing; run in release with --ignored --nocapture"]
    #[cfg(not(target_family = "wasm"))]
    fn test_restore_timing() {
        use std::{hint::black_box, sync::LazyLock, time::Instant};

        use super::{toggle_groups, with_saved_grouping};

        static IDS: LazyLock<Vec<String>> =
            LazyLock::new(|| (0..128).map(|ix| format!("column_{ix}")).collect());
        static ALL: [WideColumn; 128] = {
            let mut all = [WideColumn(0); 128];
            let mut ix = 0;
            while ix < 128 {
                all[ix] = WideColumn(ix as u16);
                ix += 1;
            }
            all
        };

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        struct WideColumn(u16);

        impl GridColumns for WideColumn {
            fn all() -> &'static [Self] {
                &ALL
            }

            fn id(self) -> &'static str {
                IDS[self.0 as usize].as_str()
            }

            fn ix(self) -> ColumnIx {
                ColumnIx::new(self.0)
            }
        }

        struct WideRow;

        impl GridRow for WideRow {
            type Key = u32;
            type Column = WideColumn;

            fn key(&self) -> &u32 {
                &0
            }

            fn value(&self, _: WideColumn) -> CellValue<'_> {
                CellValue::Empty
            }
        }

        let schema = ALL.iter().fold(GridSchema::new(), |schema, column| {
            schema.column(GridColumn::new(*column, column.id()).with_width(px(80.)))
        });
        let slot = LayoutSlot::<WideRow>::new(&schema);
        let mut columns = ColumnLayout::new(schema.clone());
        columns.normalize(&FreeMove);
        // Two layouts that differ in every column, so that each restore
        // moves and resizes them all.
        let layout = |reversed: bool, width: f32| {
            let mut expansion = LayoutExpansion::new(ExpandDepth::Levels(1));
            for ix in 0..10 {
                expansion = expansion.with_toggled([
                    text(&format!("instrument {ix}")),
                    text("Buy"),
                    OwnedValue::Float(100.25 + f64::from(ix)),
                ]);
            }
            let ids: Vec<&String> = match reversed {
                true => IDS.iter().rev().collect(),
                false => IDS.iter().collect(),
            };
            ids.into_iter()
                .map(|id| LayoutColumn::new(id.clone(), width))
                .fold(GridLayout::new(), GridLayout::with_column)
                .with_sort([LayoutSort::new("column_3", SortDirection::Descending)])
                .with_group_by(
                    IDS[..3]
                        .iter()
                        .map(|id| LayoutGroup::new(id.clone(), SortDirection::Ascending)),
                )
                .with_expansion(expansion)
        };
        let layouts = [layout(true, 96.5), layout(false, 80.)];
        let started = Instant::now();
        let rounds = 1_000;
        let mut best = f64::MAX;
        for _ in 0..5 {
            let started_round = Instant::now();
            for round in 0..rounds {
                let saved = &layouts[round % 2];
                let (resolved, report) = slot.resolve(&columns, &FreeMove, black_box(saved));
                assert!(report.is_exact());
                black_box(resolved.apply(&mut columns));
                let levels = resolved.arrangement.levels();
                let expansion = toggle_groups(
                    Expansion::<u32>::new(resolved.depth),
                    resolved.toggled.iter().cloned(),
                    levels.len(),
                );
                black_box(with_saved_grouping(GridLayout::new(), levels, &expansion));
            }
            best = best.min(started_round.elapsed().as_secs_f64() * 1e6 / rounds as f64);
        }
        println!(
            "restore of 128 columns, 3 levels, 10 groups: {best:.1} µs ({:.0} ms in all)",
            started.elapsed().as_secs_f64() * 1e3
        );
    }
}
