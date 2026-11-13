//! The tree on screen: turning a tree on and off, the column that shows it,
//! and what the keys and the menu ask of a tree row.
//!
//! The rest is shared with group captions: the indent and the chevron, the
//! press on a chevron and the keys (`state/disclosure.rs`), the selection
//! of tree rows ([`SelectionPolicy`](crate::SelectionPolicy),
//! `state/selection.rs`).

use std::mem;

use gpui_kit::Context;

use super::{
    ColumnOf, KeyOf, LiveGridDelegate, LiveGridState,
    events::{ExpandTarget, GroupOrigin},
    menu::GridTarget,
};
use crate::{
    engine::TreeSpec,
    model::{ColumnIx, GridColumns as _, GridRow as _},
};

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Shows the rows as a tree by `GridRow::parent_key` (`Some`) or flat
    /// (`None`), and rebuilds at once. The new order is shown from the top,
    /// unless the focused line (a row or a group caption) is on screen and
    /// still shown: it keeps its screen line. A tree replaces the grouping,
    /// as [`Self::set_group_by`] replaces a tree. Emits `GroupChange` with
    /// `GroupOrigin::Programmatic`, after `OrderHeldChange { held: false }`
    /// if a held order was dropped; a `ProjectionChange` follows.
    ///
    /// A [`TreeSpec`] has no equality (its rules are closures), so a tree
    /// always rebuilds and emits; `None` on a view that is no tree does
    /// nothing. The expansion of tree rows is kept by key: turning the tree
    /// off and on again brings it back.
    pub fn set_tree(&mut self, tree: Option<TreeSpec<D::Row>>, cx: &mut Context<Self>) {
        self.apply_tree(tree, GroupOrigin::Programmatic, cx);
    }

    /// The one path of every tree change but `set_view`, which compares the
    /// whole view: `set_tree` and a restored layout. Returns false, doing
    /// nothing, for `None` on a view that is no tree; otherwise puts the
    /// tree into the view (a grouping goes) or makes the view flat, and
    /// rebuilds at once, as a user action, shown as any new view
    /// (`view_changed`): `OrderHeldChange { held: false }` if a held order
    /// is dropped, then `GroupChange { origin }`.
    pub(crate) fn apply_tree(
        &mut self,
        tree: Option<TreeSpec<D::Row>>,
        origin: GroupOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let view = mem::take(&mut self.view);
        let is_tree = view.tree().is_some();
        self.view = match tree {
            Some(tree) => view.with_tree(tree),
            None if is_tree => view.with_group_by(std::iter::empty()),
            None => view,
        };
        if !is_tree && self.view.tree().is_none() {
            return false;
        }
        self.view_changed(None, Some(origin), cx);
        true
    }

    /// The column that shows the tree now: [`TreeSpec::column`] if it is
    /// visible, else the first visible column in display order (also while
    /// a tree projection is still shown for a view that is no tree any
    /// more). Read from the column layout every frame, so hiding or moving
    /// columns never loses the tree.
    pub(crate) fn tree_column(&self) -> Option<ColumnIx> {
        self.view
            .tree()
            .and_then(|tree| tree.column())
            .map(|column| column.ix())
            .filter(|ix| self.columns.is_visible(*ix))
            .or_else(|| self.columns.visible().next())
    }

    /// Returns true if the shown projection is a tree and the row with
    /// `key` has shown children in it.
    pub(super) fn is_tree_branch(&self, key: &KeyOf<D>) -> bool {
        self.projection.is_tree()
            && self
                .snapshot
                .row_id(key)
                .is_some_and(|id| self.projection.branch(id).is_some())
    }

    /// The parent of a tree row in the shown projection: its nearest
    /// ancestor the view passes (a filter may hide the rows between).
    /// `None` for a root, and outside a tree. O(depth), once per key press.
    pub(super) fn tree_parent(&self, key: &KeyOf<D>) -> Option<KeyOf<D>> {
        let (projection, snapshot) = (&self.projection, &self.snapshot);
        let tree = projection.tree_data()?;
        let id = snapshot.row_id(key)?;
        // A root: no parent, an orphan, or a row of a cycle.
        if tree.depth(projection.order_ix(id)?) == 0 {
            return None;
        }
        let mut row = snapshot.row(id)?;
        // Up the parent keys to the nearest row the projection holds; the
        // bound ends a cycle of rows it does not.
        for _ in 0..=u16::MAX {
            let parent = snapshot.row_id(row.parent_key()?)?;
            row = snapshot.row(parent)?;
            if projection.contains(parent) {
                return Some(row.key().clone());
            }
        }
        None
    }

    /// Whether the line of a menu target is expanded, if it expands: a
    /// group caption, or a tree row with children.
    pub(super) fn target_expansion(
        &self,
        target: &GridTarget<KeyOf<D>, ColumnOf<D>>,
    ) -> Option<bool> {
        match target {
            GridTarget::GroupCaption { group, .. } => {
                Some(self.is_expanded(&ExpandTarget::Group(group.clone())))
            }
            GridTarget::Cell { key, .. } if self.is_tree_branch(key) => {
                Some(self.is_expanded(&ExpandTarget::Row(key.clone())))
            }
            _ => None,
        }
    }
}
