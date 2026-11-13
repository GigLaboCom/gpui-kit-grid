//! The group panel: a strip above the header with the grouping levels as
//! chips. The user drags a column header into it to group by the column,
//! drags a chip to reorder the levels or out onto the grid to remove one,
//! clicks a chip to reverse its groups and removes it with its button; the
//! same commands are in the chip's menu, in the column menu
//! (`grouping_items`) and Actions.
//!
//! The panel shows only when `LiveGrid::group_panel(true)` asks for it. It
//! is one header row tall, inside the grid root and above the header, in
//! the key context [`GROUP_PANEL_KEY_CONTEXT`]. Every change goes through
//! `apply_group_by(.., GroupOrigin::User)`: one `GroupChange` per gesture.
//!
//! Element ids: the panel `"live-grid-group-panel"`; a chip
//! `NamedChild("live-grid-group-chip", level id)` and its remove button
//! `NamedChild("live-grid-group-chip-remove", level id)`; the hint
//! `"live-grid-group-panel-hint"`, the insertion line
//! `"live-grid-group-panel-gap"`.
//!
//! Accessibility: the panel is a `Toolbar` named "Grouping" (the hint is its
//! description while nothing is grouped); a chip is a `Button` named "Title,
//! Sorted ascending" with its level's position; the remove button is a
//! `Button` named "Remove grouping by Title"; separators have no node.
//!
//! Keyboard: chips and their remove buttons are Tab stops, after the grid's
//! own. Enter and Space press them (the grid's Enter and Space are unbound
//! in the panel); Delete and Backspace remove the focused chip's level
//! (`UngroupColumn`) and pass the focus to the next chip; Shift+F10 and the
//! Menu key open the chip's menu (`ShowContextMenu`).

use std::sync::Arc;

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size as ComponentSize, ThemeStyled as _,
    button::{Button, ButtonVariants as _},
    menu::{PopupMenu, PopupMenuItem},
    tag::Tag,
};
use gpui_kit::{
    AnyElement, AppContext as _, Bounds, ClickEvent, Context, CursorStyle, Div, DragMoveEvent,
    ElementId, EntityId, FocusHandle, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Point, Render, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, WeakEntity, Window, div, point, prelude::FluentBuilder as _, px, size,
};
use rust_i18n::t;

use super::{
    CELL_PADDING, ColumnOf, GridMenuContext, GridTarget, LiveGridDelegate, LiveGridState,
    columns::column_of, events::GroupOrigin, ghost::GhostTextStyle, header::DragSource,
};
use crate::{
    actions::{
        ClearGrouping, CollapseAll, ExpandAll, GROUP_PANEL_KEY_CONTEXT, GroupByColumn,
        ShowCaptionsOnly, ShowContextMenu, UngroupColumn,
    },
    engine::{ExpandDepth, GroupBy, SortDirection},
    model::{ColumnId, ColumnIx, GridColumns, GridRow},
    policy::GridArea,
};

/// The id of the panel element.
const PANEL_ID: &str = "live-grid-group-panel";
/// The id prefix of a chip, named by its level id.
const CHIP_ID: &str = "live-grid-group-chip";
/// The id prefix of a chip's remove button, named by its level id.
const CHIP_REMOVE_ID: &str = "live-grid-group-chip-remove";
/// The id of the hint shown while nothing is grouped.
const HINT_ID: &str = "live-grid-group-panel-hint";
/// The id of the insertion line of a drag over the panel.
const GAP_ID: &str = "live-grid-group-panel-gap";
/// The room between a chip and a separator.
const CHIP_GAP: Pixels = px(4.);
/// The insertion line's width, as the header's.
const GAP_LINE: Pixels = px(2.);

/// The group panel's state.
#[derive(Default)]
pub(super) struct GroupPanelState {
    /// `LiveGrid::group_panel` shows the panel.
    shown: bool,
    /// The grid's size; chips are one step smaller.
    size: ComponentSize,
    /// The chips' focus handles by level id, for the levels of the last
    /// frame.
    chips: Vec<(ColumnId, FocusHandle)>,
    /// The chips' bounds in the last frame (window coordinates), left to
    /// right: the drop gaps and the press targets.
    chip_bounds: Vec<Bounds<Pixels>>,
    /// What a drop at the pointer would do while a drag is over the panel.
    target: Option<PanelTarget>,
    /// The chip drag in progress.
    dragged: Option<LevelDrag>,
    /// The last primary press on the panel closed the context menu or
    /// opened one: its click changes nothing (as a header press).
    menu_press: bool,
}

impl GroupPanelState {
    /// Whether `LiveGrid::group_panel` shows the panel.
    pub(super) fn is_shown(&self) -> bool {
        self.shown
    }

    fn chip_handle(&self, level: &ColumnId) -> Option<&FocusHandle> {
        self.chips
            .iter()
            .find(|(id, _)| id == level)
            .map(|(_, handle)| handle)
    }
}

/// What a drop on the panel would do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PanelTarget {
    /// The level lands in gap `gap` between the chips (0 before the first);
    /// the insertion line is at `x` from the panel's left edge.
    Gap { gap: usize, x: Pixels },
    /// Accepted, but nothing changes: a level dropped where it is.
    Here,
    /// Refused: a band, a column that is not groupable, a tree view. The
    /// "not allowed" cursor shows.
    Refused,
}

impl PanelTarget {
    /// Whether the panel takes the drag (the cursor is not "not allowed").
    pub(super) fn is_accepted(self) -> bool {
        self != Self::Refused
    }
}

/// A chip drag in progress.
struct LevelDrag {
    /// The cursor shown.
    cursor: Option<CursorStyle>,
}

/// The payload of a chip drag: renders the ghost as a chip, in the chip's
/// font.
#[derive(Clone)]
pub(crate) struct DraggedGroupLevel {
    grid: EntityId,
    level: ColumnId,
    /// The column's header icon, before the title.
    icon: Option<SharedString>,
    title: SharedString,
    direction: SortDirection,
    size: ComponentSize,
    text: GhostTextStyle,
}

impl Render for DraggedGroupLevel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        self.text.root().child(
            Tag::new()
                .outline()
                .with_size(self.size)
                .gap_1()
                .bg(theme.table_head)
                .text_color(theme.table_head_foreground)
                .shadow_md()
                .opacity(0.9)
                .children(self.icon.clone().map(level_icon))
                .child(self.title.clone())
                .child(Icon::new(direction_icon(self.direction)).size_3()),
        )
    }
}

/// What the grouping items of a column menu know (`grouping_items`).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MenuGrouping {
    /// The target column's grouping; `None` without a target column.
    column: Option<ColumnGrouping>,
    /// Some level groups the view.
    grouped: bool,
}

#[derive(Clone, Copy, Debug)]
struct ColumnGrouping {
    /// The user may group by it: it is groupable and the view is no tree.
    groupable: bool,
    /// It is a grouping level.
    level: bool,
}

impl<'a, R: GridRow> GridMenuContext<'a, R> {
    /// "Group by this column" (`GroupByColumn`), "Ungroup" (`UngroupColumn`)
    /// for the target column, then "Ungroup all" (`ClearGrouping`). Items
    /// that would do nothing are disabled: grouping by a column that is not
    /// groupable or already a level, or in a tree view (a tree and a
    /// grouping exclude each other, as in AG Grid and DevExpress);
    /// ungrouping a column that is no level; ungrouping all without a
    /// grouping. Without a target column, only "Ungroup all".
    /// [`Self::column_items`] ends with them.
    pub fn grouping_items(&self, menu: PopupMenu) -> PopupMenu {
        grouping_items(self.grouping, menu)
    }
}

/// [`GridMenuContext::grouping_items`].
pub(super) fn grouping_items(grouping: MenuGrouping, menu: PopupMenu) -> PopupMenu {
    menu.when_some(grouping.column, |menu, column| {
        menu.menu_with_disabled(
            label("live_grid.menu.group_by"),
            Box::new(GroupByColumn),
            !column.groupable || column.level,
        )
        .menu_with_disabled(
            label("live_grid.menu.ungroup"),
            Box::new(UngroupColumn),
            !column.level,
        )
    })
    .menu_with_disabled(
        label("live_grid.menu.clear_grouping"),
        Box::new(ClearGrouping),
        !grouping.grouped,
    )
}

/// A text from this crate's catalog.
fn label(key: &str) -> SharedString {
    SharedString::from(t!(key).into_owned())
}

/// The icon of a level's direction, as the header's sort indicator.
fn direction_icon(direction: SortDirection) -> IconName {
    match direction {
        SortDirection::Ascending => IconName::SortAscending,
        SortDirection::Descending => IconName::SortDescending,
    }
}

fn reversed(direction: SortDirection) -> SortDirection {
    match direction {
        SortDirection::Ascending => SortDirection::Descending,
        SortDirection::Descending => SortDirection::Ascending,
    }
}

/// A column's header icon on a chip, at the size of the direction icon.
fn level_icon(path: SharedString) -> Icon {
    Icon::empty().path(path).size_3()
}

/// The accessible name of a chip: "Instrument, Sorted ascending".
fn chip_label(title: &str, direction: SortDirection) -> SharedString {
    let direction = match direction {
        SortDirection::Ascending => t!("live_grid.sort.ascending"),
        SortDirection::Descending => t!("live_grid.sort.descending"),
    };
    let text = t!(
        "live_grid.group_panel.chip",
        title = title,
        direction = direction
    );
    SharedString::from(text.into_owned())
}

/// The name and tooltip of a chip's remove button.
fn remove_label(title: &str) -> SharedString {
    SharedString::from(t!("live_grid.group_panel.remove", title = title).into_owned())
}

/// The id of a chip part, named by its level: identity by level, never by
/// position.
fn chip_part_id(part: &'static str, level: &ColumnId) -> ElementId {
    ElementId::NamedChild(
        Arc::new(ElementId::Name(part.into())),
        SharedString::from(level.as_str().to_owned()),
    )
}

/// The remove button's size inside a chip of `size`: it fits the chip's
/// text line, so the chip stays inside the panel's row.
fn remove_button_size(size: ComponentSize) -> ComponentSize {
    match size {
        ComponentSize::Large | ComponentSize::Medium => ComponentSize::Small,
        _ => ComponentSize::XSmall,
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Shows or hides the group panel, for chips of one step below `size`
    /// (from the [`crate::LiveGrid`] flag and size).
    pub(crate) fn set_group_panel_shown(
        &mut self,
        shown: bool,
        size: ComponentSize,
        cx: &mut Context<Self>,
    ) {
        let panel = &mut self.group_panel;
        if panel.shown == shown && panel.size == size {
            return;
        }
        panel.shown = shown;
        panel.size = size;
        panel.target = None;
        // The body gets another height: the next frame lays it out.
        cx.notify();
    }

    /// Whether the group panel shows (`LiveGrid::group_panel`), as of the
    /// last `LiveGrid` render.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_group_panel_shown(&self) -> bool {
        self.group_panel.shown
    }

    /// The group panel's height: one header row, or zero when hidden.
    pub(super) fn group_panel_height(&self) -> Pixels {
        if self.group_panel.shown {
            self.row_height
        } else {
            px(0.)
        }
    }

    /// The panel's window bounds: above the header, as wide as the body;
    /// arithmetic from the body's bounds, as the header's hit test.
    fn group_panel_bounds(&self) -> Option<Bounds<Pixels>> {
        let height = self.group_panel_height();
        (height > px(0.)).then(|| {
            let body = self.body_bounds;
            Bounds::new(
                point(body.origin.x, body.origin.y - self.header_height() - height),
                size(body.size.width, height),
            )
        })
    }

    /// Whether a window position is over the shown group panel.
    pub(super) fn is_over_group_panel(&self, position: Point<Pixels>) -> bool {
        self.group_panel_bounds()
            .is_some_and(|bounds| bounds.contains(&position))
    }

    /// Whether the user may group by a column now: it is groupable and the
    /// view is no tree. `set_group_by` groups by any column.
    fn accepts_user_grouping(&self, column: ColumnIx) -> bool {
        self.tree().is_none()
            && self
                .columns
                .get(column)
                .is_some_and(|column| column.groupable)
    }

    /// The position of a level among the grouping levels.
    fn level_ix(&self, level: &ColumnId) -> Option<usize> {
        self.view
            .group_by()
            .iter()
            .position(|kept| kept.id() == *level)
    }

    /// The grouping of a column menu's target column, for `show_menu`.
    pub(super) fn menu_grouping(&self, column: Option<ColumnOf<D>>) -> MenuGrouping {
        MenuGrouping {
            column: column.map(|column| ColumnGrouping {
                groupable: self.accepts_user_grouping(column.ix()),
                level: self.level_ix(&column.column_id()).is_some(),
            }),
            grouped: !self.view.group_by().is_empty(),
        }
    }

    /// Edits a copy of the levels and applies it as a user action; `edit`
    /// returns false to leave them. Returns true if the grouping changed
    /// (one `GroupChange { origin: User }`).
    fn edit_levels(
        &mut self,
        edit: impl FnOnce(&mut Vec<GroupBy<D::Row>>) -> bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut levels = self.view.group_by().to_vec();
        edit(&mut levels) && self.apply_group_by(levels, GroupOrigin::User, cx)
    }

    /// Puts `column` into gap `gap` of the levels, ascending; a column that
    /// is a level already moves there with its direction.
    fn insert_column_level(&mut self, column: ColumnOf<D>, gap: usize, cx: &mut Context<Self>) {
        let id = column.column_id();
        self.edit_levels(
            |levels| {
                let (level, gap) = match levels.iter().position(|level| level.id() == id) {
                    Some(ix) => (levels.remove(ix), if gap > ix { gap - 1 } else { gap }),
                    None => (GroupBy::column(column), gap),
                };
                levels.insert(gap.min(levels.len()), level);
                true
            },
            cx,
        );
    }

    /// Moves a level into gap `gap` of the levels as they are now.
    fn move_level_to(&mut self, level: &ColumnId, gap: usize, cx: &mut Context<Self>) {
        let Some(ix) = self.level_ix(level) else {
            return;
        };
        self.edit_levels(
            |levels| {
                let moved = levels.remove(ix);
                let gap = if gap > ix { gap - 1 } else { gap };
                levels.insert(gap.min(levels.len()), moved);
                true
            },
            cx,
        );
    }

    /// Moves a level one place left (`-1`) or right (`1`).
    fn move_level_by(&mut self, level: &ColumnId, step: isize, cx: &mut Context<Self>) {
        let Some(ix) = self.level_ix(level) else {
            return;
        };
        let Some(other) = ix.checked_add_signed(step) else {
            return;
        };
        self.edit_levels(
            |levels| {
                let swapped = other < levels.len();
                if swapped {
                    levels.swap(ix, other);
                }
                swapped
            },
            cx,
        );
    }

    /// Sets the direction of a level's groups; `None` reverses it.
    fn set_level_direction(
        &mut self,
        level: &ColumnId,
        direction: Option<SortDirection>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.level_ix(level) else {
            return;
        };
        self.edit_levels(
            |levels| {
                let current = levels[ix].direction();
                let direction = direction.unwrap_or_else(|| reversed(current));
                levels[ix] = levels[ix].clone().with_direction(direction);
                direction != current
            },
            cx,
        );
    }

    /// Takes a level out of the grouping; returns false if it was none.
    fn remove_level(&mut self, level: &ColumnId, cx: &mut Context<Self>) -> bool {
        self.edit_levels(
            |levels| {
                let count = levels.len();
                levels.retain(|kept| kept.id() != *level);
                levels.len() != count
            },
            cx,
        )
    }

    /// [`Self::remove_level`] from the panel. With `refocus`, the focus passes
    /// to the chip on the right, else on the left, else to the grid: the
    /// removed chip had it (a key on the chip or its remove button).
    fn ungroup_level(
        &mut self,
        level: &ColumnId,
        refocus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.level_ix(level) else {
            return;
        };
        let levels = self.view.group_by();
        let neighbour = levels
            .get(ix + 1)
            .or_else(|| ix.checked_sub(1).and_then(|left| levels.get(left)))
            .map(GroupBy::id);
        let removed = self.remove_level(level, cx);
        if removed && refocus {
            let handle = neighbour
                .and_then(|id| self.group_panel.chip_handle(&id))
                .unwrap_or(&self.focus_handle)
                .clone();
            window.focus(&handle, cx);
        }
    }

    // The grouping Actions on the grid root: over `current_column()`. On a
    // chip, its own `UngroupColumn` handler runs first and keeps the Action.

    fn on_group_by_column(&mut self, _: &GroupByColumn, _: &mut Window, cx: &mut Context<Self>) {
        let Some(column) = self.current_column() else {
            return;
        };
        if !self.accepts_user_grouping(column.ix()) {
            return;
        }
        let id = column.column_id();
        self.edit_levels(
            |levels| {
                let added = levels.iter().all(|level| level.id() != id);
                if added {
                    levels.push(GroupBy::column(column));
                }
                added
            },
            cx,
        );
    }

    fn on_ungroup_column(&mut self, _: &UngroupColumn, _: &mut Window, cx: &mut Context<Self>) {
        let Some(column) = self.current_column() else {
            return;
        };
        self.remove_level(&column.column_id(), cx);
    }

    fn on_clear_grouping(&mut self, _: &ClearGrouping, _: &mut Window, cx: &mut Context<Self>) {
        self.edit_levels(
            |levels| {
                let grouped = !levels.is_empty();
                levels.clear();
                grouped
            },
            cx,
        );
    }

    /// Binds the grouping Actions and the chip drags to the grid root: a
    /// chip dropped on the header or the body removes its level.
    pub(super) fn bind_group_panel<E: InteractiveElement>(root: E, cx: &Context<Self>) -> E {
        root.on_action(cx.listener(Self::on_group_by_column))
            .on_action(cx.listener(Self::on_ungroup_column))
            .on_action(cx.listener(Self::on_clear_grouping))
            .on_drag_move(cx.listener(Self::on_level_drag_move))
            .on_drop(cx.listener(Self::on_level_drop))
    }

    // Drops on the panel.

    /// The gap nearest to window `x` between the chips (by their centres)
    /// and its insertion line from the panel's left edge. Without chips (or
    /// before they are painted) the one gap is at the end.
    fn panel_gap_at(&self, x: Pixels) -> (usize, Pixels) {
        let count = self.view.group_by().len();
        let chips = &self.group_panel.chip_bounds;
        let left = self.body_bounds.origin.x;
        if chips.len() != count || count == 0 {
            let x = chips
                .last()
                .map_or(CELL_PADDING, |chip| chip.right() + CHIP_GAP / 2. - left);
            return (count, x);
        }
        let gap = chips.iter().take_while(|chip| x >= chip.center().x).count();
        let line = match gap {
            0 => chips[0].left() - CHIP_GAP / 2.,
            gap if gap == count => chips[count - 1].right() + CHIP_GAP / 2.,
            gap => (chips[gap - 1].right() + chips[gap].left()) / 2.,
        };
        (gap, line - left)
    }

    /// What dropping a header drag at `position` over the panel does: a
    /// column the user may group by lands in a gap; a band, a column that
    /// is not groupable and a tree view are refused.
    fn panel_column_target(&self, source: DragSource, position: Point<Pixels>) -> PanelTarget {
        let DragSource::Column(ix) = source else {
            return PanelTarget::Refused;
        };
        if !self.accepts_user_grouping(ix) {
            return PanelTarget::Refused;
        }
        let (gap, x) = self.panel_gap_at(position.x);
        match self.level_ix(&column_of::<ColumnOf<D>>(ix).column_id()) {
            Some(level) if gap == level || gap == level + 1 => PanelTarget::Here,
            _ => PanelTarget::Gap { gap, x },
        }
    }

    /// What dropping a chip at `position` over the panel does: its level
    /// moves to a gap; its own place changes nothing.
    fn panel_level_target(&self, level: &ColumnId, position: Point<Pixels>) -> PanelTarget {
        let Some(ix) = self.level_ix(level) else {
            return PanelTarget::Refused;
        };
        let (gap, x) = self.panel_gap_at(position.x);
        if gap == ix || gap == ix + 1 {
            PanelTarget::Here
        } else {
            PanelTarget::Gap { gap, x }
        }
    }

    fn set_panel_target(&mut self, target: Option<PanelTarget>, cx: &mut Context<Self>) {
        if self.group_panel.target != target {
            self.group_panel.target = target;
            cx.notify();
        }
    }

    /// Follows a header drag at `position` (`None`: outside the grid).
    /// Returns what a drop there would do on the panel, or `None` if the
    /// pointer is not over it: then the header drag moves the column.
    pub(super) fn refresh_panel_column_target(
        &mut self,
        source: DragSource,
        position: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) -> Option<PanelTarget> {
        let target = position
            .filter(|position| self.is_over_group_panel(*position))
            .map(|position| self.panel_column_target(source, position));
        self.set_panel_target(target, cx);
        target
    }

    /// A header dropped on the panel: its column becomes a level in the gap
    /// (or its level moves there); the column itself does not move.
    pub(super) fn drop_column_on_group_panel(
        &mut self,
        source: DragSource,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let (PanelTarget::Gap { gap, .. }, DragSource::Column(ix)) =
            (self.panel_column_target(source, position), source)
        {
            self.insert_column_level(column_of(ix), gap, cx);
        }
    }

    /// No drag is over the panel any more.
    pub(super) fn clear_panel_target(&mut self) {
        self.group_panel.target = None;
    }

    // Chip drags.

    /// A chip drag started (the payload's constructor): the pointer stops
    /// counting as over the rows, as in a header drag.
    fn begin_level_drag(&mut self, cx: &mut Context<Self>) {
        self.group_panel.dragged = Some(LevelDrag { cursor: None });
        self.column_gesture_started(cx);
        cx.notify();
    }

    fn end_level_drag(&mut self) {
        self.group_panel.dragged = None;
        self.group_panel.target = None;
    }

    /// Whether a chip is being dragged.
    pub(super) fn is_level_dragged(&self) -> bool {
        self.group_panel.dragged.is_some()
    }

    fn on_level_drag_move(
        &mut self,
        event: &DragMoveEvent<DraggedGroupLevel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx);
        if drag.grid != cx.entity_id() {
            return;
        }
        let level = drag.level.clone();
        if self.group_panel.dragged.is_none() {
            self.begin_level_drag(cx);
        }
        let position = event.event.position;
        let inside = event.bounds.contains(&position);
        let target = (inside && self.is_over_group_panel(position))
            .then(|| self.panel_level_target(&level, position));
        self.set_panel_target(target, cx);
        // On the grid the chip moves or goes; outside it nothing happens.
        let cursor = if inside {
            CursorStyle::ClosedHand
        } else {
            CursorStyle::OperationNotAllowed
        };
        if let Some(dragged) = &mut self.group_panel.dragged
            && dragged.cursor != Some(cursor)
        {
            dragged.cursor = Some(cursor);
            cx.set_active_drag_cursor_style(cursor, window);
        }
    }

    /// A chip dropped on its grid: in the panel its level moves to the gap;
    /// on the header or the body its level goes (as in Syncfusion). Dropped
    /// outside the grid, nothing happens.
    fn on_level_drop(
        &mut self,
        drag: &DraggedGroupLevel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if drag.grid != cx.entity_id() {
            return;
        }
        let position = window.mouse_position();
        self.end_level_drag();
        if self.is_over_group_panel(position) {
            if let PanelTarget::Gap { gap, .. } = self.panel_level_target(&drag.level, position) {
                self.move_level_to(&drag.level, gap, cx);
            }
        } else {
            self.ungroup_level(&drag.level, false, window, cx);
        }
        cx.notify();
    }

    /// The left button went up anywhere: a chip drag ends in the drop
    /// handler, which runs later in this dispatch, or here after it.
    pub(super) fn on_level_drag_mouse_up(&mut self, cx: &mut Context<Self>) {
        if self.group_panel.dragged.is_none() {
            return;
        }
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = this.update(cx, |state, cx| {
                if state.group_panel.dragged.is_some() {
                    state.end_level_drag();
                    cx.notify();
                }
            });
        });
    }

    // Chip gestures and the panel's menu.

    /// A click on a chip (or Enter and Space on it) reverses its level.
    fn on_chip_click(&mut self, level: &ColumnId, event: &ClickEvent, cx: &mut Context<Self>) {
        if !event.standard_click() || (self.group_panel.menu_press && !event.is_keyboard()) {
            return;
        }
        self.set_level_direction(level, None, cx);
    }

    /// The remove button: from the keyboard, the focus moves on.
    fn on_chip_remove(
        &mut self,
        level: &ColumnId,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.group_panel.menu_press && !event.is_keyboard() {
            return;
        }
        self.ungroup_level(level, event.is_keyboard(), window, cx);
    }

    /// A press on the panel. One the [`ContextMenuGesture`] takes (the
    /// panel counts as the header) opens the menu of the chip under it, or
    /// the panel's; a primary one focuses the grid, so a chip never takes
    /// the focus from a click (desktop buttons do not).
    ///
    /// [`ContextMenuGesture`]: crate::ContextMenuGesture
    fn on_group_panel_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let primary = event.button == MouseButton::Left;
        if primary {
            self.group_panel.menu_press = self.is_menu_open();
            if self.group_panel.menu_press {
                return;
            }
        }
        if self.is_context_press(
            event.button,
            event.modifiers,
            event.click_count,
            GridArea::Header,
        ) {
            self.group_panel.menu_press = primary;
            window.focus(&self.focus_handle, cx);
            let level = self.chip_at(event.position);
            self.open_group_panel_menu(level, event.position, window, cx);
            return;
        }
        if primary && !self.is_menu_open() {
            window.focus(&self.focus_handle, cx);
        }
    }

    /// The level of the chip at a window position in the last frame.
    fn chip_at(&self, position: Point<Pixels>) -> Option<ColumnId> {
        let levels = self.view.group_by();
        let chips = &self.group_panel.chip_bounds;
        if chips.len() != levels.len() {
            return None;
        }
        chips
            .iter()
            .position(|chip| chip.contains(&position))
            .map(|ix| levels[ix].id())
    }

    /// Shift+F10 or the Menu key on a chip: its menu, below it.
    fn show_chip_menu(&mut self, level: &ColumnId, window: &mut Window, cx: &mut Context<Self>) {
        let position = self
            .level_ix(level)
            .and_then(|ix| self.group_panel.chip_bounds.get(ix))
            .map(Bounds::bottom_left)
            .or_else(|| self.group_panel_bounds().map(|panel| panel.bottom_left()))
            .unwrap_or_default();
        self.open_group_panel_menu(Some(level.clone()), position, window, cx);
    }

    /// Opens the menu of a chip (`Some` level) or of the panel after the
    /// current event, in the grid's menu layer.
    fn open_group_panel_menu(
        &mut self,
        level: Option<ColumnId>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let _ = this.update(cx, |state, cx| {
                state.build_group_panel_menu(level, position, window, cx)
            });
        });
    }

    /// Builds the panel's own menu, not the delegate's `context_menu`: for a
    /// chip, its direction, order and removal; for both, Ungroup all and the
    /// expansion of every group. A chip whose level went meanwhile opens no
    /// menu.
    fn build_group_panel_menu(
        &mut self,
        level: Option<ColumnId>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let replaced = self.menu.take().is_some();
        let levels = self.view.group_by();
        let chip = match level {
            Some(level) => match self.level_ix(&level) {
                Some(ix) => Some((ix, level, levels[ix].direction())),
                None => {
                    if replaced {
                        self.reconsider_held(cx);
                    }
                    cx.notify();
                    return;
                }
            },
            None => None,
        };
        let count = levels.len();
        let captions_only = self.expand_depth() == ExpandDepth::Captions;
        let focus = self.focus_handle.clone();
        let this = cx.weak_entity();
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            panel_menu_items(
                menu.action_context(focus),
                &this,
                chip,
                count,
                captions_only,
            )
        });
        self.install_menu(menu, GridTarget::Empty, position, window, cx);
        cx.notify();
    }

    // Rendering.

    /// Keeps a focus handle per level, created once and dropped with the
    /// level, so the Tab order and the focus survive frames.
    fn sync_chip_handles(&mut self, cx: &mut Context<Self>) {
        let levels = self.view.group_by();
        let chips = &mut self.group_panel.chips;
        chips.retain(|(id, _)| levels.iter().any(|level| level.id() == *id));
        for level in levels {
            let id = level.id();
            if chips.iter().all(|(kept, _)| *kept != id) {
                chips.push((id, cx.focus_handle().tab_stop(true)));
            }
        }
    }

    /// A level's header icon: its column's, if the header shows one.
    fn level_icon(&self, level: &GroupBy<D::Row>) -> Option<SharedString> {
        let column = level.as_column()?;
        self.columns.get(column.ix())?.icon().cloned()
    }

    /// A level's title: its column's, or its key's (`GroupKey::title`).
    fn level_title(&self, level: &GroupBy<D::Row>) -> SharedString {
        match (level.as_column(), level.as_key()) {
            (Some(column), _) => self.columns.column(column.ix()).title.clone(),
            (None, Some(key)) => SharedString::from(key.title().to_owned()),
            (None, None) => SharedString::default(),
        }
    }

    /// The group panel, if `LiveGrid` shows it.
    pub(super) fn render_group_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<D>> {
        if !self.group_panel.shown {
            return None;
        }
        self.sync_chip_handles(cx);
        let theme = cx.theme();
        let levels = self.view.group_by();
        let count = levels.len();
        let height = self.group_panel_height();
        let hint = (count == 0).then(|| {
            if self.tree().is_some() {
                label("live_grid.group_panel.tree")
            } else {
                label("live_grid.group_panel.hint")
            }
        });
        let mut chips: Vec<AnyElement> = Vec::with_capacity(count * 2);
        for (ix, level) in levels.iter().enumerate() {
            if ix > 0 {
                // The level on the right is nested in the one on the left.
                chips.push(
                    Icon::new(IconName::ChevronRight)
                        .size_3()
                        .text_color(theme.muted_foreground)
                        .into_any_element(),
                );
            }
            chips.push(
                self.render_chip(ix, level, count, window, cx)
                    .into_any_element(),
            );
        }
        let this = cx.weak_entity();
        let row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(CHIP_GAP)
            // Every other child is a chip: their bounds place the drop gaps
            // and the presses of the next events.
            .on_children_prepainted(move |bounds, _, cx| {
                let chips: Vec<Bounds<Pixels>> = bounds.into_iter().step_by(2).collect();
                let _ = this.update(cx, |state, _| state.group_panel.chip_bounds = chips);
            })
            .children(chips);
        let gap_line = self.panel_gap_line(cx);
        Some(
            div()
                .id(PANEL_ID)
                .test_support()
                .key_context(GROUP_PANEL_KEY_CONTEXT)
                .role(Role::Toolbar)
                .aria_label(label("live_grid.group_panel.label"))
                .when_some(hint.clone(), |this, hint| this.aria_description(hint))
                .relative()
                .flex_none()
                .w_full()
                .h(height)
                .overflow_hidden()
                .flex()
                .items_center()
                .pl(CELL_PADDING)
                .bg(theme.table_head)
                .text_color(theme.table_head_foreground)
                // The panel owns the line between it and the header (Design
                // Guides, "Hairlines belong on the boundary owner").
                .border_b_1()
                .border_color(theme.border)
                .on_any_mouse_down(cx.listener(Self::on_group_panel_mouse_down))
                .child(row)
                .when_some(hint, |this, hint| {
                    this.child(
                        div()
                            .id(HINT_ID)
                            .test_support()
                            .aria_label(hint.clone())
                            .min_w(px(0.))
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    )
                })
                .when_some(gap_line, |this, (x, top, line_height)| {
                    this.child(
                        div()
                            .id(GAP_ID)
                            .test_support()
                            .absolute()
                            .left(x - GAP_LINE / 2.)
                            .top(top)
                            .w(GAP_LINE)
                            .h(line_height)
                            .bg(theme.drag_border),
                    )
                }),
        )
    }

    /// The insertion line of a drag over the panel, from the panel's top
    /// left: `x`, `top`, height, as tall as the chips.
    fn panel_gap_line(&self, cx: &gpui_kit::App) -> Option<(Pixels, Pixels, Pixels)> {
        let Some(PanelTarget::Gap { x, .. }) = self.group_panel.target else {
            return None;
        };
        if !cx.has_active_drag() {
            return None;
        }
        let panel = self.group_panel_bounds()?;
        let inset = px(4.);
        let (top, height) = self
            .group_panel
            .chip_bounds
            .first()
            .map(|chip| (chip.top() - panel.top(), chip.size.height))
            .unwrap_or((inset, panel.size.height - inset * 2.));
        Some((x, top, height))
    }

    /// A chip: a `Tag` one step smaller than the grid with the level's
    /// title and direction, focusable and draggable, and its remove button.
    /// The chip's actions are on the wrapper, so they reach the level from
    /// the remove button too.
    fn render_chip(
        &self,
        ix: usize,
        level: &GroupBy<D::Row>,
        count: usize,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let id = level.id();
        let title = self.level_title(level);
        let icon = self.level_icon(level);
        let direction = level.direction();
        let size = self.group_panel.size.smaller();
        let handle = self.group_panel.chip_handle(&id).cloned();
        let ring = window.last_input_was_keyboard()
            && handle
                .as_ref()
                .is_some_and(|handle| handle.is_focused(window));
        let drag = DraggedGroupLevel {
            grid: cx.entity_id(),
            level: id.clone(),
            icon: icon.clone(),
            title: title.clone(),
            direction,
            size,
            text: GhostTextStyle::capture(window),
        };
        let this = cx.weak_entity();
        let chip = div()
            .id(chip_part_id(CHIP_ID, &id))
            .test_support()
            .when_some(handle, |this, handle| this.track_focus(&handle))
            .role(Role::Button)
            .aria_label(chip_label(&title, direction))
            .aria_position_in_set(ix + 1)
            .aria_size_of_set(count)
            .flex()
            .items_center()
            .gap_1()
            // A click leaves the focus where it is (on the grid).
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener({
                let id = id.clone();
                move |state, event: &ClickEvent, _, cx| state.on_chip_click(&id, event, cx)
            }))
            .on_drag(drag, move |drag: &DraggedGroupLevel, _, _, cx| {
                let _ = this.update(cx, |state, cx| state.begin_level_drag(cx));
                cx.new(|_| drag.clone())
            })
            .children(icon.map(level_icon))
            .child(title.clone())
            .child(Icon::new(direction_icon(direction)).size_3());
        let remove = Button::new(chip_part_id(CHIP_REMOVE_ID, &id))
            .ghost()
            .icon(IconName::Close)
            .with_size(remove_button_size(size))
            .accessibility_label(remove_label(&title))
            .tooltip(remove_label(&title))
            .on_click(cx.listener({
                let id = id.clone();
                move |state, event: &ClickEvent, window, cx| {
                    state.on_chip_remove(&id, event, window, cx)
                }
            }));
        div()
            .flex_none()
            .on_action(cx.listener({
                let id = id.clone();
                move |state, _: &UngroupColumn, window, cx| {
                    state.ungroup_level(&id, true, window, cx)
                }
            }))
            .on_action(cx.listener(move |state, _: &ShowContextMenu, window, cx| {
                state.show_chip_menu(&id, window, cx)
            }))
            .child(
                Tag::new()
                    .outline()
                    .with_size(size)
                    .py_0()
                    .gap_0p5()
                    .text_color(theme.table_head_foreground)
                    .when(ring, |this| this.focus_ring_style(window, cx))
                    .child(chip)
                    .child(remove),
            )
    }
}

/// A menu item that runs `command` on the grid.
fn command<D: LiveGridDelegate>(
    label: SharedString,
    this: &WeakEntity<LiveGridState<D>>,
    command: impl Fn(&mut LiveGridState<D>, &mut Context<LiveGridState<D>>) + 'static,
) -> PopupMenuItem {
    let this = this.clone();
    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        let _ = this.update(cx, |state, cx| command(state, cx));
    })
}

/// The panel's menu: for a chip (`ix`, level, direction), Sort ascending
/// and descending (the current one checked), Move left and right, Ungroup;
/// then Ungroup all and the expansion of every group, which need a
/// grouping. Grouping items run on the grid, `GroupOrigin::User`.
///
/// Expand all, Collapse all and Captions only (checked while the default
/// depth is `ExpandDepth::Captions`) dispatch `ExpandAll`, `CollapseAll` and
/// `ShowCaptionsOnly` to the grid, as the caption's menu does.
fn panel_menu_items<D: LiveGridDelegate>(
    menu: PopupMenu,
    this: &WeakEntity<LiveGridState<D>>,
    chip: Option<(usize, ColumnId, SortDirection)>,
    count: usize,
    captions_only: bool,
) -> PopupMenu {
    menu.when_some(chip, |menu, (ix, level, direction)| {
        let directed = |key: &str, to: SortDirection| {
            let level = level.clone();
            command(label(key), this, move |state, cx| {
                state.set_level_direction(&level, Some(to), cx)
            })
            .checked(direction == to)
        };
        let moved = |key: &str, step: isize| {
            let level = level.clone();
            command(label(key), this, move |state, cx| {
                state.move_level_by(&level, step, cx)
            })
        };
        let ungroup = {
            let level = level.clone();
            command(label("live_grid.menu.ungroup"), this, move |state, cx| {
                state.remove_level(&level, cx);
            })
        };
        menu.item(directed(
            "live_grid.menu.sort_ascending",
            SortDirection::Ascending,
        ))
        .item(directed(
            "live_grid.menu.sort_descending",
            SortDirection::Descending,
        ))
        .separator()
        .item(moved("live_grid.menu.move_left", -1).disabled(ix == 0))
        .item(moved("live_grid.menu.move_right", 1).disabled(ix + 1 >= count))
        .separator()
        .item(ungroup)
    })
    .menu_with_disabled(
        label("live_grid.menu.clear_grouping"),
        Box::new(ClearGrouping),
        count == 0,
    )
    .separator()
    .menu_with_disabled(
        label("live_grid.menu.expand_all"),
        Box::new(ExpandAll),
        count == 0,
    )
    .menu_with_disabled(
        label("live_grid.menu.collapse_all"),
        Box::new(CollapseAll),
        count == 0,
    )
    .menu_with_check_and_disabled(
        label("live_grid.menu.captions_only"),
        captions_only,
        Box::new(ShowCaptionsOnly),
        count == 0,
    )
}
