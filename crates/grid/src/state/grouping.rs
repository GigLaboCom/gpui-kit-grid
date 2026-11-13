//! Grouping and expansion in the grid state: the grouping levels and
//! aggregates of the view, the expansion the grid owns, the one path each
//! change takes and the events it emits.
//!
//! The grid owns the expansion as it owns the quick filter: it lives in the
//! view ([`ViewSpec::expansion`]) and survives every new view. Toggles are
//! kept by [`GroupPath`] and by row key, so a change of grouping silences
//! them and going back brings them back; only a new default depth clears
//! them.

use std::{mem, sync::Arc};

use gpui_kit::Context;

use super::{
    ColumnOf, KeyOf, LiveGridDelegate, LiveGridEvent, LiveGridState,
    events::{ExpandTarget, GroupOrigin},
    projection::PendingWork,
};
use crate::{
    engine::{
        Aggregate, DisplayRow, ExpandDepth, Expansion, GroupBy, GroupPath, Structure, TreeSpec,
        ViewSpec,
    },
    model::GridRow,
};

/// Counts the edits of the view's expansion. A job and a held order
/// remember the revision their lines are laid out for, and lines of an
/// older one are laid out again when shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ExpansionRevision(u64);

impl ExpansionRevision {
    fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// The grouping levels, the top level first; empty when not grouped.
    pub fn group_by(&self) -> &[GroupBy<D::Row>] {
        self.view.group_by()
    }

    /// Replaces the grouping and rebuilds at once (`GroupChange` with
    /// `GroupOrigin::Programmatic`). The new order is shown from the top,
    /// unless the focused line (a row or a group caption) is on screen and
    /// still shown: it keeps its screen line. Repeated levels are dropped,
    /// the first one stays; the same grouping again (the same level ids and
    /// directions) does nothing. Works for columns the group panel does not
    /// accept. A grouping replaces a tree; no levels turn grouping off and
    /// leave a tree alone.
    ///
    /// The expansion stays: toggles are kept by group path, so those of
    /// another grouping come back with it.
    pub fn set_group_by(
        &mut self,
        levels: impl IntoIterator<Item = GroupBy<D::Row>>,
        cx: &mut Context<Self>,
    ) {
        self.apply_group_by(levels.into_iter().collect(), GroupOrigin::Programmatic, cx);
    }

    /// The one path of every grouping change: the group panel and its menu,
    /// grouping Actions, a layout, `set_group_by`. Drops repeated levels
    /// (the first one stays) and returns false, doing nothing, if the
    /// grouping is unchanged. Otherwise releases a held order
    /// (`OrderHeldChange { held: false }`), emits `GroupChange { origin }`
    /// and rebuilds at once, as a user action. The new order is shown from
    /// the top, unless the focused line is on screen and still shown: it
    /// keeps its screen line (`restore_line_anchor`).
    pub(crate) fn apply_group_by(
        &mut self,
        levels: Vec<GroupBy<D::Row>>,
        origin: GroupOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut normalized: Vec<GroupBy<D::Row>> = Vec::with_capacity(levels.len());
        for level in levels {
            if !normalized.iter().any(|kept| kept.id() == level.id()) {
                normalized.push(level);
            }
        }
        if same_levels(&normalized, self.view.group_by()) {
            return false;
        }
        self.view = mem::take(&mut self.view).with_group_by(normalized);
        self.view_changed(None, Some(origin), cx);
        true
    }

    /// The tree of the view, if it builds one; `None` when flat or grouped.
    pub fn tree(&self) -> Option<&TreeSpec<D::Row>> {
        self.view.tree()
    }

    /// The aggregates of group captions, group footers and the totals, by
    /// column.
    pub fn aggregates(&self) -> &[(ColumnOf<D>, Aggregate<D::Row>)] {
        self.view.aggregates()
    }

    /// Replaces the aggregates of group captions, group footers and the
    /// totals; a repeated column keeps its last aggregate. The same
    /// aggregates again (custom ones by identity, in any order) do nothing;
    /// otherwise the view rebuilds at once, as a user action, keeping the
    /// scroll position (an aggregate order may move groups), and a held
    /// order is dropped (`OrderHeldChange { held: false }`). No event of its
    /// own: the application sets the aggregates, and a layout does not keep
    /// them.
    pub fn set_aggregates(
        &mut self,
        aggregates: impl IntoIterator<Item = (ColumnOf<D>, Aggregate<D::Row>)>,
        cx: &mut Context<Self>,
    ) {
        let view = self.view.clone().with_aggregates(aggregates);
        if same_aggregates(view.aggregates(), self.view.aggregates()) {
            return;
        }
        self.view = view;
        self.restart_projection(cx);
        cx.notify();
    }

    /// Sets how deep groups and tree rows are expanded by default
    /// ([`ExpandDepth::All`] unless set), with nothing toggled. For a grid
    /// being built; [`Self::set_expand_depth`] changes it later. The
    /// expansion of a layout given to [`Self::with_layout`], before or after,
    /// replaces it.
    pub fn with_expand_depth(mut self, depth: ExpandDepth) -> Self {
        self.view = mem::take(&mut self.view).with_expansion(Expansion::new(depth));
        self.expansion_revision = self.expansion_revision.next();
        // The default layout keeps this depth.
        self.layout.set_view_depth(depth);
        self.apply_initial_layout()
    }

    /// How deep groups and tree rows are expanded by default.
    pub fn expand_depth(&self) -> ExpandDepth {
        self.view.expansion().depth()
    }

    /// Sets the default depth and clears every toggle: expand all
    /// (`ExpandDepth::All`), collapse all (`Levels(0)`), captions only
    /// (`Captions`). Emits `ExpansionChange { target: None }` with
    /// `GroupOrigin::Programmatic`, unless the depth is the same and nothing
    /// is toggled.
    pub fn set_expand_depth(&mut self, depth: ExpandDepth, cx: &mut Context<Self>) {
        self.apply_expand_depth(depth, GroupOrigin::Programmatic, cx);
    }

    /// [`Self::set_expand_depth`] for `origin`: the Actions and menu items
    /// of expand all, collapse all and captions only are `User`. Returns
    /// false if nothing changed.
    pub(crate) fn apply_expand_depth(
        &mut self,
        depth: ExpandDepth,
        origin: GroupOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        self.apply_expansion(Expansion::new(depth), None, origin, cx)
    }

    /// Whether a group or a tree row is expanded now: the view's expansion,
    /// not the projection shown, which may lag one job behind. The root
    /// path is expanded. A tree row's depth comes from the shown projection:
    /// a row it does not hold, or one of a projection that is not a tree,
    /// is not expanded.
    pub fn is_expanded(&self, target: &ExpandTarget<KeyOf<D>>) -> bool {
        let expansion = self.view.expansion();
        match target {
            ExpandTarget::Group(path) => {
                path.is_root() || expansion.is_group_expanded(path, self.group_levels(path))
            }
            ExpandTarget::Row(key) => self
                .tree_row_depth(key)
                .is_some_and(|depth| expansion.is_row_expanded(key, depth)),
        }
    }

    /// Expands or collapses one group or tree row. Returns false, doing
    /// nothing, if it was so already, for the root path, and for a tree row
    /// the shown projection does not hold as a tree. Otherwise emits
    /// `ExpansionChange` with `GroupOrigin::Programmatic` at once; the lines
    /// are laid out again in the background, without the throttle and the
    /// reorder hold, and a `ProjectionChange` follows.
    ///
    /// A group of another grouping may be toggled too: the toggle shows when
    /// that grouping comes back. A new row never expands its group: it
    /// arrives in the group's state.
    pub fn set_expanded(
        &mut self,
        target: ExpandTarget<KeyOf<D>>,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        self.apply_expanded(target, expanded, GroupOrigin::Programmatic, cx)
    }

    /// [`Self::set_expanded`] for `origin`: chevrons, keys and menu items
    /// are `User`.
    pub(crate) fn apply_expanded(
        &mut self,
        target: ExpandTarget<KeyOf<D>>,
        expanded: bool,
        origin: GroupOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let expansion = self.view.expansion().clone();
        let expansion = match &target {
            ExpandTarget::Group(path) => {
                if path.is_root() {
                    return false;
                }
                expansion.with_group(path.clone(), expanded, self.group_levels(path))
            }
            ExpandTarget::Row(key) => {
                let Some(depth) = self.tree_row_depth(key) else {
                    return false;
                };
                expansion.with_row(key.clone(), expanded, depth)
            }
        };
        self.apply_expansion(expansion, Some(target), origin, cx)
    }

    /// The one path of every expansion change: puts `expansion` into the
    /// view and emits `ExpansionChange { target, origin }` at once, before
    /// the lines are laid out; returns false, doing nothing, if it equals
    /// the view's. A grouped or tree view lays its lines out again as a user
    /// action (`expand`): no throttle, no reorder hold, ahead of a
    /// data-driven rebuild that waits; a held order stays held and is laid
    /// out when shown. A flat view has nothing to lay out.
    ///
    /// A deep toggle (a line and every descendant) builds the whole
    /// expansion and passes the line as `target`: one event.
    ///
    /// A focused line the new expansion hides moves at once to the caption
    /// or tree row of the outermost collapsed line above it: `FocusChange`
    /// after `ExpansionChange`, with the same origin.
    pub(crate) fn apply_expansion(
        &mut self,
        expansion: Expansion<KeyOf<D>>,
        target: Option<ExpandTarget<KeyOf<D>>>,
        origin: GroupOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        if expansion == *self.view.expansion() {
            return false;
        }
        self.view = mem::take(&mut self.view).with_expansion(expansion);
        self.expansion_revision = self.expansion_revision.next();
        if self.has_structure() {
            self.pending = self.pending.with(PendingWork::EXPAND);
        }
        cx.emit(LiveGridEvent::ExpansionChange { target, origin });
        self.focus_off_collapsed(origin, cx);
        // The chevrons follow the view's expansion in the next frame, which
        // also starts the layout.
        cx.notify();
        true
    }

    /// The levels a group's default expansion is decided by: the view's
    /// grouping, or the path's own depth for a group of a deeper one.
    pub(super) fn group_levels(&self, path: &GroupPath) -> usize {
        self.view
            .group_by()
            .len()
            .max(usize::from(path.depth()) + 1)
    }

    /// The depth of a tree row in the shown projection; `None` if the
    /// projection is not a tree or does not hold the row. A row under a
    /// collapsed ancestor is not on screen: its depth counts the ancestors
    /// the projection holds.
    fn tree_row_depth(&self, key: &KeyOf<D>) -> Option<u16> {
        let (projection, snapshot) = (&self.projection, &self.snapshot);
        if !projection.is_tree() {
            return None;
        }
        let id = snapshot.row_id(key)?;
        if let Some(display_ix) = projection.display_ix(id) {
            return projection.line(display_ix).map(DisplayRow::depth);
        }
        if !projection.contains(id) {
            return None;
        }
        let mut row = snapshot.row(id)?;
        let mut depth = 0u16;
        // A depth fits `u16`: the bound also ends a cycle of parents.
        while depth < u16::MAX {
            let Some(parent) = row
                .parent_key()
                .and_then(|parent| snapshot.row_id(parent))
                .filter(|parent| projection.contains(*parent))
            else {
                break;
            };
            depth += 1;
            row = snapshot.row(parent)?;
        }
        Some(depth)
    }
}

/// Returns true if two lists of levels group alike: the same level ids and
/// directions, in order. A group key is known by its id.
fn same_levels<R: GridRow>(a: &[GroupBy<R>], b: &[GroupBy<R>]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.id() == b.id() && a.direction() == b.direction())
}

/// Returns true if two views arrange rows alike: both flat, grouped by the
/// same levels, or trees of the same column. What a `GroupChange` reports.
pub(super) fn same_structure<R: GridRow>(a: &ViewSpec<R>, b: &ViewSpec<R>) -> bool {
    match (a.structure(), b.structure()) {
        (Structure::Flat, Structure::Flat) => true,
        (Structure::Groups(_), Structure::Groups(_)) => same_levels(a.group_by(), b.group_by()),
        (Structure::Tree(a), Structure::Tree(b)) => a.column() == b.column(),
        _ => false,
    }
}

/// Returns true if two sets of aggregates are the same, in any order: built-in
/// ones by kind, custom ones by identity. Columns are unique in each.
fn same_aggregates<R: GridRow>(
    a: &[(R::Column, Aggregate<R>)],
    b: &[(R::Column, Aggregate<R>)],
) -> bool {
    a.len() == b.len()
        && a.iter().all(|(column, aggregate)| {
            b.iter()
                .any(|(other, kept)| other == column && same_aggregate(aggregate, kept))
        })
}

fn same_aggregate<R: GridRow>(a: &Aggregate<R>, b: &Aggregate<R>) -> bool {
    match (a, b) {
        (Aggregate::Custom(a), Aggregate::Custom(b)) => Arc::ptr_eq(a, b),
        (Aggregate::Custom(_), _) | (_, Aggregate::Custom(_)) => false,
        _ => mem::discriminant(a) == mem::discriminant(b),
    }
}
