//! The context menu: its target, what the delegate knows when it builds the
//! menu, the standard items, and the menu's life under live data.
//!
//! One path for the pointer and the keyboard: the grid builds its own
//! `PopupMenu` in `window.defer` after the press (or the key) and shows it in
//! a `deferred(anchored(..))` layer of its view, not through `ContextMenuExt`.
//! So the grid knows while the menu is open, holds the row order meanwhile
//! (`ReorderContext::is_menu_open`), and closes the menu when live data
//! removes its target row. Items dispatch Actions to the grid's focus
//! (`action_context`), so they pass through the `LiveGrid` key context and
//! the grid's ancestors in the application.

use std::sync::Arc;

use gpui_kit::base::actions::Confirm;
use gpui_kit::base::{POPUP_PRIORITY, StyledExt as _, TestSupportExt as _, h_flex};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    AnyElement, App, Context, DismissEvent, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored, deferred, div,
    point, prelude::FluentBuilder as _, px, transparent_white,
};
use rust_i18n::t;

use super::{
    CELL_PADDING, ColumnOf, ColumnZone, KeyOf, LiveGridDelegate, LiveGridState, ScopedSelection,
    SortOrigin, columns::column_of, selection::LineTarget,
};
// The grouping items of the column menu.
use super::group_panel::{MenuGrouping, grouping_items};
use crate::{
    actions,
    engine::{ExpandDepth, GroupInfo, GroupPath, Projection, SortDirection},
    model::{GridColumns, GridRow},
    store::{PinnedEdge, StoreSnapshot},
};

/// What a press or the keyboard pointed at. Captured when the button goes
/// down, so a re-sort before the menu is built cannot retarget it.
///
/// A value type: its variants' fields are its definition.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GridTarget<K, C> {
    /// A data cell.
    Cell {
        /// The row.
        key: K,
        /// The column.
        column: C,
    },
    /// A cell of a pinned row. Its key is of its band's own key space: a
    /// data row or the other band may have the same key.
    Pinned {
        /// The band.
        edge: PinnedEdge,
        /// The pinned row.
        key: K,
        /// The column.
        column: C,
    },
    /// A column header cell, a band (its first visible column) or a resize
    /// handle.
    Header {
        /// The column.
        column: C,
    },
    /// The body or header outside rows and columns; the keyboard menu with
    /// no focused cell.
    Empty,
    /// A group caption.
    GroupCaption {
        /// The group.
        group: GroupPath,
        /// The aggregate cell under the pointer, if any; `None` from the
        /// keyboard.
        column: Option<C>,
    },
    /// A cell of a group footer.
    GroupFooter {
        /// The group the footer sums up.
        group: GroupPath,
        /// The column.
        column: C,
    },
    /// A cell of the totals row.
    Totals {
        /// The band the totals row is in.
        edge: PinnedEdge,
        /// The column.
        column: C,
    },
}

impl<K, C: Copy> GridTarget<K, C> {
    /// The data row key of a cell target. A pinned row's key is of its
    /// band's key space, so it is not returned here, where it could be
    /// taken for a data row's (match [`GridTarget::Pinned`]).
    pub fn key(&self) -> Option<&K> {
        match self {
            Self::Cell { key, .. } => Some(key),
            Self::Pinned { .. }
            | Self::Header { .. }
            | Self::Empty
            | Self::GroupCaption { .. }
            | Self::GroupFooter { .. }
            | Self::Totals { .. } => None,
        }
    }

    /// The column of a cell, pinned cell, header, footer or totals target,
    /// and of a caption's aggregate cell.
    pub fn column(&self) -> Option<C> {
        match self {
            Self::Cell { column, .. }
            | Self::Pinned { column, .. }
            | Self::Header { column }
            | Self::GroupFooter { column, .. }
            | Self::Totals { column, .. } => Some(*column),
            Self::GroupCaption { column, .. } => *column,
            Self::Empty => None,
        }
    }

    /// The group of a caption or footer target.
    pub fn group(&self) -> Option<&GroupPath> {
        match self {
            Self::GroupCaption { group, .. } | Self::GroupFooter { group, .. } => Some(group),
            Self::Cell { .. }
            | Self::Pinned { .. }
            | Self::Header { .. }
            | Self::Empty
            | Self::Totals { .. } => None,
        }
    }

    /// The line a command from this target acts on: a data row, or a
    /// caption's group. A footer and the totals take no command of their
    /// own: Copy over them copies the scoped selection.
    pub(crate) fn line(&self) -> Option<LineTarget<K>>
    where
        K: Clone,
    {
        match self {
            Self::Cell { key, .. } => Some(LineTarget::Row(key.clone())),
            Self::GroupCaption { group, .. } => Some(LineTarget::Group(group.clone())),
            Self::Pinned { .. }
            | Self::Header { .. }
            | Self::Empty
            | Self::GroupFooter { .. }
            | Self::Totals { .. } => None,
        }
    }
}

/// The state of the target column when the menu opened, for the standard
/// items' enabled and checked states.
#[derive(Clone, Copy, Debug)]
struct MenuColumn {
    sortable: bool,
    /// Its direction, if it is in the sort.
    direction: Option<SortDirection>,
    zone: ColumnZone,
    /// The only visible column: it cannot be hidden.
    last_visible: bool,
    /// The user may hide it.
    hideable: bool,
}

/// What [`LiveGridDelegate::context_menu`] and
/// [`LiveGridDelegate::primary_command`] know: the target, the command scope
/// and the rows of the snapshot shown when the menu opened.
///
/// [`LiveGridDelegate::context_menu`]: crate::LiveGridDelegate::context_menu
/// [`LiveGridDelegate::primary_command`]: crate::LiveGridDelegate::primary_command
pub struct GridMenuContext<'a, R: GridRow> {
    target: GridTarget<R::Key, R::Column>,
    selection: ScopedSelection<R::Key>,
    snapshot: &'a StoreSnapshot<R>,
    projection: &'a Projection,
    /// The expansion state of a caption target, by the view's expansion.
    expanded: Option<bool>,
    /// The view's default depth.
    expand_depth: ExpandDepth,
    column: Option<MenuColumn>,
    any_hidden: bool,
    /// Some column is in the sort: Clear all sorting is enabled.
    sorted: bool,
    /// The grouping when the menu opened, for `grouping_items`.
    pub(super) grouping: MenuGrouping,
    /// The grid's focus: items dispatch their Actions from it.
    focus_handle: FocusHandle,
}

impl<'a, R: GridRow> GridMenuContext<'a, R> {
    /// Where the menu was opened.
    pub fn target(&self) -> &GridTarget<R::Key, R::Column> {
        &self.target
    }

    /// The command scope of the target when the menu opened, in display
    /// order: a caption's group rows, else `scoped_selection()`, as the
    /// selection policy decides. For labels and enabled states only: a
    /// command handler reads the scope again when it runs.
    pub fn selection(&self) -> &ScopedSelection<R::Key> {
        &self.selection
    }

    /// The group of a caption or footer target, in the projection shown
    /// when the menu opened.
    pub fn group(&self) -> Option<GroupInfo<'a>> {
        self.projection.groups().find(self.target.group()?)
    }

    /// A data row of the snapshot shown when the menu opened.
    pub fn row(&self, key: &R::Key) -> Option<&'a Arc<R>> {
        self.snapshot.get(key)
    }

    /// The row the target is in, as the snapshot shown when the menu opened
    /// has it: the data row of a cell target, the pinned row of a pinned
    /// target; `None` for a header, the empty area and the lines the grid
    /// makes (captions, footers, totals).
    pub fn target_row(&self) -> Option<&'a Arc<R>> {
        match &self.target {
            GridTarget::Cell { key, .. } => self.snapshot.get(key),
            GridTarget::Pinned { edge, key, .. } => self
                .snapshot
                .pinned_rows(*edge)
                .iter()
                .find(|row| row.key() == key),
            GridTarget::Header { .. }
            | GridTarget::Empty
            | GridTarget::GroupCaption { .. }
            | GridTarget::GroupFooter { .. }
            | GridTarget::Totals { .. } => None,
        }
    }

    /// The built-in items for the target, in the standard order:
    ///
    /// - a cell: Copy, Copy all; the Sort and Column submenus for its
    ///   column, so the keyboard reaches the header's commands (Shift+F10
    ///   on the focused cell);
    /// - a header: the sort items, the column items, Copy, Copy all;
    /// - a group caption: the group items, Copy (the group's rows), Copy
    ///   all;
    /// - a pinned row, a group footer, the totals row and elsewhere: Copy,
    ///   Copy all (Copy copies the selection of data rows; Copy all takes
    ///   the bands and the totals too).
    ///
    /// The grid puts the primary command above them.
    pub fn standard_items(
        &self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        match &self.target {
            GridTarget::Cell { .. } => {
                // The keyboard path to sorting and to the column commands:
                // Shift+F10 on a cell, then Sort or Column. Submenus do not
                // inherit the action context: each sets its own.
                let (column, any_hidden, sorted) = (self.column, self.any_hidden, self.sorted);
                let grouping = self.grouping;
                let sort = PopupMenu::build(window, cx, {
                    let focus = self.focus_handle.clone();
                    move |menu, _, _| sort_items(column, sorted, menu.action_context(focus))
                });
                let columns = PopupMenu::build(window, cx, {
                    let focus = self.focus_handle.clone();
                    move |menu, _, _| {
                        column_items(column, any_hidden, grouping, menu.action_context(focus))
                    }
                });
                // Clear all sorting is reachable from a column that is
                // not sortable too.
                let sortable = column.is_some_and(|column| column.sortable);
                // A tree row with children expands, as a caption.
                let menu = menu.when(self.expanded.is_some(), |menu| {
                    self.group_items(menu).separator()
                });
                self.copy_items(menu)
                    .separator()
                    .item(
                        PopupMenuItem::submenu(label("live_grid.menu.sort"), sort)
                            .disabled(!sortable && !sorted),
                    )
                    .item(PopupMenuItem::submenu(
                        label("live_grid.menu.column"),
                        columns,
                    ))
            }
            GridTarget::Header { .. } => {
                let menu = self.sort_items(menu).separator();
                self.copy_items(self.column_items(menu).separator())
            }
            GridTarget::GroupCaption { .. } => self.copy_items(self.group_items(menu).separator()),
            GridTarget::Pinned { .. }
            | GridTarget::GroupFooter { .. }
            | GridTarget::Totals { .. }
            | GridTarget::Empty => self.copy_items(menu),
        }
    }

    /// Expand or Collapse for a caption target (by its state), then Expand
    /// all, Collapse all and Captions only (checked while the default depth
    /// is `ExpandDepth::Captions`). They dispatch `ExpandRow`,
    /// `CollapseRow`, `ExpandAll`, `CollapseAll` and `ShowCaptionsOnly` to
    /// the grid, whose focus is on the caption a press opened the menu for.
    pub fn group_items(&self, menu: PopupMenu) -> PopupMenu {
        menu.when_some(self.expanded, |menu, expanded| {
            if expanded {
                menu.menu(
                    label("live_grid.menu.collapse"),
                    Box::new(actions::CollapseRow),
                )
            } else {
                menu.menu(label("live_grid.menu.expand"), Box::new(actions::ExpandRow))
            }
        })
        .menu(
            label("live_grid.menu.expand_all"),
            Box::new(actions::ExpandAll),
        )
        .menu(
            label("live_grid.menu.collapse_all"),
            Box::new(actions::CollapseAll),
        )
        // A tree has no captions.
        .when(!self.projection.is_tree(), |menu| {
            menu.menu_with_check(
                label("live_grid.menu.captions_only"),
                self.expand_depth == ExpandDepth::Captions,
                Box::new(actions::ShowCaptionsOnly),
            )
        })
    }

    /// Copy (`Copy`, disabled when the command scope is empty) and Copy all
    /// (`CopyAll`).
    pub fn copy_items(&self, menu: PopupMenu) -> PopupMenu {
        menu.menu_with_disabled(
            label("live_grid.menu.copy"),
            Box::new(actions::Copy),
            self.selection.is_empty(),
        )
        .menu(label("live_grid.menu.copy_all"), Box::new(actions::CopyAll))
    }

    /// Sort ascending, Sort descending and Clear sort for the target column,
    /// then Clear all sorting (`ClearAllSorting`); nothing without a target
    /// column. The first three are disabled for a column that is not
    /// sortable; the current direction is checked; Clear sort is disabled
    /// for a column outside the sort, Clear all sorting when nothing is
    /// sorted.
    pub fn sort_items(&self, menu: PopupMenu) -> PopupMenu {
        sort_items(self.column, self.sorted, menu)
    }

    /// Autofit, hide, show all, choose (`ShowColumnChooser`), freeze,
    /// unfreeze and reset columns for the target column, then the grouping items ([`Self::grouping_items`]).
    /// Without a target column, only the items for every column. "Hide
    /// column" is disabled for the last visible column and for a column the
    /// user may not hide (`GridColumn::with_hideable`).
    pub fn column_items(&self, menu: PopupMenu) -> PopupMenu {
        column_items(self.column, self.any_hidden, self.grouping, menu)
    }
}

/// A menu label from this crate's catalog.
fn label(key: &str) -> SharedString {
    SharedString::from(t!(key).into_owned())
}

/// [`GridMenuContext::column_items`]; `any_hidden` enables Show all columns.
fn column_items(
    column: Option<MenuColumn>,
    any_hidden: bool,
    grouping: MenuGrouping,
    menu: PopupMenu,
) -> PopupMenu {
    menu.when(column.is_some(), |menu| {
        menu.menu(
            label("live_grid.menu.autofit_column"),
            Box::new(actions::AutofitColumn),
        )
    })
    .menu(
        label("live_grid.menu.autofit_columns"),
        Box::new(actions::AutofitColumns),
    )
    .separator()
    .when_some(column, |menu, column| {
        menu.menu_with_disabled(
            label("live_grid.menu.hide_column"),
            Box::new(actions::HideColumn),
            column.last_visible || !column.hideable,
        )
    })
    .menu_with_disabled(
        label("live_grid.menu.show_all_columns"),
        Box::new(actions::ShowAllColumns),
        !any_hidden,
    )
    // Every column in one place.
    .menu(
        label("live_grid.menu.choose_columns"),
        Box::new(actions::ShowColumnChooser),
    )
    .separator()
    // Only the moves that change the column's zone.
    .when_some(column, |menu, column| {
        menu.when(column.zone != ColumnZone::FrozenLeft, |menu| {
            menu.menu(
                label("live_grid.menu.freeze_left"),
                Box::new(actions::FreezeColumnLeft),
            )
        })
        .when(column.zone != ColumnZone::FrozenRight, |menu| {
            menu.menu(
                label("live_grid.menu.freeze_right"),
                Box::new(actions::FreezeColumnRight),
            )
        })
        .when(column.zone != ColumnZone::Scrolling, |menu| {
            menu.menu(
                label("live_grid.menu.unfreeze"),
                Box::new(actions::UnfreezeColumn),
            )
        })
        .separator()
    })
    .menu(
        label("live_grid.menu.reset_columns"),
        Box::new(actions::ResetLayout),
    )
    // The grouping commands, after a separator.
    .map(|menu| grouping_items(grouping, menu.separator()))
}

/// [`GridMenuContext::sort_items`]; `sorted` enables Clear all sorting.
fn sort_items(column: Option<MenuColumn>, sorted: bool, menu: PopupMenu) -> PopupMenu {
    let Some(column) = column else {
        return menu;
    };
    let disabled = !column.sortable;
    menu.menu_with_check_and_disabled(
        label("live_grid.menu.sort_ascending"),
        column.direction == Some(SortDirection::Ascending),
        Box::new(actions::SortAscending),
        disabled,
    )
    .menu_with_check_and_disabled(
        label("live_grid.menu.sort_descending"),
        column.direction == Some(SortDirection::Descending),
        Box::new(actions::SortDescending),
        disabled,
    )
    .menu_with_disabled(
        label("live_grid.menu.clear_sort"),
        Box::new(actions::ClearSort),
        disabled || column.direction.is_none(),
    )
    .menu_with_disabled(
        label("live_grid.menu.clear_all_sorting"),
        Box::new(actions::ClearAllSorting),
        !sorted,
    )
}

/// The element id of the primary command's label.
const PRIMARY_LABEL_ID: &str = "live-grid-primary-command";

/// The primary command: its label in bold, with Enter's key from the
/// keymap (Design Guides: the emphasized command is the one Enter runs).
///
/// An element item has no accessible name of its own. AccessKit names a
/// menu item after the values of its `Label` descendants, so the label is
/// one: a screen reader reads the command's name.
fn primary_item(
    label: SharedString,
    focus: FocusHandle,
) -> impl Fn(&mut Window, &mut App) -> AnyElement + 'static {
    move |window, _| {
        let key = Kbd::binding_for_action_in(&Confirm { secondary: false }, &focus, window);
        h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .justify_between()
            .child(
                div()
                    .id(PRIMARY_LABEL_ID)
                    .test_support()
                    .role(Role::Label)
                    .aria_value(label.clone())
                    .font_semibold()
                    .child(label.clone()),
            )
            .children(key.map(|key| key.p_0().flex_nowrap().border_0().bg(transparent_white())))
            .into_any_element()
    }
}

/// The open context menu.
pub(super) struct OpenMenu<K, C> {
    entity: Entity<PopupMenu>,
    target: GridTarget<K, C>,
    /// Where it opened, in window coordinates.
    position: Point<Pixels>,
    _dismiss: Subscription,
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Returns true while the context menu is open.
    pub fn is_menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// Where the open context menu is, in window coordinates ("Choose
    /// columns…" opens the chooser there).
    pub(super) fn menu_position(&self) -> Option<Point<Pixels>> {
        self.menu.as_ref().map(|menu| menu.position)
    }

    /// Opens the context menu for the focused cell, below it, scrolled into
    /// view (`ShowContextMenu`); with no focused cell, at the top left of the
    /// rows, for the whole grid. The menu is built after the current event.
    pub fn show_context_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal_focus();
        let target = self
            .selection
            .focus()
            .and_then(|cell| {
                if let Some(group) = cell.group() {
                    return Some(GridTarget::GroupCaption {
                        group: group.clone(),
                        column: None,
                    });
                }
                let key = cell.row()?;
                self.snapshot.get(key)?;
                Some(GridTarget::Cell {
                    key: key.clone(),
                    column: cell.column(),
                })
            })
            .unwrap_or(GridTarget::Empty);
        let position = self
            .cell_menu_position(&target)
            .unwrap_or_else(|| self.rows_bounds().origin + point(CELL_PADDING, CELL_PADDING));
        self.open_menu(target, position, window, cx);
        cx.notify();
    }

    /// The bottom left corner of a cell target on screen, or of a caption
    /// target's text, within the data rows' area.
    fn cell_menu_position(
        &self,
        target: &GridTarget<KeyOf<D>, ColumnOf<D>>,
    ) -> Option<Point<Pixels>> {
        if let GridTarget::GroupCaption { group, .. } = target {
            let found = self.projection.groups().find(group)?;
            let display_ix = self.projection.group_line(found.id())?;
            let line = display_ix.checked_sub(self.scroll_row)?;
            let body = self.rows_bounds();
            let bottom = body.origin.y + self.row_height * (line + 1) as f32 - self.scroll_offset;
            return Some(point(
                self.caption_lane_x(group.depth()),
                bottom.clamp(body.origin.y, body.bottom()),
            ));
        }
        let GridTarget::Cell { key, column } = target else {
            return None;
        };
        let display_ix = self
            .snapshot
            .row_id(key)
            .and_then(|id| self.projection.display_ix(id))?;
        let line = display_ix.checked_sub(self.scroll_row)?;
        let body = self.rows_bounds();
        let bottom = body.origin.y + self.row_height * (line + 1) as f32 - self.scroll_offset;
        let geometry = self.columns.geometry(body.size.width, self.scroll_x);
        let (zone, i) = self.columns.zone_position(column.ix())?;
        let frame = geometry.zone(zone);
        let x = frame
            .column_x(self.columns.zone_columns(zone), i)
            .clamp(frame.x, (frame.x + frame.width).max(frame.x));
        Some(point(
            body.origin.x + x,
            bottom.clamp(body.origin.y, body.bottom()),
        ))
    }

    /// The menu of a context press on the body, at the target captured when
    /// the button went down (`pointer_target()`). A row removed since the
    /// frame was painted opens no menu: it leaves the screen next frame.
    pub(super) fn open_body_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pointer) = self.pointer_target() else {
            return;
        };
        let position = pointer.position();
        if let Some(target) = self.pressed_target() {
            self.open_menu(target, position, window, cx);
        }
    }

    /// The target of the last press on the body (`pointer_target()`), for
    /// its menu and its click; `None` with no press, or for a row removed
    /// since the frame was painted. Called while the button goes down: a
    /// press on the totals row or a group footer is resolved against the
    /// frame on screen then.
    pub(super) fn pressed_target(&self) -> Option<GridTarget<KeyOf<D>, ColumnOf<D>>> {
        let pointer = self.pointer_target()?;
        if let Some(group) = pointer.group() {
            return Some(GridTarget::GroupCaption {
                group: group.clone(),
                column: pointer.column().map(column_of),
            });
        }
        if pointer.row().is_none()
            && let Some(target) = self.summary_target(pointer.position())
        {
            return Some(target);
        }
        Some(match (pointer.pinned(), pointer.row(), pointer.column()) {
            (Some(edge), Some(key), Some(column)) => GridTarget::Pinned {
                edge,
                key: key.clone(),
                column: column_of(column),
            },
            (None, Some(key), Some(column)) => GridTarget::Cell {
                key: key.clone(),
                column: column_of(column),
            },
            (_, Some(_), None) => GridTarget::Empty,
            (_, None, _) if self.is_removed_row_at(pointer.position()) => return None,
            (_, None, _) => GridTarget::Empty,
        })
    }

    /// Returns true if the frame painted a row at `position` that the store
    /// no longer has.
    fn is_removed_row_at(&self, position: Point<Pixels>) -> bool {
        self.line_at(position)
            .and_then(|line| self.painted.get(line)?.row())
            .is_some_and(|id| self.snapshot.row(id).is_none())
    }

    /// Opens the menu for `target` at `position` (window coordinates) after
    /// the current event, when the press has updated the selection. A
    /// target with a column makes it the current column, so the column
    /// Actions of the items act on it.
    pub(super) fn open_menu(
        &mut self,
        target: GridTarget<KeyOf<D>, ColumnOf<D>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(column) = target.column() {
            self.set_current_column(column.ix());
        }
        let this = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let _ = this.update(cx, |state, cx| {
                state.build_menu(target, position, window, cx)
            });
        });
    }

    /// Builds and shows the menu: the primary command first, then what the
    /// delegate adds. The hooks run here, inside the grid's update: they
    /// must not read the grid entity. An empty menu is not shown.
    fn build_menu(
        &mut self,
        target: GridTarget<KeyOf<D>, ColumnOf<D>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A newer menu replaces an open one, and the column chooser (one
        // overlay at a time).
        let replaced = self.menu.take().is_some();
        self.drop_column_chooser();
        let removed = self.is_target_gone(&target);
        if !removed {
            self.show_menu(target, position, window, cx);
        }
        if replaced && self.menu.is_none() {
            self.reconsider_held(cx);
        }
        cx.notify();
    }

    fn show_menu(
        &mut self,
        target: GridTarget<KeyOf<D>, ColumnOf<D>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The scope of the target's line: a caption's group rows, a cell's
        // as the selection policy says, else the scoped selection.
        let selection = self.command_scope(target.line().as_ref());
        // A caption's or a tree row's state.
        let expanded = self.target_expansion(&target);
        let column = target.column().map(|column| {
            let ix = column.ix();
            MenuColumn {
                sortable: self.columns.column(ix).sortable,
                direction: self
                    .view
                    .sort()
                    .iter()
                    .find(|(sorted, _)| *sorted == column)
                    .map(|(_, direction)| *direction),
                zone: self.columns.zone_of(ix),
                last_visible: self.columns.is_visible(ix) && self.columns.visible_set().len() <= 1,
                hideable: self.columns.column(ix).hideable,
            }
        });
        // Internal columns are never shown, so they count as none.
        let any_hidden = self.columns.any_hidden();
        let sorted = !self.view.sort().is_empty();
        let grouping = self.menu_grouping(target.column());
        let snapshot = self.snapshot.clone();
        let projection = self.projection.clone();
        let focus = self.focus_handle.clone();
        let context = GridMenuContext {
            target,
            selection,
            snapshot: &snapshot,
            projection: &projection,
            expanded,
            expand_depth: self.expand_depth(),
            column,
            any_hidden,
            sorted,
            grouping,
            focus_handle: focus.clone(),
        };
        let primary = match &context.target {
            GridTarget::Cell { .. } | GridTarget::GroupCaption { .. }
                if !context.selection.is_empty() =>
            {
                self.delegate.primary_command(&context, cx)
            }
            GridTarget::Cell { .. }
            | GridTarget::GroupCaption { .. }
            | GridTarget::Pinned { .. }
            | GridTarget::Header { .. }
            | GridTarget::GroupFooter { .. }
            | GridTarget::Totals { .. }
            | GridTarget::Empty => None,
        };
        let delegate = &self.delegate;
        let menu = PopupMenu::build(window, cx, |menu, window, cx| {
            let menu = menu
                .action_context(focus.clone())
                .when_some(primary, |menu, label| {
                    menu.menu_element(
                        Box::new(Confirm { secondary: false }),
                        primary_item(label, focus.clone()),
                    )
                    .separator()
                });
            delegate.context_menu(&context, menu, window, cx)
        });
        if menu.read(cx).is_empty() {
            return;
        }
        self.install_menu(menu, context.target, position, window, cx);
    }

    /// Shows a built menu as the open menu: focuses it and closes it on its
    /// dismissal. The group panel opens its own menu here too, with the
    /// `Empty` target, so `is_menu_open()` holds the order while it is open.
    pub(super) fn install_menu(
        &mut self,
        menu: Entity<PopupMenu>,
        target: GridTarget<KeyOf<D>, ColumnOf<D>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu_focus = menu.read(cx).focus_handle(cx);
        window.focus(&menu_focus, cx);
        let dismiss = cx.subscribe_in(&menu, window, Self::on_menu_dismiss);
        self.menu = Some(OpenMenu {
            entity: menu,
            target,
            position,
            _dismiss: dismiss,
        });
    }

    /// Escape, an item or a press outside closed the menu (`PopupMenu`
    /// gives the focus back to the grid): the reorder policy is asked again,
    /// as when the pointer leaves.
    fn on_menu_dismiss(
        &mut self,
        menu: &Entity<PopupMenu>,
        _: &DismissEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.as_ref().is_some_and(|open| open.entity == *menu) {
            self.menu = None;
            self.reconsider_held(cx);
            cx.notify();
        }
    }

    /// Returns true if the row of a target left the store: a data row its
    /// rows, a pinned row its band; if a caption's group left the shown
    /// projection; or if a footer or the totals row is no longer shown, or
    /// its column is hidden.
    pub(super) fn is_target_gone(&self, target: &GridTarget<KeyOf<D>, ColumnOf<D>>) -> bool {
        match target {
            GridTarget::Cell { key, .. } => self.snapshot.get(key).is_none(),
            GridTarget::Pinned { edge, key, .. } => !self.is_pinned_key(*edge, key),
            GridTarget::GroupCaption { group, .. } => {
                self.projection.groups().find(group).is_none()
            }
            GridTarget::GroupFooter { group, column } => {
                !self.columns.is_visible(column.ix()) || !self.is_footer_shown(group)
            }
            GridTarget::Totals { edge, column } => {
                !self.columns.is_visible(column.ix()) || self.totals() != Some(*edge)
            }
            GridTarget::Header { .. } | GridTarget::Empty => false,
        }
    }

    /// Closes the menu whose target row left the store, or left its band,
    /// the menu of a caption whose group left the shown projection, and the
    /// menu of a footer or the totals that are no longer shown: an overlay
    /// action must refer to an object the overlay shows (Design Guides).
    /// Called in `render`, which has the window to give the focus back. A
    /// row hidden by a filter is still in the store: its menu stays.
    pub(super) fn close_stale_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stale = self
            .menu
            .as_ref()
            .is_some_and(|open| self.is_target_gone(&open.target));
        if !stale {
            return;
        }
        if let Some(open) = self.menu.take()
            && open
                .entity
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        {
            window.focus(&self.focus_handle, cx);
        }
        // Not while rendering: a held order may be shown now.
        cx.defer_in(window, |state, _, cx| state.reconsider_held(cx));
    }

    /// The layer of the open menu: over the whole window, so a wheel over
    /// the rows does not scroll the target away, with the menu at its
    /// position, kept 8 px inside the window.
    pub(super) fn render_menu(&self, window: &Window) -> Option<impl IntoElement + use<D>> {
        let open = self.menu.as_ref()?;
        let viewport = window.viewport_size();
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .w(viewport.width)
                        .h(viewport.height)
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(
                            anchored()
                                .position(open.position)
                                .snap_to_window_with_margin(px(8.))
                                .child(open.entity.clone()),
                        ),
                ),
            )
            .with_priority(POPUP_PRIORITY),
        )
    }

    /// `ShowContextMenu`.
    pub(super) fn on_show_context_menu(
        &mut self,
        _: &actions::ShowContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_context_menu(window, cx);
    }

    /// `SortAscending`: the current column alone, ascending.
    pub(super) fn on_sort_ascending(
        &mut self,
        _: &actions::SortAscending,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sort_current_column(Some(SortDirection::Ascending), cx);
    }

    /// `SortDescending`: the current column alone, descending.
    pub(super) fn on_sort_descending(
        &mut self,
        _: &actions::SortDescending,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sort_current_column(Some(SortDirection::Descending), cx);
    }

    /// `ClearSort`: the current column leaves the sort; the others stay.
    pub(super) fn on_clear_sort(
        &mut self,
        _: &actions::ClearSort,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sort_current_column(None, cx);
    }

    /// `ClearAllSorting`: no column stays in the sort.
    pub(super) fn on_clear_all_sorting(
        &mut self,
        _: &actions::ClearAllSorting,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_sort([], SortOrigin::User, cx);
    }

    /// The sort commands of the menu: a user action, like a header click,
    /// so a column that is not sortable is left alone.
    fn sort_current_column(&mut self, direction: Option<SortDirection>, cx: &mut Context<Self>) {
        let Some(column) = self.current_column() else {
            return;
        };
        if !self.columns.column(column.ix()).sortable {
            return;
        }
        let sort: Vec<(ColumnOf<D>, SortDirection)> = match direction {
            Some(direction) => vec![(column, direction)],
            None => self
                .view
                .sort()
                .iter()
                .copied()
                .filter(|(sorted, _)| *sorted != column)
                .collect(),
        };
        self.apply_sort(sort, SortOrigin::User, cx);
    }
}
