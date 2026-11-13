//! Expanding and collapsing on screen: one mechanism for group captions and
//! tree rows with children. It gives a tree row's cell its indent and
//! chevron, finds a press on a chevron, runs the [`ExpandGesture`] and the
//! keys (`ExpandRow`, `CollapseRow`, ←/→ on a caption, expand all, collapse
//! all, captions only), toggles a line with everything below it, and moves
//! the focus out of what a collapse hides. The tree's column is
//! [`LiveGridState::tree_column`] (`state/tree.rs`), and `-` on a tree leaf
//! or a collapsed tree row collapses its parent.
//!
//! The chevron follows the view's expansion, so it turns in the frame after
//! the press; the lines follow when the expand job is done.

use gpui_kit::accesskit::Action;
use gpui_kit::component::{IconName, IconNamed as _};
use gpui_kit::{
    Context, InteractiveElement, MouseDownEvent, Pixels, Point, SharedString, Window, px,
};

use super::{
    CELL_PADDING, ColumnOf, KeyOf, LiveGridDelegate, LiveGridState, SelectionOrigin,
    a11y::RowContent,
    events::{ExpandTarget, GroupOrigin},
    frame::{LEAD_ICON_LANE, PaintedLine},
    group_hash,
    scroll::{AnchorLine, LineAnchor},
    selection::{FocusedCell, LineTarget, Navigation, ShownRows},
};
use crate::{
    actions::{CollapseAll, CollapseRow, ExpandAll, ExpandRow, ShowCaptionsOnly},
    engine::{DisplayRow, ExpandDepth, GroupInfo, GroupPath, Projection},
    model::GridRow,
    policy::{ExpandArea, ExpandEdit, ExpandGesture, ExpandGestureContext},
};

/// The indent of one level of groups and of a tree.
pub(super) const INDENT: Pixels = px(16.);

/// The chevron's size: 12 px on `xsmall` and `small` rows, 14 px on
/// `medium` and `large` ones.
pub(super) fn chevron_size(row_height: Pixels) -> Pixels {
    if row_height < px(32.) {
        px(12.)
    } else {
        px(14.)
    }
}

/// The paths of the chevrons, made once per frame.
pub(super) struct Chevrons {
    expanded: SharedString,
    collapsed: SharedString,
}

impl Chevrons {
    pub(super) fn new() -> Self {
        Self {
            expanded: IconName::ChevronDown.path(),
            collapsed: IconName::ChevronRight.path(),
        }
    }

    pub(super) fn path(&self, expanded: bool) -> SharedString {
        if expanded {
            self.expanded.clone()
        } else {
            self.collapsed.clone()
        }
    }
}

/// The selection origin of a focus move an expansion makes.
fn selection_origin(origin: GroupOrigin) -> SelectionOrigin {
    match origin {
        GroupOrigin::User => SelectionOrigin::User,
        GroupOrigin::Programmatic => SelectionOrigin::Programmatic,
    }
}

/// The deepest group of a projection that holds the row at `order_ix` of
/// `Projection::rows`, shown or not. Groups come in caption order, a group
/// before its subgroups, so the last one holding the row is the deepest.
/// O(groups): once per command or per shown order, never per frame.
pub(super) fn row_group(projection: &Projection, order_ix: usize) -> Option<GroupInfo<'_>> {
    projection
        .groups()
        .iter()
        .filter(|group| {
            let rows = group.rows();
            rows.first()
                .and_then(|first| projection.order_ix(*first))
                .is_some_and(|start| (start..start + rows.len()).contains(&order_ix))
        })
        .last()
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets how a press expands or collapses a group caption or a tree row
    /// (default [`StandardExpandGesture`](crate::StandardExpandGesture): the
    /// chevron toggles, with Alt/Option everything below it too, and a
    /// double click on a caption toggles it).
    pub fn with_expand_gesture(mut self, gesture: impl ExpandGesture) -> Self {
        self.expand_gesture = Box::new(gesture);
        self
    }

    /// The window x of the chevron lane of a caption at `depth`: the lanes
    /// of every level and of a tree's rows line up from the body's left
    /// edge.
    pub(super) fn caption_lane_x(&self, depth: u16) -> Pixels {
        self.body_bounds.origin.x + CELL_PADDING + INDENT * f32::from(depth)
    }

    /// The chevron, the caption or the tree cell under a window position,
    /// with its painted line: a caption anywhere, a tree row with children
    /// in its tree column's cell.
    pub(super) fn disclosure_hit(
        &self,
        position: Point<Pixels>,
    ) -> Option<(PaintedLine, ExpandArea)> {
        let line = self.painted.get(self.line_at(position)?)?;
        let lane = |lane_x: Pixels| (lane_x..lane_x + LEAD_ICON_LANE).contains(&position.x);
        match line {
            PaintedLine::Caption(path) => {
                let area = if lane(self.caption_lane_x(path.depth())) {
                    ExpandArea::Chevron
                } else {
                    ExpandArea::Caption
                };
                Some((line.clone(), area))
            }
            PaintedLine::Row(id) => {
                let projection = &self.projection;
                projection.branch(*id)?;
                let column = self.tree_column()?;
                if self.column_at_position(position) != Some(column) {
                    return None;
                }
                let depth = projection.line(projection.display_ix(*id)?)?.depth();
                let geometry = self
                    .columns
                    .geometry(self.body_bounds.size.width, self.scroll_x);
                let (zone, i) = self.columns.zone_position(column)?;
                let cell_x = self.body_bounds.origin.x
                    + geometry
                        .zone(zone)
                        .column_x(self.columns.zone_columns(zone), i);
                let area = if lane(cell_x + CELL_PADDING + INDENT * f32::from(depth)) {
                    ExpandArea::Chevron
                } else {
                    ExpandArea::TreeCell
                };
                Some((line.clone(), area))
            }
            // A group footer has no disclosure.
            PaintedLine::Footer(_) => None,
            // A fading row has no children.
            PaintedLine::Ghost(_) => None,
        }
    }

    /// The expand target of a painted line.
    fn line_expand_target(&self, line: &PaintedLine) -> Option<ExpandTarget<KeyOf<D>>> {
        match line {
            PaintedLine::Caption(path) => Some(ExpandTarget::Group(path.clone())),
            PaintedLine::Row(id) => Some(ExpandTarget::Row(self.snapshot.row(*id)?.key().clone())),
            // A group footer has no disclosure.
            PaintedLine::Footer(_) => None,
            // A fading row has no children.
            PaintedLine::Ghost(_) => None,
        }
    }

    /// A press on a caption or a tree row's chevron: the [`ExpandGesture`]
    /// decides, and the line is toggled at once (`GroupOrigin::User`).
    /// Returns true if it was.
    pub(super) fn press_disclosure(
        &mut self,
        line: &PaintedLine,
        area: ExpandArea,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let context =
            ExpandGestureContext::new(area, event.button, event.modifiers, event.click_count);
        let Some(edit) = self.expand_gesture.edit(&context) else {
            return false;
        };
        let Some(target) = self.line_expand_target(line) else {
            return false;
        };
        self.anchor_toggled(line);
        self.toggle_line(target, edit, GroupOrigin::User, cx)
    }

    /// Toggles a line: alone, or with every line below it (`ToggleDeep`).
    fn toggle_line(
        &mut self,
        target: ExpandTarget<KeyOf<D>>,
        edit: ExpandEdit,
        origin: GroupOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let expanded = !self.is_expanded(&target);
        match edit {
            ExpandEdit::ToggleDeep => self.apply_deep_expanded(target, expanded, origin, cx),
            _ => self.apply_expanded(target, expanded, origin, cx),
        }
    }

    /// Expands or collapses a line and every line below it in the shown
    /// projection: one `ExpansionChange`, with the line as its target.
    pub(crate) fn apply_deep_expanded(
        &mut self,
        target: ExpandTarget<KeyOf<D>>,
        expanded: bool,
        origin: GroupOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut expansion = self.view.expansion().clone();
        match &target {
            ExpandTarget::Group(path) => {
                if path.is_root() {
                    return false;
                }
                let projection = self.projection.clone();
                let below: Vec<GroupPath> = projection
                    .groups()
                    .iter()
                    .filter(|group| group.path().starts_with(path) && group.path() != path)
                    .map(|group| group.path().clone())
                    .collect();
                for path in std::iter::once(path.clone()).chain(below) {
                    let levels = self.group_levels(&path);
                    expansion = expansion.with_group(path, expanded, levels);
                }
            }
            ExpandTarget::Row(key) => {
                let (projection, snapshot) = (self.projection.clone(), self.snapshot.clone());
                let Some(tree) = projection.tree_data() else {
                    return false;
                };
                let Some((id, order_ix)) = snapshot
                    .row_id(key)
                    .and_then(|id| Some((id, projection.order_ix(id)?)))
                else {
                    return false;
                };
                let descendants = projection
                    .branch(id)
                    .map_or(0, |branch| branch.descendant_count());
                for ix in order_ix..=order_ix + descendants {
                    let Some(&row_id) = projection.rows().get(ix) else {
                        break;
                    };
                    if projection.branch(row_id).is_none() {
                        continue;
                    }
                    if let Some(row) = snapshot.row(row_id) {
                        expansion = expansion.with_row(row.key().clone(), expanded, tree.depth(ix));
                    }
                }
            }
        }
        self.apply_expansion(expansion, Some(target), origin, cx)
    }

    /// Keeps a toggled line on its screen line when the lines below it
    /// come and go.
    fn anchor_toggled(&mut self, line: &PaintedLine) {
        let Some(screen) = self.painted.iter().position(|painted| painted == line) else {
            return;
        };
        let anchor = match line {
            PaintedLine::Caption(path) => AnchorLine::Group(path.clone()),
            PaintedLine::Row(id) => AnchorLine::Row(*id),
            // A group footer is never toggled.
            PaintedLine::Footer(_) => return,
            // A fading row is never toggled.
            PaintedLine::Ghost(_) => return,
        };
        self.expand_anchor = Some(LineAnchor::new(anchor, screen, true));
    }

    /// Anchors the focused line, if painted, for a toggle from the keys.
    fn anchor_focused(&mut self) {
        let focused = self.selection.focus().and_then(|focus| {
            Some(match (focus.row(), focus.group()) {
                (Some(key), _) => PaintedLine::Row(self.snapshot.row_id(key)?),
                (None, Some(path)) => PaintedLine::Caption(path.clone()),
                (None, None) => return None,
            })
        });
        if let Some(line) = focused {
            self.anchor_toggled(&line);
        }
    }

    /// ← and → on a focused caption (WAI-ARIA treegrid): ← collapses an
    /// expanded caption, or moves to the enclosing one; → expands a
    /// collapsed caption, or moves to the first line of the group.
    pub(super) fn caption_arrow(&mut self, path: &GroupPath, right: bool, cx: &mut Context<Self>) {
        let target = ExpandTarget::Group(path.clone());
        let expanded = self.is_expanded(&target);
        if right != expanded {
            self.anchor_focused();
            self.apply_expanded(target, right, GroupOrigin::User, cx);
        } else if right {
            self.navigate(Navigation::Down, false, cx);
        } else if let Some(parent) = path.parent().filter(|parent| !parent.is_root()) {
            let focused = self.edit_selection_focus(&parent, SelectionOrigin::User, cx);
            if focused {
                self.reveal_focus();
                cx.notify();
            }
        }
    }

    /// `ExpandRow`: expands the focused caption or tree row with children;
    /// otherwise the key goes on.
    ///
    /// On a cell that typing would edit, `+` is the draft's text: the key
    /// goes on to typing.
    fn on_expand_row(&mut self, _: &ExpandRow, _: &mut Window, cx: &mut Context<Self>) {
        if self.would_type_into_focus("+", cx) {
            cx.propagate();
            return;
        }
        let target = match self.focus_target() {
            Some(LineTarget::Group(path)) => Some(ExpandTarget::Group(path)),
            Some(LineTarget::Row(key)) if self.is_tree_branch(&key) => Some(ExpandTarget::Row(key)),
            _ => None,
        };
        match target.filter(|target| !self.is_expanded(target)) {
            Some(target) => {
                self.anchor_focused();
                self.apply_expanded(target, true, GroupOrigin::User, cx);
            }
            None => cx.propagate(),
        }
    }

    /// `CollapseRow`: collapses the focused caption or expanded tree row with
    /// children; on a data row of a grouped view, its group, and the focus
    /// goes to the group's caption (as Excel's outline); on a tree leaf or
    /// a collapsed tree row, its parent, and the focus goes to the parent
    /// (WAI-ARIA treegrid's "move to parent", on `-` because ← moves
    /// between cells). Otherwise, on a root for one, the key goes on.
    ///
    /// On a cell that typing would edit, `-` is the draft's text: the key
    /// goes on to typing, so `-5` typed into a quantity does not collapse
    /// the group.
    fn on_collapse_row(&mut self, _: &CollapseRow, _: &mut Window, cx: &mut Context<Self>) {
        if self.would_type_into_focus("-", cx) {
            cx.propagate();
            return;
        }
        let target = match self.focus_target() {
            Some(LineTarget::Group(path)) => Some(ExpandTarget::Group(path)),
            Some(LineTarget::Row(key)) if self.projection.is_tree() => {
                let branch = ExpandTarget::Row(key.clone());
                if self.is_tree_branch(&key) && self.is_expanded(&branch) {
                    Some(branch)
                } else {
                    self.tree_parent(&key).map(ExpandTarget::Row)
                }
            }
            Some(LineTarget::Row(key)) => self
                .snapshot
                .row_id(&key)
                .and_then(|id| self.projection.order_ix(id))
                .and_then(|order_ix| row_group(&self.projection, order_ix))
                .map(|group| ExpandTarget::Group(group.path().clone())),
            None => None,
        };
        match target.filter(|target| self.is_expanded(target)) {
            Some(target) => {
                let line = match &target {
                    ExpandTarget::Group(path) => Some(PaintedLine::Caption(path.clone())),
                    ExpandTarget::Row(key) => self.snapshot.row_id(key).map(PaintedLine::Row),
                };
                if let Some(line) = line {
                    self.anchor_toggled(&line);
                }
                self.apply_expanded(target, false, GroupOrigin::User, cx);
            }
            None => cx.propagate(),
        }
    }

    /// `ExpandAll`, `CollapseAll`, `ShowCaptionsOnly`: a new default depth,
    /// every toggle cleared.
    fn set_depth_from_user(&mut self, depth: ExpandDepth, cx: &mut Context<Self>) {
        self.apply_expand_depth(depth, GroupOrigin::User, cx);
    }

    /// The keyboard Actions of expansion, on the grid root.
    pub(super) fn bind_disclosure_actions<E: InteractiveElement>(root: E, cx: &Context<Self>) -> E {
        root.on_action(cx.listener(Self::on_expand_row))
            .on_action(cx.listener(Self::on_collapse_row))
            .on_action(cx.listener(|this, _: &ExpandAll, _, cx| {
                this.set_depth_from_user(ExpandDepth::All, cx)
            }))
            .on_action(cx.listener(|this, _: &CollapseAll, _, cx| {
                this.set_depth_from_user(ExpandDepth::Levels(0), cx)
            }))
            .on_action(cx.listener(|this, _: &ShowCaptionsOnly, _, cx| {
                this.set_depth_from_user(ExpandDepth::Captions, cx)
            }))
    }

    /// A screen reader asks the row element with `key_hash` (a caption's
    /// group hash, a tree row's key hash) to expand or collapse
    /// (`on_a11y_action`), as its chevron would.
    pub(crate) fn on_a11y_disclosure(
        &mut self,
        content: RowContent,
        key_hash: u64,
        action: Action,
        cx: &mut Context<Self>,
    ) {
        let line = self.painted.iter().find(|line| match (line, content) {
            (PaintedLine::Caption(path), RowContent::Caption) => group_hash(path) == key_hash,
            (PaintedLine::Row(id), RowContent::Data) => self
                .snapshot
                .row(*id)
                .is_some_and(|row| super::key_hash(row.key()) == key_hash),
            _ => false,
        });
        let Some(line) = line.cloned() else {
            return;
        };
        let Some(target) = self.line_expand_target(&line) else {
            return;
        };
        let expanded = match action {
            Action::Expand => true,
            Action::Collapse => false,
            _ => return,
        };
        if self.is_expanded(&target) != expanded {
            self.anchor_toggled(&line);
            self.apply_expanded(target, expanded, GroupOrigin::User, cx);
        }
    }

    /// Sends a screen reader's `action` to the accessibility row element
    /// with `row_id` (`NamedInteger("live-grid-group-row", group hash)` of
    /// a caption, `NamedInteger("live-grid-row", key hash)` of a tree row),
    /// as GPUI does through the element's `on_a11y_action`, which a headless
    /// window never calls. For tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn send_a11y_action(
        &mut self,
        row_id: &gpui_kit::ElementId,
        action: Action,
        cx: &mut Context<Self>,
    ) {
        let gpui_kit::ElementId::NamedInteger(name, key_hash) = row_id else {
            return;
        };
        let content = match name.as_ref() {
            "live-grid-group-row" => RowContent::Caption,
            "live-grid-row" => RowContent::Data,
            _ => return,
        };
        self.on_a11y_disclosure(content, *key_hash, action, cx);
    }

    /// Moves the focus out of what the view's expansion hides, at once,
    /// before the lines are laid out again: a focused row or caption under a
    /// collapsed group or tree row moves to the caption or row of the
    /// outermost collapsed one, the deepest line still shown. The
    /// `FocusChange` follows the `ExpansionChange`, with the same origin.
    pub(super) fn focus_off_collapsed(&mut self, origin: GroupOrigin, cx: &mut Context<Self>) {
        let Some(target) = self.collapsed_ancestor() else {
            return;
        };
        let mark = self.selection.mark();
        let column = self.selection.focus().map(|focus| focus.column());
        if let Some(column) = column {
            self.selection.focus_on(match target {
                LineTarget::Group(path) => FocusedCell::caption(path, column),
                LineTarget::Row(key) => FocusedCell::new(key, column),
            });
        }
        self.finish_change(mark, selection_origin(origin), cx);
    }

    /// The outermost collapsed group or tree row, by the view's expansion,
    /// above the focused line in the shown projection; `None` if nothing
    /// above it is collapsed.
    fn collapsed_ancestor(&self) -> Option<LineTarget<KeyOf<D>>> {
        let focus = self.selection.focus()?;
        let (projection, snapshot) = (&self.projection, &self.snapshot);
        let expansion = self.view.expansion();
        if let Some(path) = focus.group() {
            let group = projection.groups().find(path)?;
            return self
                .outermost_collapsed(group.parent().and_then(|id| projection.groups().get(id)));
        }
        let id = snapshot.row_id(focus.row()?)?;
        if projection.is_grouped() {
            let group = row_group(projection, projection.order_ix(id)?)?;
            return self.outermost_collapsed(Some(group));
        }
        if !projection.is_tree() {
            return None;
        }
        // The tree ancestors the projection holds, nearest first.
        let mut ancestors: Vec<KeyOf<D>> = Vec::new();
        let mut row = snapshot.row(id)?;
        while ancestors.len() < usize::from(u16::MAX) {
            let Some(parent) = row
                .parent_key()
                .and_then(|parent| snapshot.row_id(parent))
                .filter(|parent| projection.contains(*parent))
            else {
                break;
            };
            row = snapshot.row(parent)?;
            ancestors.push(row.key().clone());
        }
        ancestors
            .iter()
            .rev()
            .enumerate()
            .find(|(depth, key)| !expansion.is_row_expanded(key, *depth as u16))
            .map(|(_, key)| LineTarget::Row(key.clone()))
    }

    /// The outermost collapsed group from `group` up, by the view's
    /// expansion.
    fn outermost_collapsed(&self, group: Option<GroupInfo<'_>>) -> Option<LineTarget<KeyOf<D>>> {
        let projection = &self.projection;
        let expansion = self.view.expansion();
        let mut collapsed = None;
        let mut group = group;
        while let Some(info) = group {
            let path = info.path();
            if !expansion.is_group_expanded(path, self.group_levels(path)) {
                collapsed = Some(path.clone());
            }
            group = info.parent().and_then(|id| projection.groups().get(id));
        }
        collapsed.map(LineTarget::Group)
    }

    /// Keeps the focus on a line the newly shown projection shows, after
    /// `ProjectionChange`: a focused caption whose group is gone goes to the
    /// line at its place, else the nearest one above; a focused row a
    /// rebuild moved under a collapsed group or tree row goes to the caption
    /// or row of the outermost collapsed one. Both are `Filtered`; the
    /// caller emits the events.
    pub(super) fn follow_shown_lines(&mut self, previous: &Projection) {
        let Some(focus) = self.selection.focus() else {
            return;
        };
        let column = focus.column();
        if let Some(path) = focus.group() {
            if let Some(group) = self.projection.groups().find(path) {
                if self.projection.group_line(group.id()).is_some() {
                    return;
                }
            } else {
                let place = previous
                    .groups()
                    .find(path)
                    .and_then(|group| previous.group_line(group.id()))
                    .unwrap_or(0);
                if let Some(cell) = self.focusable_at_or_above(place, column) {
                    self.selection.focus_on(cell);
                    return;
                }
            }
        } else {
            let shown = focus
                .row()
                .and_then(|key| self.snapshot.row_id(key))
                .map(|id| {
                    self.projection.display_ix(id).is_some() || !self.projection.contains(id)
                });
            if shown != Some(false) {
                return;
            }
        }
        if let Some(target) = self.collapsed_ancestor() {
            self.selection.focus_on(match target {
                LineTarget::Group(path) => FocusedCell::caption(path, column),
                LineTarget::Row(key) => FocusedCell::new(key, column),
            });
        }
    }

    /// The line at `place` of the shown projection that takes the focus,
    /// else the nearest one above, else below.
    fn focusable_at_or_above(
        &self,
        place: usize,
        column: ColumnOf<D>,
    ) -> Option<FocusedCell<KeyOf<D>, ColumnOf<D>>> {
        let (projection, snapshot) = (&self.projection, &self.snapshot);
        let len = projection.len();
        let place = place.min(len.checked_sub(1)?);
        let cell_at = |ix: usize| -> Option<FocusedCell<KeyOf<D>, ColumnOf<D>>> {
            match projection.line(ix)? {
                DisplayRow::Data { row, .. } => {
                    Some(FocusedCell::new(snapshot.row(row)?.key().clone(), column))
                }
                DisplayRow::GroupCaption { group, .. } => Some(FocusedCell::caption(
                    projection.groups().get(group)?.path().clone(),
                    column,
                )),
                _ => None,
            }
        };
        (0..=place)
            .rev()
            .find_map(cell_at)
            .or_else(|| (place + 1..len).find_map(cell_at))
    }

    /// Focuses the caption at `path`, if shown; returns true if it did.
    fn edit_selection_focus(
        &mut self,
        path: &GroupPath,
        origin: SelectionOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let mark = self.selection.mark();
        let (projection, snapshot) = (self.projection.clone(), self.snapshot.clone());
        let rows = ShownRows::new(&projection, &snapshot);
        let focused = self.selection.focus_caption(path, rows, &self.columns);
        self.finish_change(mark, origin, cx);
        focused
    }
}
