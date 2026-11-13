//! A grid without its delegate type: what a toolbar, a status bar or a
//! column chooser needs to read and command any grid by column id
//! ([`LiveGridHandle`]), and what its events say without keys and columns
//! ([`GridHandleEvent`]).
//!
//! The handle holds the typed entity behind an object-safe trait
//! ([`GridOps`]): each method is one `read` or one `update` of the grid.
//! Like the entity, it must not be used from inside an update of the same
//! grid, a delegate hook included: the re-entrant borrow panics. Commands
//! are `Programmatic`, as the typed API's are.

use std::{fmt, hash::Hash, rc::Rc};

use gpui_kit::{
    App, Context, Entity, EntityId, FocusHandle, Focusable as _, Pixels, SharedString,
    Subscription, WeakEntity, Window,
};

#[cfg(not(target_family = "wasm"))]
use super::export::{ExportJob, ExportRequest};
use super::{
    ColumnOf, LiveGridDelegate, LiveGridState,
    chooser::ChooserCommand,
    columns::{ColumnZone, HeaderLabel},
    events::{GroupOrigin, LayoutOrigin, LayoutParts, LiveGridEvent, SelectionOrigin, SortOrigin},
};
#[cfg(not(target_family = "wasm"))]
use crate::export::{CsvFormat, ExportError};
use crate::{
    engine::{ExpandDepth, GroupBy, SortDirection},
    export::ExportScope,
    model::{ColumnId, ColumnIx, GridColumns as _},
    persist::{GridLayout, LayoutError, LayoutGroup, LayoutReport, LayoutSort},
    policy::ReorderPolicy,
};

/// A grid without its delegate type: columns, layout, sort, grouping, quick
/// filter, counts, the held order, copy and export, and its events, all by
/// column id. What a toolbar, a status bar or a column chooser needs.
///
/// Clones refer to the same grid and keep it alive, as an `Entity` does;
/// [`downgrade`](Self::downgrade) for a back reference. Each method is one
/// read or one update of the grid: never call it from inside an update of
/// the same grid, such as a delegate hook (the borrow panics). Edits are
/// not here: they need row keys, the application's own type.
///
/// ```
/// use gpui_kit::{App, IntoElement, ParentElement as _, div};
/// use gpui_kit_grid::LiveGridHandle;
///
/// /// "120 of 4000 rows", whatever the grid's rows are.
/// fn row_counts(grid: &LiveGridHandle, cx: &App) -> impl IntoElement {
///     div().child(format!(
///         "{} of {} rows",
///         grid.row_count(cx),
///         grid.total_row_count(cx)
///     ))
/// }
/// ```
#[derive(Clone)]
pub struct LiveGridHandle {
    ops: Rc<dyn GridOps>,
}

impl LiveGridHandle {
    /// A handle of `grid`.
    pub fn new<D: LiveGridDelegate>(grid: &Entity<LiveGridState<D>>) -> Self {
        Self {
            ops: Rc::new(Ops { grid: grid.clone() }),
        }
    }

    /// The grid entity's id.
    pub fn entity_id(&self) -> EntityId {
        self.ops.entity_id()
    }

    /// A handle that does not keep the grid alive.
    pub fn downgrade(&self) -> WeakLiveGridHandle {
        self.ops.downgrade()
    }

    /// The grid's focus handle: focus it to give the grid the keyboard.
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.ops.focus_handle(cx)
    }

    // Columns, by `ColumnId` text.

    /// Every schema column in display order (left, scrolling, right zone),
    /// hidden ones at their place. O(columns).
    pub fn columns(&self, cx: &App) -> Vec<ColumnInfo> {
        self.ops.columns(cx)
    }

    /// The schema column `id`; `None` if the schema has none.
    pub fn column(&self, id: &str, cx: &App) -> Option<ColumnInfo> {
        self.ops.column(id, cx)
    }

    /// Shows or hides the column `id`, as
    /// [`LiveGridState::set_column_visible`] does. Returns false, changing
    /// nothing, if the column is unknown or is the last visible one.
    pub fn set_column_visible(&self, id: &str, visible: bool, cx: &mut App) -> bool {
        self.ops.set_column_visible(id, visible, cx)
    }

    /// Shows every column, with one `LayoutChange`.
    pub fn show_all_columns(&self, cx: &mut App) {
        self.ops.show_all_columns(cx);
    }

    /// Moves the column `id` before the column `before` (`None`: to the end
    /// of its zone), as [`LiveGridState::move_column`] does. Returns false,
    /// changing nothing, if a column is unknown, the zones differ or the
    /// move policy refuses.
    pub fn move_column(&self, id: &str, before: Option<&str>, cx: &mut App) -> bool {
        self.ops.move_column(id, before, cx)
    }

    /// Moves every column of `band` before the column `before` (`None`: to
    /// the end of the zone), as [`LiveGridState::move_band`] does. Returns
    /// false, changing nothing, if the band or the column is unknown, the
    /// zones differ or the move policy refuses.
    pub fn move_band(&self, band: &str, before: Option<&str>, cx: &mut App) -> bool {
        self.ops.move_band(band, before, cx)
    }

    /// Moves the column `id` to `zone`, as
    /// [`LiveGridState::set_column_zone`] does. Returns false if the column
    /// is unknown.
    pub fn set_column_zone(&self, id: &str, zone: ColumnZone, cx: &mut App) -> bool {
        self.ops.set_column_zone(id, zone, cx)
    }

    /// Sets the width of the column `id`, within its limits. Returns false
    /// if the column is unknown.
    pub fn set_column_width(&self, id: &str, width: Pixels, cx: &mut App) -> bool {
        self.ops.set_column_width(id, width, cx)
    }

    /// Fits every visible column to its header and painted rows, as
    /// [`LiveGridState::autofit_columns`] does.
    pub fn autofit_columns(&self, cx: &mut App) {
        self.ops.autofit_columns(cx);
    }

    // Layout, sort, grouping, quick filter.

    /// The view id of the grid's saved layouts, if it has one.
    pub fn view_id(&self, cx: &App) -> Option<SharedString> {
        self.ops.view_id(cx)
    }

    /// The current layout ([`LiveGridState::layout`]).
    pub fn layout(&self, cx: &App) -> GridLayout {
        self.ops.layout(cx)
    }

    /// Restores a saved layout ([`LiveGridState::set_layout`]).
    pub fn set_layout(
        &self,
        layout: &GridLayout,
        cx: &mut App,
    ) -> Result<LayoutReport, LayoutError> {
        self.ops.set_layout(layout, cx)
    }

    /// Restores the default layout ([`LiveGridState::reset_layout`]).
    pub fn reset_layout(&self, cx: &mut App) {
        self.ops.reset_layout(cx);
    }

    /// The sort by column id, most significant first; empty when unsorted.
    pub fn sort(&self, cx: &App) -> Vec<LayoutSort> {
        self.ops.sort(cx)
    }

    /// Replaces the sort, as [`LiveGridState::set_sort`] does. Returns
    /// false, changing nothing, if a column is unknown.
    pub fn set_sort(&self, sort: impl IntoIterator<Item = LayoutSort>, cx: &mut App) -> bool {
        self.ops.set_sort(&sort.into_iter().collect::<Vec<_>>(), cx)
    }

    /// The grouping levels by id, the top level first: a column level by its
    /// column's id, a `GroupKey` level by its own.
    pub fn group_by(&self, cx: &App) -> Vec<LayoutGroup> {
        self.ops.group_by(cx)
    }

    /// Replaces the grouping by column levels, as
    /// [`LiveGridState::set_group_by`] does; no levels turn grouping off.
    /// Returns false, changing nothing, if a column is unknown: a `GroupKey`
    /// level needs the typed state.
    pub fn set_group_by(
        &self,
        levels: impl IntoIterator<Item = LayoutGroup>,
        cx: &mut App,
    ) -> bool {
        self.ops
            .set_group_by(&levels.into_iter().collect::<Vec<_>>(), cx)
    }

    /// How deep groups and tree rows are expanded by default.
    pub fn expand_depth(&self, cx: &App) -> ExpandDepth {
        self.ops.expand_depth(cx)
    }

    /// Sets the default depth and clears every toggle
    /// ([`LiveGridState::set_expand_depth`]).
    pub fn set_expand_depth(&self, depth: ExpandDepth, cx: &mut App) {
        self.ops.set_expand_depth(depth, cx);
    }

    /// The quick filter text; empty when the filter is off.
    pub fn quick_filter(&self, cx: &App) -> SharedString {
        self.ops.quick_filter(cx)
    }

    /// Sets the quick filter text ([`LiveGridState::set_quick_filter`]).
    pub fn set_quick_filter(&self, text: impl Into<SharedString>, cx: &mut App) {
        self.ops.set_quick_filter(text.into(), cx);
    }

    // Counts and the order: O(1).

    /// The data rows of the shown order (`Projection::row_count`): rows of
    /// collapsed groups count, pinned rows do not.
    pub fn row_count(&self, cx: &App) -> usize {
        self.ops.row_count(cx)
    }

    /// The live rows of the shown snapshot: the "4000" of "120 of 4000".
    pub fn total_row_count(&self, cx: &App) -> usize {
        self.ops.total_row_count(cx)
    }

    /// The selected rows, those a filter hides included.
    pub fn selected_count(&self, cx: &App) -> usize {
        self.ops.selected_count(cx)
    }

    /// Returns true if a newer order is ready but held by the reorder
    /// policy.
    pub fn is_order_held(&self, cx: &App) -> bool {
        self.ops.is_order_held(cx)
    }

    /// Shows the held order now.
    pub fn apply_held_order(&self, cx: &mut App) {
        self.ops.apply_held_order(cx);
    }

    /// Replaces the reorder policy, as [`LiveGridState::set_reorder_policy`]
    /// does: a held order is offered to the new policy at once. A toolbar's
    /// Live / Hold / Manual switch, without the delegate type.
    pub fn set_reorder_policy(&self, policy: impl ReorderPolicy, cx: &mut App) {
        self.ops.set_reorder_policy(Box::new(policy), cx);
    }

    // Commands.

    /// Copies `scope` to the clipboard ([`LiveGridState::copy_scope`]).
    /// Returns false, leaving the clipboard alone, if there is nothing to
    /// copy.
    pub fn copy_scope(&self, scope: ExportScope, cx: &mut App) -> bool {
        self.ops.copy_scope(scope, cx)
    }

    /// Returns true if an export of `scope` would have nothing to write
    /// ([`LiveGridState::is_export_empty`]).
    pub fn is_export_empty(&self, scope: ExportScope, cx: &App) -> bool {
        self.ops.is_export_empty(scope, cx)
    }

    /// Exports `scope` as CSV to a file at `path`, with a header line and
    /// the grid's cell export ([`LiveGridState::export_to_path`]). Other
    /// formats and requests go through the typed state. Not on the web.
    #[cfg(not(target_family = "wasm"))]
    pub fn export_to_path(
        &self,
        scope: ExportScope,
        format: CsvFormat,
        path: impl Into<std::path::PathBuf>,
        cx: &mut App,
    ) -> Result<ExportJob<std::path::PathBuf>, ExportError> {
        self.ops.export_to_path(scope, format, path.into(), cx)
    }

    // Observing.

    /// Calls `on_event` with what each event of the grid says without keys
    /// and columns ([`GridHandleEvent`]), until the subscription is
    /// dropped. Events with keys (`Click`, `Confirm`, `FocusChange`, edits)
    /// need the typed subscription.
    pub fn subscribe(
        &self,
        cx: &mut App,
        on_event: impl FnMut(&GridHandleEvent, &mut App) + 'static,
    ) -> Subscription {
        self.ops.subscribe(Box::new(on_event), cx)
    }

    /// Calls `on_notify` whenever the grid notifies, until the subscription
    /// is dropped: for a status bar that reads the counts again.
    pub fn observe(&self, cx: &mut App, on_notify: impl FnMut(&mut App) + 'static) -> Subscription {
        self.ops.observe(Box::new(on_notify), cx)
    }

    // The column chooser.

    /// Opens the grid's column chooser, as
    /// [`LiveGridState::show_column_chooser`] does.
    pub fn show_column_chooser(&self, window: &mut Window, cx: &mut App) {
        self.ops.show_column_chooser(window, cx);
    }

    /// Returns true while the grid shows its column chooser.
    pub fn is_column_chooser_open(&self, cx: &App) -> bool {
        self.ops.is_column_chooser_open(cx)
    }

    /// The grid behind the handle, for the column chooser.
    pub(super) fn ops(&self) -> &dyn GridOps {
        &*self.ops
    }
}

impl fmt::Debug for LiveGridHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LiveGridHandle")
            .field(&self.entity_id())
            .finish()
    }
}

impl<D: LiveGridDelegate> From<&Entity<LiveGridState<D>>> for LiveGridHandle {
    fn from(grid: &Entity<LiveGridState<D>>) -> Self {
        Self::new(grid)
    }
}

/// A [`LiveGridHandle`] that does not keep the grid alive.
#[derive(Clone)]
pub struct WeakLiveGridHandle {
    entity_id: EntityId,
    upgrade: Rc<dyn Fn() -> Option<LiveGridHandle>>,
}

impl WeakLiveGridHandle {
    /// The handle, if the grid is still alive.
    pub fn upgrade(&self) -> Option<LiveGridHandle> {
        (self.upgrade)()
    }

    /// The grid entity's id.
    pub fn entity_id(&self) -> EntityId {
        self.entity_id
    }
}

impl fmt::Debug for WeakLiveGridHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("WeakLiveGridHandle")
            .field(&self.entity_id)
            .finish()
    }
}

/// What a `LiveGridEvent` says without keys and columns.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GridHandleEvent {
    /// The column layout changed.
    LayoutChange {
        /// What changed.
        parts: LayoutParts,
        /// Who changed it.
        origin: LayoutOrigin,
    },
    /// The sort changed; [`LiveGridHandle::sort`] reads it.
    SortChange {
        /// Why it changed.
        origin: SortOrigin,
    },
    /// The grouping or the tree changed.
    GroupChange {
        /// Who changed it.
        origin: GroupOrigin,
    },
    /// Groups or tree rows were expanded or collapsed.
    ExpansionChange {
        /// Who changed it.
        origin: GroupOrigin,
    },
    /// A new display order is shown.
    ProjectionChange {
        /// Data rows in the shown order.
        row_count: usize,
        /// The version of the store snapshot the order was built from.
        store_version: u64,
    },
    /// The selection changed.
    SelectionChange {
        /// The selected rows, those a filter hides included.
        count: usize,
        /// Why it changed.
        origin: SelectionOrigin,
    },
    /// A newer order is held (`held`), or the held order was applied.
    OrderHeldChange {
        /// Whether a newer order is waiting.
        held: bool,
    },
}

impl GridHandleEvent {
    /// What `event` says without keys and columns; `None` for an event about
    /// rows or cells.
    fn from_grid_event<K: Eq + Hash, C>(event: &LiveGridEvent<K, C>) -> Option<Self> {
        Some(match event {
            LiveGridEvent::LayoutChange { parts, origin, .. } => Self::LayoutChange {
                parts: *parts,
                origin: *origin,
            },
            LiveGridEvent::SortChange { origin, .. } => Self::SortChange { origin: *origin },
            LiveGridEvent::GroupChange { origin } => Self::GroupChange { origin: *origin },
            LiveGridEvent::ExpansionChange { origin, .. } => {
                Self::ExpansionChange { origin: *origin }
            }
            LiveGridEvent::ProjectionChange {
                row_count,
                store_version,
            } => Self::ProjectionChange {
                row_count: *row_count,
                store_version: *store_version,
            },
            LiveGridEvent::SelectionChange { selection, origin } => Self::SelectionChange {
                count: selection.len(),
                origin: *origin,
            },
            LiveGridEvent::OrderHeldChange { held } => Self::OrderHeldChange { held: *held },
            _ => return None,
        })
    }
}

/// A schema column as the grid shows it now.
#[derive(Clone, Debug)]
pub struct ColumnInfo {
    id: ColumnId,
    title: SharedString,
    header_label: HeaderLabel,
    zone: ColumnZone,
    visible: bool,
    width: Pixels,
    bands: Box<[BandInfo]>,
    hideable: bool,
    internal: bool,
    movable: bool,
    resizable: bool,
    sortable: bool,
    groupable: bool,
    sort: Option<(SortDirection, usize)>,
}

impl ColumnInfo {
    /// The column's id.
    pub fn id(&self) -> &ColumnId {
        &self.id
    }

    /// The column's name: its header text, or the tooltip and accessible
    /// name of a header that shows an icon alone.
    pub fn title(&self) -> &SharedString {
        &self.title
    }

    /// The header's icon (its asset path), if it shows one.
    pub fn icon(&self) -> Option<&SharedString> {
        match &self.header_label {
            HeaderLabel::Title => None,
            HeaderLabel::IconAndTitle(icon) | HeaderLabel::Icon(icon) => Some(icon),
        }
    }

    /// What the header shows.
    pub fn header_label(&self) -> &HeaderLabel {
        &self.header_label
    }

    /// The zone the column is in.
    pub fn zone(&self) -> ColumnZone {
        self.zone
    }

    /// Returns true if the column is shown.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The column's width.
    pub fn width(&self) -> Pixels {
        self.width
    }

    /// The bands over the column, outermost first.
    pub fn bands(&self) -> &[BandInfo] {
        &self.bands
    }

    /// Returns true if the user may hide the column.
    pub fn is_hideable(&self) -> bool {
        self.hideable
    }

    /// Returns true if the column is internal (`GridColumn::with_internal`):
    /// never shown, and left out of a column chooser.
    pub fn is_internal(&self) -> bool {
        self.internal
    }

    /// Returns true if the user can drag the column's header.
    pub fn is_movable(&self) -> bool {
        self.movable
    }

    /// Returns true if the user can resize the column.
    pub fn is_resizable(&self) -> bool {
        self.resizable
    }

    /// Returns true if a click on the column's header sorts by it.
    pub fn is_sortable(&self) -> bool {
        self.sortable
    }

    /// Returns true if the user can group by the column.
    pub fn is_groupable(&self) -> bool {
        self.groupable
    }

    /// The column's sort direction, if the view sorts by it.
    pub fn sort(&self) -> Option<SortDirection> {
        self.sort.map(|(direction, _)| direction)
    }

    /// The column's place in a multi-column sort, from 0 for the most
    /// significant.
    pub fn sort_priority(&self) -> Option<usize> {
        self.sort.map(|(_, priority)| priority)
    }
}

/// A header band over a column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BandInfo {
    id: &'static str,
    title: SharedString,
}

impl BandInfo {
    /// The band's id (`ColumnBand::new`).
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// The band's header text.
    pub fn title(&self) -> &SharedString {
        &self.title
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// A handle of this grid without its delegate type: for a toolbar, a
    /// status bar or a column chooser.
    pub fn handle(&self, cx: &Context<Self>) -> LiveGridHandle {
        LiveGridHandle::new(&cx.entity())
    }

    /// The schema column whose id is `id`.
    pub(super) fn column_by_id(&self, id: &str) -> Option<ColumnOf<D>> {
        <ColumnOf<D> as crate::model::GridColumns>::all()
            .iter()
            .copied()
            .find(|column| column.id() == id)
            .filter(|column| self.columns.get(column.ix()).is_some())
    }

    /// The column `ix` as the grid shows it now; `None` outside the schema.
    fn column_info(&self, ix: ColumnIx) -> Option<ColumnInfo> {
        let column = self.columns.get(ix)?;
        let sort = self
            .view
            .sort()
            .iter()
            .enumerate()
            .find(|(_, (sorted, _))| sorted.ix() == ix)
            .map(|(priority, (_, direction))| (*direction, priority));
        Some(ColumnInfo {
            id: column.column().column_id(),
            title: column.title().clone(),
            header_label: column.header_label().clone(),
            zone: self.columns.zone_of(ix),
            visible: self.columns.is_visible(ix),
            width: self.columns.width(ix),
            bands: self
                .columns
                .bands_of(ix)
                .iter()
                .map(|&id| BandInfo {
                    id,
                    title: self
                        .columns
                        .band_title(id)
                        .cloned()
                        .unwrap_or_else(|| SharedString::new_static(id)),
                })
                .collect(),
            hideable: column.is_hideable(),
            internal: column.is_internal(),
            movable: column.is_movable(),
            resizable: column.is_resizable(),
            sortable: column.is_sortable(),
            groupable: column.is_groupable(),
            sort,
        })
    }

    /// The `'static` id of the band `band`, if a schema column is under it.
    pub(super) fn band_id(&self, band: &str) -> Option<&'static str> {
        self.columns
            .order()
            .iter()
            .flat_map(|ix| self.columns.bands_of(*ix).iter().copied())
            .find(|id| *id == band)
    }
}

/// The grid behind a handle: the typed entity, as an object-safe trait.
pub(super) trait GridOps {
    fn entity_id(&self) -> EntityId;
    fn downgrade(&self) -> WeakLiveGridHandle;
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    fn columns(&self, cx: &App) -> Vec<ColumnInfo>;
    fn column(&self, id: &str, cx: &App) -> Option<ColumnInfo>;
    fn set_column_visible(&self, id: &str, visible: bool, cx: &mut App) -> bool;
    fn show_all_columns(&self, cx: &mut App);
    fn move_column(&self, id: &str, before: Option<&str>, cx: &mut App) -> bool;
    fn move_band(&self, band: &str, before: Option<&str>, cx: &mut App) -> bool;
    fn set_column_zone(&self, id: &str, zone: ColumnZone, cx: &mut App) -> bool;
    fn set_column_width(&self, id: &str, width: Pixels, cx: &mut App) -> bool;
    fn autofit_columns(&self, cx: &mut App);
    fn view_id(&self, cx: &App) -> Option<SharedString>;
    fn layout(&self, cx: &App) -> GridLayout;
    fn set_layout(&self, layout: &GridLayout, cx: &mut App) -> Result<LayoutReport, LayoutError>;
    fn reset_layout(&self, cx: &mut App);
    fn sort(&self, cx: &App) -> Vec<LayoutSort>;
    fn set_sort(&self, sort: &[LayoutSort], cx: &mut App) -> bool;
    fn group_by(&self, cx: &App) -> Vec<LayoutGroup>;
    fn set_group_by(&self, levels: &[LayoutGroup], cx: &mut App) -> bool;
    fn expand_depth(&self, cx: &App) -> ExpandDepth;
    fn set_expand_depth(&self, depth: ExpandDepth, cx: &mut App);
    fn quick_filter(&self, cx: &App) -> SharedString;
    fn set_quick_filter(&self, text: SharedString, cx: &mut App);
    fn row_count(&self, cx: &App) -> usize;
    fn total_row_count(&self, cx: &App) -> usize;
    fn selected_count(&self, cx: &App) -> usize;
    fn is_order_held(&self, cx: &App) -> bool;
    fn apply_held_order(&self, cx: &mut App);
    fn set_reorder_policy(&self, policy: Box<dyn ReorderPolicy>, cx: &mut App);
    fn copy_scope(&self, scope: ExportScope, cx: &mut App) -> bool;
    fn is_export_empty(&self, scope: ExportScope, cx: &App) -> bool;
    #[cfg(not(target_family = "wasm"))]
    fn export_to_path(
        &self,
        scope: ExportScope,
        format: CsvFormat,
        path: std::path::PathBuf,
        cx: &mut App,
    ) -> Result<ExportJob<std::path::PathBuf>, ExportError>;
    fn subscribe(
        &self,
        on_event: Box<dyn FnMut(&GridHandleEvent, &mut App)>,
        cx: &mut App,
    ) -> Subscription;
    fn observe(&self, on_notify: Box<dyn FnMut(&mut App)>, cx: &mut App) -> Subscription;
    // The column chooser.
    fn show_column_chooser(&self, window: &mut Window, cx: &mut App);
    fn is_column_chooser_open(&self, cx: &App) -> bool;
    fn chooser(
        &self,
        command: ChooserCommand<'_>,
        window: Option<&mut Window>,
        cx: &mut App,
    ) -> bool;
}

/// [`GridOps`] of a grid with the delegate `D`.
struct Ops<D: LiveGridDelegate> {
    grid: Entity<LiveGridState<D>>,
}

impl<D: LiveGridDelegate> Ops<D> {
    /// Runs `command` on the column `id`; false, changing nothing, if the
    /// column is unknown.
    fn on_column(
        &self,
        id: &str,
        cx: &mut App,
        command: impl FnOnce(&mut LiveGridState<D>, ColumnOf<D>, &mut Context<LiveGridState<D>>) -> bool,
    ) -> bool {
        self.grid
            .update(cx, |grid, cx| match grid.column_by_id(id) {
                Some(column) => command(grid, column, cx),
                None => false,
            })
    }
}

impl<D: LiveGridDelegate> GridOps for Ops<D> {
    fn entity_id(&self) -> EntityId {
        self.grid.entity_id()
    }

    fn downgrade(&self) -> WeakLiveGridHandle {
        let grid: WeakEntity<LiveGridState<D>> = self.grid.downgrade();
        WeakLiveGridHandle {
            entity_id: self.grid.entity_id(),
            upgrade: Rc::new(move || grid.upgrade().map(|grid| LiveGridHandle::new(&grid))),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }

    fn columns(&self, cx: &App) -> Vec<ColumnInfo> {
        let grid = self.grid.read(cx);
        grid.columns
            .order()
            .iter()
            .filter_map(|ix| grid.column_info(*ix))
            .collect()
    }

    fn column(&self, id: &str, cx: &App) -> Option<ColumnInfo> {
        let grid = self.grid.read(cx);
        grid.column_info(grid.column_by_id(id)?.ix())
    }

    fn set_column_visible(&self, id: &str, visible: bool, cx: &mut App) -> bool {
        self.on_column(id, cx, |grid, column, cx| {
            grid.set_column_visible(column, visible, cx)
        })
    }

    fn show_all_columns(&self, cx: &mut App) {
        self.grid.update(cx, LiveGridState::show_all_columns);
    }

    fn move_column(&self, id: &str, before: Option<&str>, cx: &mut App) -> bool {
        self.grid.update(cx, |grid, cx| {
            let Some(column) = grid.column_by_id(id) else {
                return false;
            };
            let before = match before {
                Some(before) => match grid.column_by_id(before) {
                    Some(before) => Some(before),
                    None => return false,
                },
                None => None,
            };
            grid.move_column(column, before, cx)
        })
    }

    fn move_band(&self, band: &str, before: Option<&str>, cx: &mut App) -> bool {
        self.grid.update(cx, |grid, cx| {
            let Some(band) = grid.band_id(band) else {
                return false;
            };
            let before = match before {
                Some(before) => match grid.column_by_id(before) {
                    Some(before) => Some(before),
                    None => return false,
                },
                None => None,
            };
            grid.move_band(band, before, cx)
        })
    }

    fn set_column_zone(&self, id: &str, zone: ColumnZone, cx: &mut App) -> bool {
        self.on_column(id, cx, |grid, column, cx| {
            grid.set_column_zone(column, zone, cx);
            true
        })
    }

    fn set_column_width(&self, id: &str, width: Pixels, cx: &mut App) -> bool {
        self.on_column(id, cx, |grid, column, cx| {
            grid.set_column_width(column, width, cx);
            true
        })
    }

    fn autofit_columns(&self, cx: &mut App) {
        self.grid.update(cx, LiveGridState::autofit_columns);
    }

    fn view_id(&self, cx: &App) -> Option<SharedString> {
        self.grid.read(cx).view_id().cloned()
    }

    fn layout(&self, cx: &App) -> GridLayout {
        self.grid.read(cx).layout()
    }

    fn set_layout(&self, layout: &GridLayout, cx: &mut App) -> Result<LayoutReport, LayoutError> {
        self.grid.update(cx, |grid, cx| grid.set_layout(layout, cx))
    }

    fn reset_layout(&self, cx: &mut App) {
        self.grid.update(cx, LiveGridState::reset_layout);
    }

    fn sort(&self, cx: &App) -> Vec<LayoutSort> {
        self.grid
            .read(cx)
            .sort()
            .iter()
            .map(|(column, direction)| LayoutSort::new(column.column_id(), *direction))
            .collect()
    }

    fn set_sort(&self, sort: &[LayoutSort], cx: &mut App) -> bool {
        self.grid.update(cx, |grid, cx| {
            let Some(sort) = sort
                .iter()
                .map(|level| {
                    Some((
                        grid.column_by_id(level.column().as_ref())?,
                        level.direction(),
                    ))
                })
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            grid.set_sort(sort, cx);
            true
        })
    }

    fn group_by(&self, cx: &App) -> Vec<LayoutGroup> {
        self.grid
            .read(cx)
            .group_by()
            .iter()
            .map(|level| LayoutGroup::new(level.id(), level.direction()))
            .collect()
    }

    fn set_group_by(&self, levels: &[LayoutGroup], cx: &mut App) -> bool {
        self.grid.update(cx, |grid, cx| {
            let Some(levels) = levels
                .iter()
                .map(|level| {
                    let column = grid.column_by_id(level.column().as_ref())?;
                    Some(GroupBy::column(column).with_direction(level.direction()))
                })
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            grid.set_group_by(levels, cx);
            true
        })
    }

    fn expand_depth(&self, cx: &App) -> ExpandDepth {
        self.grid.read(cx).expand_depth()
    }

    fn set_expand_depth(&self, depth: ExpandDepth, cx: &mut App) {
        self.grid
            .update(cx, |grid, cx| grid.set_expand_depth(depth, cx));
    }

    fn quick_filter(&self, cx: &App) -> SharedString {
        self.grid.read(cx).quick_filter().clone()
    }

    fn set_quick_filter(&self, text: SharedString, cx: &mut App) {
        self.grid
            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
    }

    fn row_count(&self, cx: &App) -> usize {
        self.grid.read(cx).projection.row_count()
    }

    fn total_row_count(&self, cx: &App) -> usize {
        self.grid.read(cx).snapshot.len()
    }

    fn selected_count(&self, cx: &App) -> usize {
        self.grid.read(cx).selection().len()
    }

    fn is_order_held(&self, cx: &App) -> bool {
        self.grid.read(cx).is_order_held()
    }

    fn apply_held_order(&self, cx: &mut App) {
        self.grid.update(cx, LiveGridState::apply_held_order);
    }

    fn set_reorder_policy(&self, policy: Box<dyn ReorderPolicy>, cx: &mut App) {
        self.grid.update(cx, |grid, cx| {
            // As `set_reorder_policy`, with the policy already boxed.
            grid.reorder = policy;
            grid.reconsider_held(cx);
        });
    }

    fn copy_scope(&self, scope: ExportScope, cx: &mut App) -> bool {
        self.grid.update(cx, |grid, cx| grid.copy_scope(scope, cx))
    }

    fn is_export_empty(&self, scope: ExportScope, cx: &App) -> bool {
        self.grid.read(cx).is_export_empty(scope)
    }

    #[cfg(not(target_family = "wasm"))]
    fn export_to_path(
        &self,
        scope: ExportScope,
        format: CsvFormat,
        path: std::path::PathBuf,
        cx: &mut App,
    ) -> Result<ExportJob<std::path::PathBuf>, ExportError> {
        self.grid.update(cx, |grid, cx| {
            grid.export_to_path(ExportRequest::new(scope), format, path, cx)
        })
    }

    fn subscribe(
        &self,
        mut on_event: Box<dyn FnMut(&GridHandleEvent, &mut App)>,
        cx: &mut App,
    ) -> Subscription {
        cx.subscribe(&self.grid, move |_, event: &LiveGridEvent<_, _>, cx| {
            if let Some(event) = GridHandleEvent::from_grid_event(event) {
                on_event(&event, cx);
            }
        })
    }

    fn observe(&self, mut on_notify: Box<dyn FnMut(&mut App)>, cx: &mut App) -> Subscription {
        cx.observe(&self.grid, move |_, cx| on_notify(cx))
    }

    fn show_column_chooser(&self, window: &mut Window, cx: &mut App) {
        self.grid
            .update(cx, |grid, cx| grid.show_column_chooser(window, cx));
    }

    fn is_column_chooser_open(&self, cx: &App) -> bool {
        self.grid.read(cx).is_column_chooser_open()
    }

    fn chooser(
        &self,
        command: ChooserCommand<'_>,
        window: Option<&mut Window>,
        cx: &mut App,
    ) -> bool {
        self.grid
            .update(cx, |grid, cx| grid.run_chooser_command(command, window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::selection::GridSelection;

    #[test]
    fn test_handle_event_translation() {
        let event = |event: LiveGridEvent<u32, u16>| GridHandleEvent::from_grid_event(&event);
        assert_eq!(
            event(LiveGridEvent::OrderHeldChange { held: true }),
            Some(GridHandleEvent::OrderHeldChange { held: true })
        );
        assert_eq!(
            event(LiveGridEvent::ProjectionChange {
                row_count: 12,
                store_version: 3
            }),
            Some(GridHandleEvent::ProjectionChange {
                row_count: 12,
                store_version: 3
            })
        );
        assert_eq!(
            event(LiveGridEvent::SelectionChange {
                selection: GridSelection::default(),
                origin: SelectionOrigin::Programmatic,
            }),
            Some(GridHandleEvent::SelectionChange {
                count: 0,
                origin: SelectionOrigin::Programmatic
            })
        );
        // An event about rows or cells has no untyped form.
        assert_eq!(
            event(LiveGridEvent::Confirm {
                keys: [1].into(),
                secondary: false
            }),
            None
        );
    }
}
