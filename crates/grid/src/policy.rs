//! Replaceable behaviors of the view (AGENTS.md, "Replaceable Behavior"):
//! when a new display order is shown, where background work runs, how
//! columns move, how autofit measures, how a header click changes the sort,
//! which selected rows a command acts on, which lines take the selection
//! and what a command over a line acts on, how a press or a key edits the
//! selection, which press opens the context menu, which press expands a
//! group caption or a tree row, which input opens a cell editor and how an
//! edit behaves under live data.

use std::{ops::Range, sync::Arc, time::Duration};

use gpui_kit::{App, AppContext as _, Modifiers, MouseButton, Pixels, Task, px};
use web_time::Instant;

use crate::{
    engine::{Branch, GroupInfo, Projection, SortDirection},
    model::{ColumnIx, ColumnSet, GridColumns as _, GridRow, OwnedValue},
    state::{
        CellContent, ColumnZone, DropPlace, DropTarget, EditInput, GridSelection, RowDrag,
        ScopedSelection, SelectionMode,
    },
    store::StoreSnapshot,
};

/// What a [`ReorderPolicy`] knows when a new display order is ready.
#[derive(Clone, Copy, Debug)]
pub struct ReorderContext {
    now: Instant,
    held_since: Instant,
    pointer_inside: bool,
    user_action: bool,
    menu_open: bool,
    tooltip_shown: bool,
    // Editing, waiting commits, row drags and the column chooser.
    editing: bool,
    pending_edits: bool,
    drag_over: bool,
    column_chooser_open: bool,
}

impl ReorderContext {
    pub(crate) fn new(
        now: Instant,
        held_since: Instant,
        pointer_inside: bool,
        user_action: bool,
    ) -> Self {
        Self {
            now,
            held_since,
            pointer_inside,
            user_action,
            menu_open: false,
            tooltip_shown: false,
            editing: false,
            pending_edits: false,
            drag_over: false,
            column_chooser_open: false,
        }
    }

    /// This context with the grid's context menu open or closed.
    pub(crate) fn with_menu_open(mut self, menu_open: bool) -> Self {
        self.menu_open = menu_open;
        self
    }

    /// This context with a cell tooltip shown or not.
    pub(crate) fn with_tooltip_shown(mut self, tooltip_shown: bool) -> Self {
        self.tooltip_shown = tooltip_shown;
        self
    }

    /// This context with a cell editor open or not.
    pub(crate) fn with_editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    /// This context with commits waiting or not.
    pub(crate) fn with_pending_edits(mut self, pending_edits: bool) -> Self {
        self.pending_edits = pending_edits;
        self
    }

    /// This context with the grid's column chooser open or not.
    pub(crate) fn with_column_chooser_open(mut self, column_chooser_open: bool) -> Self {
        self.column_chooser_open = column_chooser_open;
        self
    }

    /// This context with a drag over the rows or not.
    pub(crate) fn with_drag_over(mut self, drag_over: bool) -> Self {
        self.drag_over = drag_over;
        self
    }

    /// The current time of the store clock.
    pub fn now(&self) -> Instant {
        self.now
    }

    /// When the shown order became outdated (equals `now` for a fresh one).
    pub fn held_since(&self) -> Instant {
        self.held_since
    }

    /// Returns true if the pointer is over the grid body.
    pub fn is_pointer_inside(&self) -> bool {
        self.pointer_inside
    }

    /// Returns true if the new order comes from a user action (a sort click,
    /// a new view, a new data set) rather than from streaming data.
    pub fn is_user_action(&self) -> bool {
        self.user_action
    }

    /// Returns true while the grid's context menu is open. The built-in
    /// policies hold the order then, except for a user action: the rows of
    /// the menu's target stay where they were.
    pub fn is_menu_open(&self) -> bool {
        self.menu_open
    }

    /// Returns true while a cell tooltip is shown: GPUI built it for the
    /// hovered cell, which has not changed since, and no button was pressed.
    /// The built-in policies ignore it, since a resting pointer would hold
    /// the order without end; a policy of the application may hold on it.
    pub fn is_tooltip_shown(&self) -> bool {
        self.tooltip_shown
    }

    /// Returns true while a cell editor is open: the built-in policies then
    /// hold the order, as for the context menu, unless the new order comes
    /// from a user action.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Returns true while commits wait for `LiveGridState::resolve_edit`
    /// or for the store. The built-in policies ignore it; a policy may hold
    /// on it.
    pub fn has_pending_edits(&self) -> bool {
        self.pending_edits
    }

    /// Returns true while a row drag is over this grid's rows. The built-in
    /// policies ignore it.
    pub fn is_drag_over(&self) -> bool {
        self.drag_over
    }

    /// Returns true while the grid's column chooser is open. The built-in
    /// policies ignore it.
    pub fn is_column_chooser_open(&self) -> bool {
        self.column_chooser_open
    }
}

/// What to do with a new display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReorderDecision {
    /// Show it now.
    Apply,
    /// Keep the current order until this instant, then ask again.
    HoldUntil(Instant),
    /// Keep the current order until the pointer leaves, the application calls
    /// [`crate::LiveGridState::apply_held_order`], or a user action arrives.
    Hold,
}

/// Decides when a new display order replaces the shown one, so rows do not
/// jump under the user's pointer more than the application wants.
///
/// The default is [`LiveReorder`], the industry standard, as in AG Grid and
/// DevExpress; [`HoldReorder`] and [`ManualReorder`] are the built-in
/// alternatives. All three hold the order while the context menu or a cell
/// editor is open ([`ReorderContext::is_menu_open`],
/// [`ReorderContext::is_editing`]), unless the new order comes from a user
/// action. None holds it for commits that wait
/// ([`ReorderContext::has_pending_edits`]): a policy of the application may.
pub trait ReorderPolicy: 'static {
    /// Called when a new order is ready, when the pointer leaves the grid,
    /// when the context menu or a cell editor closes, and when a
    /// [`ReorderDecision::HoldUntil`] deadline passes.
    fn decide(&self, context: &ReorderContext) -> ReorderDecision;
}

/// Show every new order at once (throttled by `min_rebuild_interval`); hold
/// it only while the context menu or a cell editor is open: the row under
/// the field stays where it is.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveReorder;

impl ReorderPolicy for LiveReorder {
    fn decide(&self, context: &ReorderContext) -> ReorderDecision {
        if (context.is_menu_open() || context.is_editing()) && !context.is_user_action() {
            ReorderDecision::Hold
        } else {
            ReorderDecision::Apply
        }
    }
}

/// Keep the order while the pointer is over the grid, at most `max`, and
/// while the context menu or a cell editor is open, however long.
#[derive(Clone, Copy, Debug)]
pub struct HoldReorder {
    max: Duration,
}

impl HoldReorder {
    /// Holds for at most `max` while the pointer is over the grid.
    pub fn new(max: Duration) -> Self {
        Self { max }
    }
}

impl Default for HoldReorder {
    fn default() -> Self {
        Self::new(Duration::from_secs(3))
    }
}

impl ReorderPolicy for HoldReorder {
    fn decide(&self, context: &ReorderContext) -> ReorderDecision {
        if context.is_user_action() {
            return ReorderDecision::Apply;
        }
        // The menu and the editor close by an explicit act, so this hold has
        // an end.
        if context.is_menu_open() || context.is_editing() {
            return ReorderDecision::Hold;
        }
        if !context.is_pointer_inside() {
            return ReorderDecision::Apply;
        }
        let deadline = context.held_since() + self.max;
        if context.now() >= deadline {
            ReorderDecision::Apply
        } else {
            ReorderDecision::HoldUntil(deadline)
        }
    }
}

/// Keep the order until the application applies it (for example from a
/// "Refresh order" button); user actions still apply at once. An open
/// context menu changes nothing: the order is held anyway.
#[derive(Clone, Copy, Debug, Default)]
pub struct ManualReorder;

impl ReorderPolicy for ManualReorder {
    fn decide(&self, context: &ReorderContext) -> ReorderDecision {
        if context.is_user_action() {
            ReorderDecision::Apply
        } else {
            ReorderDecision::Hold
        }
    }
}

/// Runs the view's background jobs (projection rebuilds and compactions).
///
/// The default, [`BackgroundRunner`], uses GPUI's background executor.
/// [`InlineRunner`] runs jobs on the calling thread, for platforms without
/// background threads (WebAssembly without `SharedArrayBuffer`).
pub trait TaskRunner: 'static {
    /// Runs `job`; the returned task completes when it is done.
    fn run(&self, job: Box<dyn FnOnce() + Send>, cx: &App) -> Task<()>;
}

/// Runs jobs on GPUI's background executor.
#[derive(Clone, Copy, Debug, Default)]
pub struct BackgroundRunner;

impl TaskRunner for BackgroundRunner {
    fn run(&self, job: Box<dyn FnOnce() + Send>, cx: &App) -> Task<()> {
        cx.background_spawn(async move { job() })
    }
}

/// Runs jobs at once on the calling (UI) thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineRunner;

impl TaskRunner for InlineRunner {
    fn run(&self, job: Box<dyn FnOnce() + Send>, _: &App) -> Task<()> {
        job();
        Task::ready(())
    }
}

/// One zone of the column layout, as a [`ColumnMovePolicy`] sees it.
pub struct ColumnMoveContext<'a> {
    zone: ColumnZone,
    columns: &'a [ColumnIx],
    visible: ColumnSet,
    bands: &'a [Box<[&'static str]>],
}

impl<'a> ColumnMoveContext<'a> {
    pub(crate) fn new(
        zone: ColumnZone,
        columns: &'a [ColumnIx],
        visible: ColumnSet,
        bands: &'a [Box<[&'static str]>],
    ) -> Self {
        Self {
            zone,
            columns,
            visible,
            bands,
        }
    }

    /// The zone.
    pub fn zone(&self) -> ColumnZone {
        self.zone
    }

    /// The zone's columns in display order, hidden ones included.
    pub fn columns(&self) -> &'a [ColumnIx] {
        self.columns
    }

    /// Returns true if `column` is shown.
    pub fn is_visible(&self, column: ColumnIx) -> bool {
        self.visible.contains(column)
    }

    /// Ids of the bands that contain `column`, outermost first.
    pub fn bands(&self, column: ColumnIx) -> &'a [&'static str] {
        self.bands.get(column.get()).map_or(&[], |bands| bands)
    }
}

/// The whole column order, as [`ColumnMovePolicy::normalize`] may change
/// it: it can only reorder columns and change their zones. The grid then
/// sorts the order by zone, keeping the order within each zone.
pub struct ColumnOrder<'a> {
    order: &'a mut Vec<ColumnIx>,
    zones: &'a mut [ColumnZone],
    bands: &'a [Box<[&'static str]>],
}

impl<'a> ColumnOrder<'a> {
    pub(crate) fn new(
        order: &'a mut Vec<ColumnIx>,
        zones: &'a mut [ColumnZone],
        bands: &'a [Box<[&'static str]>],
    ) -> Self {
        Self {
            order,
            zones,
            bands,
        }
    }

    /// Every column of the schema in display order, hidden ones included.
    pub fn columns(&self) -> &[ColumnIx] {
        self.order
    }

    /// The zone of `column`.
    pub fn zone(&self, column: ColumnIx) -> ColumnZone {
        self.zones.get(column.get()).copied().unwrap_or_default()
    }

    /// Ids of the bands that contain `column`, outermost first.
    pub fn bands(&self, column: ColumnIx) -> &'a [&'static str] {
        self.bands.get(column.get()).map_or(&[], |bands| bands)
    }

    /// Moves `column` to `zone`.
    pub fn set_zone(&mut self, column: ColumnIx, zone: ColumnZone) {
        if let Some(slot) = self.zones.get_mut(column.get()) {
            *slot = zone;
        }
    }

    /// Replaces the order. Returns false, changing nothing, unless `columns`
    /// is a permutation of [`Self::columns`].
    pub fn reorder(&mut self, columns: &[ColumnIx]) -> bool {
        let set = |columns: &[ColumnIx]| {
            columns.iter().fold(ColumnSet::EMPTY, |mut set, column| {
                set.insert(*column);
                set
            })
        };
        if columns.len() != self.order.len() || set(columns) != set(self.order) {
            return false;
        }
        self.order.clear();
        self.order.extend_from_slice(columns);
        true
    }
}

/// Decides which columns move together and where they may go, for header
/// drags, `move_column`, `move_band`, zone changes and the move Actions.
///
/// The default is [`BandedMove`]: a [`ColumnBand`](crate::ColumnBand) stays
/// together, as in DevExpress, Syncfusion, Kendo and Infragistics.
/// [`FreeMove`] lets a column leave its band, as AG Grid does.
pub trait ColumnMovePolicy: 'static {
    /// Whether `block` (a range of `context.columns()`) may be inserted at
    /// `gap` (`0..=columns().len()`, counted before the block is removed).
    fn is_allowed(&self, context: &ColumnMoveContext<'_>, block: Range<usize>, gap: usize) -> bool;

    /// The columns that change zone together with `column`: a range of
    /// `context.columns()` that contains it.
    fn zone_block(&self, context: &ColumnMoveContext<'_>, column: ColumnIx) -> Range<usize>;

    /// Restores the policy's invariants after the schema is built or a layout
    /// is restored. Stable: a valid order stays as it is.
    fn normalize(&self, _order: &mut ColumnOrder<'_>) {}
}

/// A band stays together: a column moves only among the children of its
/// innermost band, a band moves as a whole, and a zone change takes the
/// whole top-level band.
#[derive(Clone, Copy, Debug, Default)]
pub struct BandedMove;

/// A child of a band (or of a zone): a nested band or a column.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BandChild {
    Band(&'static str),
    Column(ColumnIx),
}

fn band_child(context: &ColumnMoveContext<'_>, column: ColumnIx, level: usize) -> BandChild {
    match context.bands(column).get(level) {
        Some(band) => BandChild::Band(band),
        None => BandChild::Column(column),
    }
}

/// The range of `columns` whose bands at `level` are `band`, from the first
/// to the last of them.
fn band_span(
    context: &ColumnMoveContext<'_>,
    level: usize,
    band: &'static str,
) -> Option<Range<usize>> {
    let columns = context.columns();
    let is_member = |column: &ColumnIx| context.bands(*column).get(level) == Some(&band);
    let start = columns.iter().position(is_member)?;
    let end = columns.iter().rposition(is_member)? + 1;
    Some(start..end)
}

impl ColumnMovePolicy for BandedMove {
    fn is_allowed(&self, context: &ColumnMoveContext<'_>, block: Range<usize>, gap: usize) -> bool {
        let columns = context.columns();
        if block.is_empty() || block.end > columns.len() || gap > columns.len() {
            return false;
        }
        // The bands every column of the block is in.
        let mut common = context.bands(columns[block.start]);
        for column in &columns[block.clone()] {
            let bands = context.bands(*column);
            let shared = common.iter().zip(bands).take_while(|(a, b)| a == b).count();
            common = &common[..shared];
        }
        // A block that is a whole band moves among that band's siblings (and
        // a band that is all of its parent band, among the parent's).
        while let Some((&deepest, outer)) = common.split_last()
            && band_span(context, outer.len(), deepest)
                .is_some_and(|span| block.start <= span.start && span.end <= block.end)
        {
            common = outer;
        }
        let level = common.len();
        let parent = match common.last() {
            Some(&band) => match band_span(context, level - 1, band) {
                Some(span) => span,
                None => return false,
            },
            None => 0..columns.len(),
        };
        if gap < parent.start || gap > parent.end {
            return false;
        }
        gap == parent.start
            || gap == parent.end
            || band_child(context, columns[gap - 1], level)
                != band_child(context, columns[gap], level)
    }

    fn zone_block(&self, context: &ColumnMoveContext<'_>, column: ColumnIx) -> Range<usize> {
        let columns = context.columns();
        let Some(position) = columns.iter().position(|c| *c == column) else {
            return 0..0;
        };
        match context.bands(column).first() {
            Some(&top) => band_span(context, 0, top).unwrap_or(position..position + 1),
            None => position..position + 1,
        }
    }

    fn normalize(&self, order: &mut ColumnOrder<'_>) {
        let columns = order.columns().to_vec();
        // Every top-level band takes the zone of its first column.
        let mut zones: Vec<(&'static str, ColumnZone)> = Vec::new();
        for &column in &columns {
            let Some(&top) = order.bands(column).first() else {
                continue;
            };
            match zones.iter().find(|(band, _)| *band == top) {
                Some(&(_, zone)) => order.set_zone(column, zone),
                None => zones.push((top, order.zone(column))),
            }
        }
        let gathered = gather(&columns, 0, order);
        order.reorder(&gathered);
    }
}

/// Gathers every band at `level` into one run at the place of its first
/// column, keeping the order inside it; then the bands one level deeper.
fn gather(columns: &[ColumnIx], level: usize, order: &ColumnOrder<'_>) -> Vec<ColumnIx> {
    let mut groups: Vec<(Option<&'static str>, Vec<ColumnIx>)> = Vec::new();
    for &column in columns {
        match order.bands(column).get(level) {
            Some(&band) => match groups.iter_mut().find(|(key, _)| *key == Some(band)) {
                Some((_, members)) => members.push(column),
                None => groups.push((Some(band), vec![column])),
            },
            None => groups.push((None, vec![column])),
        }
    }
    groups
        .into_iter()
        .flat_map(|(key, members)| match key {
            Some(_) => gather(&members, level + 1, order),
            None => members,
        })
        .collect()
}

/// Any column moves anywhere in its zone, out of its band too: a band then
/// shows one header cell per run of its columns, as AG Grid does.
#[derive(Clone, Copy, Debug, Default)]
pub struct FreeMove;

impl ColumnMovePolicy for FreeMove {
    fn is_allowed(&self, context: &ColumnMoveContext<'_>, block: Range<usize>, gap: usize) -> bool {
        !block.is_empty() && block.end <= context.columns().len() && gap <= context.columns().len()
    }

    fn zone_block(&self, context: &ColumnMoveContext<'_>, column: ColumnIx) -> Range<usize> {
        context
            .columns()
            .iter()
            .position(|c| *c == column)
            .map_or(0..0, |position| position..position + 1)
    }
}

/// What an [`AutofitPolicy`] can measure for one column. Widths are text
/// widths, without cell padding.
pub struct AutofitContext<'a> {
    header_width: Pixels,
    pinned_width: Pixels,
    // The texts of the painted aggregates.
    summary_width: Pixels,
    painted_rows: Range<usize>,
    row_count: usize,
    measure: &'a mut dyn FnMut(usize) -> Option<Pixels>,
}

impl<'a> AutofitContext<'a> {
    pub(crate) fn new(
        header_width: Pixels,
        painted_rows: Range<usize>,
        row_count: usize,
        measure: &'a mut dyn FnMut(usize) -> Option<Pixels>,
    ) -> Self {
        Self {
            header_width,
            pinned_width: Pixels::ZERO,
            summary_width: Pixels::ZERO,
            painted_rows,
            row_count,
            measure,
        }
    }

    /// Sets the widest text of the column in the painted pinned rows.
    pub(crate) fn with_pinned_width(mut self, width: Pixels) -> Self {
        self.pinned_width = width;
        self
    }

    /// The header title plus the sort indicator lane; a header icon and its
    /// gap before the title, or the icon alone in place of it.
    pub fn header_width(&self) -> Pixels {
        self.header_width
    }

    /// The widest text of the column in the pinned rows painted in this
    /// frame, both bands; zero without pinned rows.
    pub fn pinned_width(&self) -> Pixels {
        self.pinned_width
    }

    /// Sets the widest aggregate text of the column on the summary lines
    /// painted in this frame.
    pub(crate) fn with_summary_width(mut self, width: Pixels) -> Self {
        self.summary_width = width;
        self
    }

    /// The widest aggregate text of the column in the group captions, group
    /// footers and totals row painted in this frame, the totals label
    /// "Total" in its column included, each in the weight it is painted
    /// in; zero without them. A caption's own text, which runs across
    /// columns, is not part of it.
    pub fn summary_width(&self) -> Pixels {
        self.summary_width
    }

    /// Display rows painted in this frame.
    pub fn painted_rows(&self) -> Range<usize> {
        self.painted_rows.clone()
    }

    /// Rows in the projection.
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// Text width of the column's cell in a display row; `None` for a
    /// removed row or one past the end.
    pub fn measure_row(&mut self, display_ix: usize) -> Option<Pixels> {
        (self.measure)(display_ix)
    }
}

/// Chooses the text width a column needs when it is autofitted (a double
/// click on its resize handle, `autofit_column`, `autofit_columns`). The
/// grid adds the cell padding and clamps to the column's limits.
///
/// Called once per column when the fit is applied, never per frame.
pub trait AutofitPolicy: 'static {
    /// The text width the column needs.
    fn content_width(&self, context: &mut AutofitContext<'_>) -> Pixels;
}

/// Fits the header title, the pinned rows, the aggregates and the data rows
/// painted in the frame of the fit; rows off screen are not measured, so the
/// cost does not grow with the row count (AG Grid does the same).
#[derive(Clone, Copy, Debug, Default)]
pub struct VisibleRowsAutofit;

impl AutofitPolicy for VisibleRowsAutofit {
    fn content_width(&self, context: &mut AutofitContext<'_>) -> Pixels {
        let floor = context
            .header_width()
            .max(context.pinned_width())
            .max(context.summary_width());
        context
            .painted_rows()
            .filter_map(|display_ix| context.measure_row(display_ix))
            .fold(floor, |widest, width| widest.max(width))
    }
}

/// What a [`SortCycle`] knows about a header click.
#[derive(Clone, Copy, Debug)]
pub struct SortCycleContext<'a> {
    sort: &'a [(ColumnIx, SortDirection)],
    column: ColumnIx,
    modifiers: Modifiers,
}

impl<'a> SortCycleContext<'a> {
    pub(crate) fn new(
        sort: &'a [(ColumnIx, SortDirection)],
        column: ColumnIx,
        modifiers: Modifiers,
    ) -> Self {
        Self {
            sort,
            column,
            modifiers,
        }
    }

    /// The sort before the click, most significant column first.
    pub fn sort(&self) -> &'a [(ColumnIx, SortDirection)] {
        self.sort
    }

    /// The column whose header was clicked.
    pub fn column(&self) -> ColumnIx {
        self.column
    }

    /// The modifier keys held during the click.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// The direction of the clicked column in the sort, if it is sorted.
    pub fn direction(&self) -> Option<SortDirection> {
        self.sort
            .iter()
            .find(|(column, _)| *column == self.column)
            .map(|(_, direction)| *direction)
    }
}

/// How a header click changes the sort.
///
/// The default is [`StandardSortCycle`]: ascending, descending, unsorted;
/// Shift adds the column to a multi-column sort. Called once per click on
/// the UI thread, and only for a column with
/// [`GridColumn::is_sortable`](crate::GridColumn::is_sortable). The grid
/// drops repeated columns of the result (the first one stays) and columns
/// outside the schema's column type.
pub trait SortCycle: 'static {
    /// The sort after a click on `context.column()`, most significant column
    /// first; empty for the insertion order.
    fn next_sort(&self, context: &SortCycleContext<'_>) -> Vec<(ColumnIx, SortDirection)>;
}

/// Ascending, then descending, then unsorted (the insertion order), as in AG
/// Grid, Syncfusion and MUI X.
///
/// A click without Shift steps the clicked column from its own direction and
/// drops every other column, even from a multi-column sort. A Shift-click
/// adds the column as the least significant one, flips it in place, then
/// removes it; the others keep their order. Shift on every platform: on
/// macOS Cmd-click means "add to the selection", not "add to the sort".
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardSortCycle;

impl SortCycle for StandardSortCycle {
    fn next_sort(&self, context: &SortCycleContext<'_>) -> Vec<(ColumnIx, SortDirection)> {
        let column = context.column();
        let direction = context.direction();
        if !context.modifiers().shift {
            return match direction {
                None => vec![(column, SortDirection::Ascending)],
                Some(SortDirection::Ascending) => vec![(column, SortDirection::Descending)],
                Some(SortDirection::Descending) => Vec::new(),
            };
        }
        let mut sort = context.sort().to_vec();
        match direction {
            None => sort.push((column, SortDirection::Ascending)),
            Some(SortDirection::Ascending) => {
                for (sorted, direction) in &mut sort {
                    if *sorted == column {
                        *direction = SortDirection::Descending;
                    }
                }
            }
            Some(SortDirection::Descending) => sort.retain(|(sorted, _)| *sorted != column),
        }
        sort
    }
}

/// What a [`SelectionScope`] knows when a command asks what it acts on.
pub struct SelectionScopeContext<'a, R: GridRow> {
    selection: &'a GridSelection<R::Key>,
    projection: &'a Projection,
    snapshot: &'a StoreSnapshot<R>,
    mode: SelectionMode,
}

impl<'a, R: GridRow> SelectionScopeContext<'a, R> {
    pub(crate) fn new(
        selection: &'a GridSelection<R::Key>,
        projection: &'a Projection,
        snapshot: &'a StoreSnapshot<R>,
        mode: SelectionMode,
    ) -> Self {
        Self {
            selection,
            projection,
            snapshot,
            mode,
        }
    }

    /// The whole selection, by key; it may hold rows a filter hides.
    pub fn selection(&self) -> &'a GridSelection<R::Key> {
        self.selection
    }

    /// The projection on screen (not one being built or held back).
    pub fn projection(&self) -> &'a Projection {
        self.projection
    }

    /// The snapshot the screen shows.
    pub fn snapshot(&self) -> &'a StoreSnapshot<R> {
        self.snapshot
    }

    /// The selection mode.
    pub fn mode(&self) -> SelectionMode {
        self.mode
    }
}

/// Which selected rows and cells a command acts on: Enter, a double click,
/// the primary menu command, copying. Called once per command on the UI
/// thread.
///
/// The default is [`ProjectionScope`]: the selection within the rows on
/// screen. [`StoreScope`] also takes selected rows a filter hides.
pub trait SelectionScope<R: GridRow>: 'static {
    /// The rows and cells, in the order the command sees them.
    fn resolve(&self, context: &SelectionScopeContext<'_, R>) -> ScopedSelection<R::Key>;
}

/// The selection within the projection on screen, in its order: rows a
/// filter hides and rows removed since are left out; selected rows of a
/// collapsed group are in. The default.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProjectionScope;

/// The selected rows `projection` passes, in its order (`rows()`): a row
/// of a collapsed group too. One pass over the projection when most of it
/// is selected, else a lookup per selected row and a sort by position.
fn shown_selection<R: GridRow>(context: &SelectionScopeContext<'_, R>) -> Vec<(R::Key, ColumnSet)> {
    let (selection, projection, snapshot) = (
        context.selection(),
        context.projection(),
        context.snapshot(),
    );
    if selection.is_empty() {
        return Vec::new();
    }
    if selection.len() > projection.rows().len() / 8 {
        return projection
            .rows()
            .iter()
            .filter_map(|id| snapshot.row(*id))
            .filter_map(|row| {
                let columns = selection.columns(row.key());
                (!columns.is_empty()).then(|| (row.key().clone(), columns))
            })
            .collect();
    }
    let mut shown: Vec<(usize, R::Key, ColumnSet)> = selection
        .iter()
        .filter_map(|key| {
            let order_ix = projection.order_ix(snapshot.row_id(key)?)?;
            Some((order_ix, key.clone(), selection.columns(key)))
        })
        .collect();
    shown.sort_unstable_by_key(|(order_ix, _, _)| *order_ix);
    shown
        .into_iter()
        .map(|(_, key, columns)| (key, columns))
        .collect()
}

impl<R: GridRow> SelectionScope<R> for ProjectionScope {
    fn resolve(&self, context: &SelectionScopeContext<'_, R>) -> ScopedSelection<R::Key> {
        shown_selection(context).into_iter().collect()
    }
}

/// The selection within the store: the rows on screen in display order,
/// then selected rows a filter hides, in insertion order. Removed rows never
/// are.
#[derive(Clone, Copy, Debug, Default)]
pub struct StoreScope;

impl<R: GridRow> SelectionScope<R> for StoreScope {
    fn resolve(&self, context: &SelectionScopeContext<'_, R>) -> ScopedSelection<R::Key> {
        let (selection, projection, snapshot) = (
            context.selection(),
            context.projection(),
            context.snapshot(),
        );
        let mut hidden: Vec<(u64, R::Key, ColumnSet)> = selection
            .iter()
            .filter_map(|key| {
                let id = snapshot.row_id(key)?;
                (!projection.contains(id))
                    .then(|| (snapshot.seq(id), key.clone(), selection.columns(key)))
            })
            .collect();
        hidden.sort_unstable_by_key(|(seq, _, _)| *seq);
        shown_selection(context)
            .into_iter()
            .chain(hidden.into_iter().map(|(_, key, columns)| (key, columns)))
            .collect()
    }
}

/// A line the selection may take, as a [`SelectionPolicy`] sees it.
#[non_exhaustive]
pub enum SelectableLine<'a, R: GridRow> {
    /// A data row; `branch` is set for a tree row with shown children.
    Row {
        /// The row.
        row: &'a R,
        /// The row's children, in a tree.
        branch: Option<Branch>,
    },
    /// A group caption.
    Caption {
        /// The group, in the projection on screen.
        group: GroupInfo<'a>,
    },
}

/// What [`SelectionPolicy::line_scope`] knows: the target line of a
/// command in the projection on screen, the snapshot it is drawn from and
/// the selection mode.
pub struct LineScopeContext<'a, R: GridRow> {
    line: SelectableLine<'a, R>,
    /// The live rows under the line: a range of `Projection::rows`.
    under: Range<usize>,
    projection: &'a Projection,
    snapshot: &'a StoreSnapshot<R>,
    mode: SelectionMode,
}

impl<'a, R: GridRow> LineScopeContext<'a, R> {
    pub(crate) fn new(
        line: SelectableLine<'a, R>,
        under: Range<usize>,
        projection: &'a Projection,
        snapshot: &'a StoreSnapshot<R>,
        mode: SelectionMode,
    ) -> Self {
        Self {
            line,
            under,
            projection,
            snapshot,
            mode,
        }
    }

    /// The target line.
    pub fn line(&self) -> &SelectableLine<'a, R> {
        &self.line
    }

    /// The live rows under the line, in the order of `Projection::rows`:
    /// every row of a caption's group, its collapsed subgroups' too; the
    /// descendants of a tree row; none under any other row. Rows removed
    /// since the projection was built are left out.
    pub fn rows(&self) -> impl Iterator<Item = &'a Arc<R>> + 'a {
        let snapshot = self.snapshot;
        self.projection.rows()[self.under.clone()]
            .iter()
            .filter_map(move |id| snapshot.row(*id))
    }

    /// The rows of [`Self::rows`] as lines, each with its own children in
    /// a tree: what [`SelectionPolicy::is_selectable`] is asked about them.
    pub fn lines(&self) -> impl Iterator<Item = SelectableLine<'a, R>> + 'a {
        let (projection, snapshot) = (self.projection, self.snapshot);
        projection.rows()[self.under.clone()]
            .iter()
            .filter_map(move |id| {
                Some(SelectableLine::Row {
                    row: &**snapshot.row(*id)?,
                    branch: projection.branch(*id),
                })
            })
    }

    /// The projection on screen.
    pub fn projection(&self) -> &'a Projection {
        self.projection
    }

    /// The snapshot the screen shows.
    pub fn snapshot(&self) -> &'a StoreSnapshot<R> {
        self.snapshot
    }

    /// The selection mode.
    pub fn mode(&self) -> SelectionMode {
        self.mode
    }
}

/// Which lines can be selected, and what a command over a line acts on: a
/// group caption, or a tree row with children.
///
/// `is_selectable` is asked per line by Cmd/Ctrl+A, Shift ranges and
/// drags; `line_scope` once per command, for its target: the focused line
/// for Enter and Cmd/Ctrl+C, the menu's target, a double-clicked row. Both
/// run on the UI thread.
///
/// The default is [`FocusCaptions`]: tree rows are data rows and take the
/// selection, as in AG Grid's tree data and DevExpress TreeList. Built in
/// too, [`FocusBranches`]: tree rows with children take the focus but not
/// the selection, and a command over one acts on its subtree's leaves.
/// Another plausible one: selectable captions whose selection takes their
/// group's rows (AG Grid's `groupSelectsChildren`).
pub trait SelectionPolicy<R: GridRow>: 'static {
    /// Returns true if `line` can be selected. A selectable caption adds
    /// its group's live rows to the selection; a selectable row itself.
    fn is_selectable(&self, line: &SelectableLine<'_, R>) -> bool;

    /// The scope of a command whose target is `context.line()`; `None`:
    /// the command acts on `scoped_selection()`.
    fn line_scope(&self, context: &LineScopeContext<'_, R>) -> Option<ScopedSelection<R::Key>>;
}

/// Captions take the focus, never the selection; a command over a caption
/// acts on every live row of its group, collapsed subgroups included, in
/// the order of `Projection::rows`, with every column. Data rows, tree rows
/// with children included, are selectable, and a command over one acts on
/// `scoped_selection()`. The default.
#[derive(Clone, Copy, Debug, Default)]
pub struct FocusCaptions;

impl<R: GridRow> SelectionPolicy<R> for FocusCaptions {
    fn is_selectable(&self, line: &SelectableLine<'_, R>) -> bool {
        match line {
            SelectableLine::Row { .. } => true,
            SelectableLine::Caption { .. } => false,
        }
    }

    fn line_scope(&self, context: &LineScopeContext<'_, R>) -> Option<ScopedSelection<R::Key>> {
        match context.line() {
            SelectableLine::Caption { .. } => {
                let columns = ColumnSet::first(R::Column::all().len());
                Some(
                    context
                        .rows()
                        .map(|row| (row.key().clone(), columns))
                        .collect(),
                )
            }
            SelectableLine::Row { .. } => None,
        }
    }
}

/// Branches as captions: a tree row with shown children (a branch) takes
/// the focus but not the selection, as a group caption does, and a command
/// over one acts on the leaves of its subtree that pass the view, those
/// under collapsed branches too, in the order of `Projection::rows`, with
/// every column. Cmd/Ctrl+A, Shift ranges and drags leave branches out.
/// Group captions and leaves behave as with [`FocusCaptions`].
///
/// A leaf that gets children stays selected (by key); new gestures leave
/// it out.
#[derive(Clone, Copy, Debug, Default)]
pub struct FocusBranches;

impl<R: GridRow> SelectionPolicy<R> for FocusBranches {
    fn is_selectable(&self, line: &SelectableLine<'_, R>) -> bool {
        match line {
            SelectableLine::Row { branch, .. } => branch.is_none(),
            SelectableLine::Caption { .. } => false,
        }
    }

    fn line_scope(&self, context: &LineScopeContext<'_, R>) -> Option<ScopedSelection<R::Key>> {
        match context.line() {
            SelectableLine::Row {
                branch: Some(_), ..
            } => {
                let columns = ColumnSet::first(R::Column::all().len());
                Some(
                    context
                        .lines()
                        .filter_map(|line| match line {
                            SelectableLine::Row { row, branch: None } => {
                                Some((row.key().clone(), columns))
                            }
                            _ => None,
                        })
                        .collect(),
                )
            }
            _ => FocusCaptions.line_scope(context),
        }
    }
}

/// Where a selection gesture comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GestureInput {
    /// A navigation Action.
    Keyboard,
    /// A primary-button press.
    Pointer,
    /// A press that opens the context menu ([`ContextMenuGesture`]), before
    /// the menu.
    ContextPointer,
}

/// How a gesture changes the selection; the target always gets the focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectionEdit {
    /// Select only the target; it becomes the anchor.
    Replace,
    /// Select the range from the anchor to the target, dropping the rest.
    Extend,
    /// Add the range from the anchor to the target to the selection.
    AddRange,
    /// Flip the target; it becomes the anchor.
    Toggle,
    /// Leave the selection and the anchor as they are.
    Keep,
}

/// What a [`SelectionGesture`] knows about a press or a navigation command.
#[derive(Clone, Copy, Debug)]
pub struct SelectionGestureContext {
    input: GestureInput,
    modifiers: Modifiers,
    extending: bool,
    target_selected: bool,
    mode: SelectionMode,
}

impl SelectionGestureContext {
    pub(crate) fn new(
        input: GestureInput,
        modifiers: Modifiers,
        extending: bool,
        target_selected: bool,
        mode: SelectionMode,
    ) -> Self {
        Self {
            input,
            modifiers,
            extending,
            target_selected,
            mode,
        }
    }

    /// Where the gesture comes from.
    pub fn input(&self) -> GestureInput {
        self.input
    }

    /// The pointer's modifiers; none for keyboard input.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Returns true for a keyboard `ExtendSelection…` command.
    pub fn is_extending(&self) -> bool {
        self.extending
    }

    /// Returns true if the target row (in the cell modes, cell) is selected
    /// now.
    pub fn is_target_selected(&self) -> bool {
        self.target_selected
    }

    /// The selection mode.
    pub fn mode(&self) -> SelectionMode {
        self.mode
    }
}

/// How a press or a navigation command edits the selection. Called once
/// per gesture on the UI thread. Space toggles and Cmd/Ctrl+A selects all
/// whatever it says.
///
/// The default is [`StandardSelectionGesture`], the Windows and macOS
/// conventions with Excel's "navigation selects". Others are plausible: as
/// AG Grid (navigation only moves the focus, `Keep`, and Space selects), as
/// Finder (Shift adds a range), or touch (a press toggles).
pub trait SelectionGesture: 'static {
    /// The edit the gesture makes.
    fn edit(&self, context: &SelectionGestureContext) -> SelectionEdit;
}

/// Windows and macOS conventions; navigation selects, as in Excel.
///
/// A press replaces the selection; Shift extends it from the anchor;
/// Cmd on macOS, Ctrl elsewhere, toggles; both add a range. A context press
/// selects an unselected row and keeps a selected one. A navigation command
/// replaces; its `ExtendSelection…` variant extends.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardSelectionGesture;

impl SelectionGesture for StandardSelectionGesture {
    fn edit(&self, context: &SelectionGestureContext) -> SelectionEdit {
        let modifiers = context.modifiers();
        match context.input() {
            GestureInput::Pointer => match (modifiers.shift, modifiers.secondary()) {
                (true, true) => SelectionEdit::AddRange,
                (true, false) => SelectionEdit::Extend,
                (false, true) => SelectionEdit::Toggle,
                (false, false) => SelectionEdit::Replace,
            },
            GestureInput::ContextPointer if context.is_target_selected() => SelectionEdit::Keep,
            GestureInput::ContextPointer => SelectionEdit::Replace,
            GestureInput::Keyboard if context.is_extending() => SelectionEdit::Extend,
            GestureInput::Keyboard => SelectionEdit::Replace,
        }
    }
}

/// The part of the grid a press lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GridArea {
    /// The rows and the empty area below them.
    Body,
    /// The column header: its cells, bands and resize handles, and the
    /// header right of the columns.
    Header,
}

/// What a [`ContextMenuGesture`] knows about a press.
#[derive(Clone, Copy, Debug)]
pub struct ContextMenuGestureContext {
    button: MouseButton,
    modifiers: Modifiers,
    click_count: usize,
    area: GridArea,
}

impl ContextMenuGestureContext {
    pub(crate) fn new(
        button: MouseButton,
        modifiers: Modifiers,
        click_count: usize,
        area: GridArea,
    ) -> Self {
        Self {
            button,
            modifiers,
            click_count,
            area,
        }
    }

    /// The pressed button.
    pub fn button(&self) -> MouseButton {
        self.button
    }

    /// The modifier keys held during the press.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// The press's place in a click series: 1 for a single press, 2 for the
    /// second press of a double click.
    pub fn click_count(&self) -> usize {
        self.click_count
    }

    /// Where the press lands.
    pub fn area(&self) -> GridArea {
        self.area
    }
}

/// Which press opens the context menu. Called once per press on the UI
/// thread, on the body and on the header; the `ShowContextMenu` Action
/// opens the menu whatever it says.
///
/// On the body, a press that opens the menu first edits the selection as
/// [`GestureInput::ContextPointer`] through the [`SelectionGesture`]; any
/// other primary-button press is a [`GestureInput::Pointer`] press. On the
/// header it opens the column's menu instead of sorting or resizing.
///
/// The default is [`StandardContextMenuGesture`]. Others are plausible: the
/// right button only, so a Ctrl+click on macOS is a plain press; Ctrl+click
/// on every platform; no pointer menu at all, only the keyboard's.
pub trait ContextMenuGesture: 'static {
    /// Returns true if the press opens the context menu.
    fn opens_menu(&self, context: &ContextMenuGestureContext) -> bool;
}

/// The platform conventions: a right click, and on macOS a Ctrl+click
/// without Cmd, open the menu, on the body and on the header alike. On
/// macOS Cmd+click toggles the selection, so Ctrl+click is free.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardContextMenuGesture;

impl ContextMenuGesture for StandardContextMenuGesture {
    fn opens_menu(&self, context: &ContextMenuGestureContext) -> bool {
        let (button, modifiers) = (context.button(), context.modifiers());
        button == MouseButton::Right
            || (cfg!(target_os = "macos")
                && button == MouseButton::Left
                && modifiers.control
                && !modifiers.platform)
    }
}

/// Where a press lands on a line that expands: a group caption or a tree
/// row with children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpandArea {
    /// The disclosure chevron of a caption or a tree row.
    Chevron,
    /// A caption outside its chevron: its text or an aggregate cell.
    Caption,
    /// The tree column's cell of a tree row, outside its chevron.
    TreeCell,
}

/// How a press expands or collapses its line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpandEdit {
    /// The line takes the other state.
    Toggle,
    /// The line takes the other state, and every line below it the same.
    ToggleDeep,
}

/// What an [`ExpandGesture`] knows about a press.
#[derive(Clone, Copy, Debug)]
pub struct ExpandGestureContext {
    area: ExpandArea,
    button: MouseButton,
    modifiers: Modifiers,
    click_count: usize,
}

impl ExpandGestureContext {
    pub(crate) fn new(
        area: ExpandArea,
        button: MouseButton,
        modifiers: Modifiers,
        click_count: usize,
    ) -> Self {
        Self {
            area,
            button,
            modifiers,
            click_count,
        }
    }

    /// Where the press lands.
    pub fn area(&self) -> ExpandArea {
        self.area
    }

    /// The pressed button.
    pub fn button(&self) -> MouseButton {
        self.button
    }

    /// The modifier keys held during the press.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// The press's place in a click series: 1 for a single press, 2 for the
    /// second press of a double click.
    pub fn click_count(&self) -> usize {
        self.click_count
    }
}

/// Which press expands or collapses a group caption or a tree row. Called
/// once per press on such a line, on the UI thread, after the
/// [`ContextMenuGesture`]. A press on a caption still focuses it; a press
/// on a tree row's chevron leaves the focus and the selection as they are,
/// as Finder's disclosure triangle does; a press on a chevron is never a
/// click. The keys (`ExpandRow`, `CollapseRow`, ←/→ on a caption) expand
/// whatever it says.
///
/// The default is [`StandardExpandGesture`]. Others are plausible: a single
/// press on a caption toggles (Syncfusion); a double click on a caption
/// confirms instead; no double click at all.
pub trait ExpandGesture: 'static {
    /// The edit the press makes, if any.
    fn edit(&self, context: &ExpandGestureContext) -> Option<ExpandEdit>;
}

/// A press on a chevron toggles, with Alt (Option on macOS) the line and
/// everything below it, as Finder's disclosure triangle; only the first
/// press of a series counts. The second press of a double click on a
/// caption toggles it (DevExpress, Windows Explorer). A press on a tree
/// row's cell does nothing here: a double click confirms, as on any row.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardExpandGesture;

impl ExpandGesture for StandardExpandGesture {
    fn edit(&self, context: &ExpandGestureContext) -> Option<ExpandEdit> {
        if context.button() != MouseButton::Left {
            return None;
        }
        match (context.area(), context.click_count()) {
            (ExpandArea::Chevron, 1) if context.modifiers().alt => Some(ExpandEdit::ToggleDeep),
            (ExpandArea::Chevron, 1) | (ExpandArea::Caption, 2) => Some(ExpandEdit::Toggle),
            _ => None,
        }
    }
}

// The edit seams: which input opens a cell editor, and how an edit behaves
// under live data.

/// What an [`EditGesture`] knows about an input on a cell that could edit
/// it: a cell of a column with an editor that the delegate's
/// `is_editable` lets edit.
#[derive(Clone, Debug)]
pub struct EditGestureContext {
    input: EditInput,
    modifiers: Modifiers,
    content: CellContent,
}

impl EditGestureContext {
    pub(crate) fn new(input: EditInput, modifiers: Modifiers, content: CellContent) -> Self {
        Self {
            input,
            modifiers,
            content,
        }
    }

    /// The input: typing, a press, a command.
    pub fn input(&self) -> &EditInput {
        &self.input
    }

    /// The modifiers held.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// What the cell's column paints: a press on a `Control` cell is the
    /// element's own.
    pub fn content(&self) -> CellContent {
        self.content
    }
}

/// The Action that committed an editor: which key, as the editor's key
/// bindings name it (`CommitEdit`, `CommitEditUp`, `CommitEditDown`,
/// `CommitEditNext`, `CommitEditPrev`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditCommitKey {
    /// Enter (`CommitEdit`).
    Enter,
    /// Shift+Enter or ↑ (`CommitEditUp`).
    Up,
    /// ↓ (`CommitEditDown`).
    Down,
    /// Tab (`CommitEditNext`).
    Next,
    /// Shift+Tab (`CommitEditPrev`).
    Prev,
}

/// Where the focus goes after a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AfterCommit {
    /// The committed cell keeps the focus.
    Stay,
    /// The focus moves.
    Move(CellMove),
    /// The focus moves and the editor opens there, if that cell takes an
    /// edit.
    MoveAndEdit(CellMove),
}

/// A move of the focus after a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CellMove {
    /// To the row above.
    Up,
    /// To the row below.
    Down,
    /// To the visible column on the left.
    Left,
    /// To the visible column on the right.
    Right,
    /// To the next cell that takes an edit, left to right, then down.
    NextEditable,
    /// To the previous cell that takes an edit, right to left, then up.
    PrevEditable,
}

/// Which input opens the editor of an editable cell, and where the focus
/// goes after a commit. Asked once per input on the UI thread.
///
/// The default is [`StandardEditGesture`], Excel's. An AG Grid-like gesture
/// opens the editor on a single click, stays on Enter and edits the next
/// editable cell on Tab (`MoveAndEdit(CellMove::NextEditable)`).
pub trait EditGesture: 'static {
    /// Returns true if `context.input()` opens the editor. `Command` (F2)
    /// and `Clear` (Backspace, Delete) are Actions: rebind their keys
    /// instead of refusing them here.
    fn starts_edit(&self, context: &EditGestureContext) -> bool;

    /// Where the focus goes after a commit by `key`.
    fn after_commit(&self, key: EditCommitKey) -> AfterCommit;
}

/// Excel's: typing and a double click edit (not on a `Control` cell,
/// whose element takes its own presses), a single click does not; Enter
/// commits and moves down, Shift+Enter and ↑ up, ↓ down, Tab right,
/// Shift+Tab left.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardEditGesture;

impl EditGesture for StandardEditGesture {
    fn starts_edit(&self, context: &EditGestureContext) -> bool {
        match context.input() {
            EditInput::Press { click_count } => {
                *click_count >= 2 && context.content() != CellContent::Control
            }
            _ => true,
        }
    }

    fn after_commit(&self, key: EditCommitKey) -> AfterCommit {
        AfterCommit::Move(match key {
            EditCommitKey::Enter | EditCommitKey::Down => CellMove::Down,
            EditCommitKey::Up => CellMove::Up,
            EditCommitKey::Next => CellMove::Right,
            EditCommitKey::Prev => CellMove::Left,
        })
    }
}

/// What an [`EditPolicy`] knows about the open edit.
#[derive(Clone, Copy, Debug)]
pub struct EditPolicyContext<'a> {
    base: &'a OwnedValue,
    current: &'a OwnedValue,
    draft: Option<&'a OwnedValue>,
    dirty: bool,
}

impl<'a> EditPolicyContext<'a> {
    pub(crate) fn new(
        base: &'a OwnedValue,
        current: &'a OwnedValue,
        draft: Option<&'a OwnedValue>,
        dirty: bool,
    ) -> Self {
        Self {
            base,
            current,
            draft,
            dirty,
        }
    }

    /// The cell's value when the edit started.
    pub fn base(&self) -> &'a OwnedValue {
        self.base
    }

    /// The cell's value in the store now.
    pub fn current(&self) -> &'a OwnedValue {
        self.current
    }

    /// The draft as a value of the column; `None` if it does not parse.
    pub fn draft(&self) -> Option<&'a OwnedValue> {
        self.draft
    }

    /// Returns true if the user changed the draft since the editor opened.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

/// How an edit that leaves the editor ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditEnd {
    /// Commit the draft.
    Commit,
    /// Close the editor without a commit.
    Cancel,
}

/// What happens to the draft when the edited cell's value changes in the
/// store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DraftAction {
    /// Keep the draft; the editor shows the conflict.
    Keep,
    /// Replace the draft with the new value.
    Replace,
    /// Close the editor without a commit.
    Cancel,
}

/// How an edit session behaves around its commit and under live data.
/// Asked once per event on the UI thread.
///
/// The default is [`StandardEditPolicy`].
pub trait EditPolicy: 'static {
    /// The focus left an open editor, or a press landed outside it.
    fn on_blur(&self, context: &EditPolicyContext<'_>) -> EditEnd;

    /// Returns true if a commit equal to the value the edit started from
    /// still emits `EditCommit`. Default: no, the editor closes with
    /// `EditCancel { reason: Unchanged }`.
    fn commit_unchanged(&self) -> bool {
        false
    }

    /// The edited cell's value changed in the store.
    fn on_value_change(&self, context: &EditPolicyContext<'_>) -> DraftAction;

    /// The edited row left the shown lines without a user action: a
    /// filter, a collapsed group.
    fn on_hidden(&self, context: &EditPolicyContext<'_>) -> EditEnd;

    /// How long an accepted value is shown while the store does not have it
    /// yet. Default: 5 s.
    fn settle_timeout(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// How long the error mark of a failed commit stays; `None` (the
    /// default): until the cell is edited again or
    /// `LiveGridState::clear_edit_error`.
    fn error_mark_timeout(&self) -> Option<Duration> {
        None
    }
}

/// As [`StandardEditPolicy`], but a draft the user has not touched follows
/// the store: a new value replaces it (`DraftAction::Replace`); a touched
/// draft is kept and shows the conflict.
#[derive(Clone, Copy, Debug, Default)]
pub struct FollowLiveValue;

impl EditPolicy for FollowLiveValue {
    fn on_blur(&self, context: &EditPolicyContext<'_>) -> EditEnd {
        StandardEditPolicy.on_blur(context)
    }

    fn on_value_change(&self, context: &EditPolicyContext<'_>) -> DraftAction {
        if context.is_dirty() {
            DraftAction::Keep
        } else {
            DraftAction::Replace
        }
    }

    fn on_hidden(&self, context: &EditPolicyContext<'_>) -> EditEnd {
        StandardEditPolicy.on_hidden(context)
    }
}

/// The default edit policy: leaving the editor commits a valid draft and
/// cancels an invalid one; a draft is kept when the store changes the
/// cell, and the conflict shows; an edited row that leaves the shown lines
/// cancels the edit; an accepted value waits 5 s for the store; an error
/// mark stays until the next edit.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardEditPolicy;

impl EditPolicy for StandardEditPolicy {
    fn on_blur(&self, context: &EditPolicyContext<'_>) -> EditEnd {
        if context.draft().is_some() {
            EditEnd::Commit
        } else {
            EditEnd::Cancel
        }
    }

    fn on_value_change(&self, _: &EditPolicyContext<'_>) -> DraftAction {
        DraftAction::Keep
    }

    fn on_hidden(&self, _: &EditPolicyContext<'_>) -> EditEnd {
        EditEnd::Cancel
    }
}

// Which presses drag rows.

/// A primary press on a data row, as a [`RowDragGesture`] sees it.
pub struct RowDragPressContext<'a, R: GridRow> {
    line: SelectableLine<'a, R>,
    column: R::Column,
    was_selected: bool,
    modifiers: Modifiers,
    mode: SelectionMode,
    click_count: usize,
}

impl<'a, R: GridRow> RowDragPressContext<'a, R> {
    pub(crate) fn new(
        line: SelectableLine<'a, R>,
        column: R::Column,
        was_selected: bool,
        modifiers: Modifiers,
        mode: SelectionMode,
        click_count: usize,
    ) -> Self {
        Self {
            line,
            column,
            was_selected,
            modifiers,
            mode,
            click_count,
        }
    }

    /// The pressed line: a data row, a tree row too. Group captions,
    /// chevrons, pinned rows and the empty area below the rows never drag.
    pub fn line(&self) -> &SelectableLine<'a, R> {
        &self.line
    }

    /// The pressed cell's column: a drag handle's, for a gesture that
    /// drags from one column only.
    pub fn column(&self) -> R::Column {
        self.column
    }

    /// Returns true if the pressed row was selected before the press.
    pub fn was_selected(&self) -> bool {
        self.was_selected
    }

    /// The modifiers held.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// The grid's selection mode.
    pub fn mode(&self) -> SelectionMode {
        self.mode
    }

    /// The press's place in a series: 2 for the second press of a double
    /// click.
    pub fn click_count(&self) -> usize {
        self.click_count
    }
}

/// Which presses on the rows may become a row drag. A press that may starts
/// no drag-selection; past [`Self::threshold`] the drag starts, carrying the
/// command scope of the pressed row. Asked once per press.
///
/// The default is [`NoRowDrag`]: rows do not drag. [`SelectedRowsDrag`] is
/// the built-in alternative, LiveGrid's two-phase gesture.
pub trait RowDragGesture<R: GridRow>: 'static {
    /// Returns true if `press` may become a row drag.
    fn is_drag_press(&self, press: &RowDragPressContext<'_, R>) -> bool;

    /// How far the pointer travels from the press before the drag starts.
    /// Default: 10 px.
    fn threshold(&self) -> Pixels {
        px(10.)
    }

    /// Returns false if no press ever drags: the grid then installs no drag
    /// listeners at all. Default: true.
    fn is_enabled(&self) -> bool {
        true
    }
}

/// No press drags rows. The default: without an application handling
/// `Drop`, a drag would only get in the way of the selection.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRowDrag;

impl<R: GridRow> RowDragGesture<R> for NoRowDrag {
    fn is_drag_press(&self, _: &RowDragPressContext<'_, R>) -> bool {
        false
    }

    fn is_enabled(&self) -> bool {
        false
    }
}

/// LiveGrid's two-phase gesture: the first press of a series, without Shift
/// and without the platform toggle (Cmd on macOS, Ctrl elsewhere), on a data
/// row selected before the press drags the selection once the pointer is
/// 10 px away. A press on an unselected row selects it and drag-selects as
/// before.
#[derive(Clone, Copy, Debug)]
pub struct SelectedRowsDrag {
    threshold: Pixels,
}

impl SelectedRowsDrag {
    /// The gesture with a 10 px threshold.
    pub fn new() -> Self {
        Self { threshold: px(10.) }
    }

    /// Sets how far the pointer travels before the drag starts, such as the
    /// system's 3 or 4 px.
    pub fn with_threshold(mut self, threshold: Pixels) -> Self {
        self.threshold = threshold;
        self
    }
}

impl Default for SelectedRowsDrag {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: GridRow> RowDragGesture<R> for SelectedRowsDrag {
    fn is_drag_press(&self, press: &RowDragPressContext<'_, R>) -> bool {
        let modifiers = press.modifiers();
        press.was_selected()
            && press.click_count() == 1
            && !modifiers.shift
            && !modifiers.secondary()
            && matches!(press.line(), SelectableLine::Row { .. })
    }

    fn threshold(&self) -> Pixels {
        self.threshold
    }
}

// Where drops land.

/// The line under a drag over the rows.
#[non_exhaustive]
pub enum DropLine<'a, R: GridRow> {
    /// A data row; `branch` is set for a tree row with shown children.
    Row {
        /// The row's key.
        key: &'a R::Key,
        /// The row's children, in a tree.
        branch: Option<Branch>,
    },
    /// A group caption.
    Caption(GroupInfo<'a>),
    /// Below the last row, a row fading out, or an empty grid.
    Empty,
}

/// A drag over the rows, as a [`RowDropPolicy`] sees it.
pub struct RowDropContext<'a, R: GridRow> {
    line: DropLine<'a, R>,
    offset: f32,
    rows: Option<&'a RowDrag<R::Key>>,
    value: Option<&'a dyn std::any::Any>,
    own: bool,
    modifiers: Modifiers,
    sorted: bool,
    grouped: bool,
    tree: bool,
    last_row: Option<&'a R::Key>,
    snapshot: &'a crate::store::StoreSnapshot<R>,
}

impl<'a, R: GridRow> RowDropContext<'a, R> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        line: DropLine<'a, R>,
        offset: f32,
        rows: Option<&'a RowDrag<R::Key>>,
        value: Option<&'a dyn std::any::Any>,
        own: bool,
        modifiers: Modifiers,
        (sorted, grouped, tree): (bool, bool, bool),
        last_row: Option<&'a R::Key>,
        snapshot: &'a crate::store::StoreSnapshot<R>,
    ) -> Self {
        Self {
            line,
            offset,
            rows,
            value,
            own,
            modifiers,
            sorted,
            grouped,
            tree,
            last_row,
            snapshot,
        }
    }

    /// The line under the pointer.
    pub fn line(&self) -> &DropLine<'a, R> {
        &self.line
    }

    /// Where the pointer is within the line's height: 0.0 at its top, 1.0
    /// at its bottom.
    pub fn offset(&self) -> f32 {
        self.offset
    }

    /// The dragged rows of a grid with this key type; `None` for a value
    /// of a type registered with `with_drop_payload`.
    pub fn rows(&self) -> Option<&'a RowDrag<R::Key>> {
        self.rows
    }

    /// The dragged value of a registered type `T`.
    pub fn payload<T: 'static>(&self) -> Option<&'a T> {
        self.value?.downcast_ref()
    }

    /// Returns true if the rows come from this grid.
    pub fn is_own(&self) -> bool {
        self.own
    }

    /// The modifiers held.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Returns true if the view is sorted: a place between rows would not
    /// hold.
    pub fn is_sorted(&self) -> bool {
        self.sorted
    }

    /// Returns true if the view is grouped.
    pub fn is_grouped(&self) -> bool {
        self.grouped
    }

    /// Returns true if the view is a tree.
    pub fn is_tree(&self) -> bool {
        self.tree
    }

    /// The last data row shown, for a drop below the rows; `None` in an
    /// empty grid.
    pub fn last_row(&self) -> Option<&'a R::Key> {
        self.last_row
    }

    /// A row of the snapshot shown.
    pub fn row(&self, key: &R::Key) -> Option<&'a Arc<R>> {
        self.snapshot.row(self.snapshot.row_id(key)?)
    }
}

/// Where a drag over the rows would land, or whether the grid refuses it.
/// Asked on every move of a drag over the rows and once more at the drop;
/// pinned bands, group footers and totals refuse without asking.
///
/// The default is [`NoRowDrop`]: the grid takes no drop. [`DropOnRows`]
/// and [`DropBetweenRows`] are built in.
pub trait RowDropPolicy<R: GridRow>: 'static {
    /// Where a drop at `drop` lands; `None` refuses it.
    fn place(&self, drop: &RowDropContext<'_, R>) -> Option<DropTarget<R::Key>>;

    /// Returns false if the grid takes no drop at all: it then installs no
    /// drop listener. Default: true.
    fn is_enabled(&self) -> bool {
        true
    }

    /// Returns true if a drop with `modifiers` held copies rather than
    /// moves: the grid then shows the copy cursor over a target. What a
    /// drop does stays the application's, which reads the modifiers of
    /// `LiveGridEvent::Drop`. Default: Option (Alt) or Ctrl, on every
    /// platform.
    fn is_copy(&self, modifiers: Modifiers) -> bool {
        modifiers.alt || modifiers.control
    }
}

/// The grid takes no drop. The default.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRowDrop;

impl<R: GridRow> RowDropPolicy<R> for NoRowDrop {
    fn place(&self, _: &RowDropContext<'_, R>) -> Option<DropTarget<R::Key>> {
        None
    }

    fn is_enabled(&self) -> bool {
        false
    }
}

/// Onto data rows (tree rows too), group captions and the empty area below
/// the rows; any source.
#[derive(Clone, Copy, Debug, Default)]
pub struct DropOnRows;

impl<R: GridRow> RowDropPolicy<R> for DropOnRows {
    fn place(&self, drop: &RowDropContext<'_, R>) -> Option<DropTarget<R::Key>> {
        Some(match drop.line() {
            DropLine::Row { key, .. } => DropTarget::Row {
                key: (*key).clone(),
                place: DropPlace::Onto,
            },
            DropLine::Caption(group) => DropTarget::Group(group.path().clone()),
            DropLine::Empty => DropTarget::Empty,
        })
    }
}

/// Before or after a data row by the half the pointer is in; in a tree the
/// top third is before, the middle onto, the bottom third after. Onto group
/// captions. Below the rows, after the last one; an empty grid takes
/// `Empty`. Refuses in a sorted flat view, where a place between rows would
/// not hold.
#[derive(Clone, Copy, Debug, Default)]
pub struct DropBetweenRows;

impl<R: GridRow> RowDropPolicy<R> for DropBetweenRows {
    fn place(&self, drop: &RowDropContext<'_, R>) -> Option<DropTarget<R::Key>> {
        if drop.is_sorted() && !drop.is_grouped() && !drop.is_tree() {
            return None;
        }
        let row = |key: &R::Key, place| DropTarget::Row {
            key: key.clone(),
            place,
        };
        Some(match drop.line() {
            DropLine::Row { key, .. } if drop.is_tree() => {
                let place = match drop.offset() {
                    offset if offset < 1. / 3. => DropPlace::Before,
                    offset if offset < 2. / 3. => DropPlace::Onto,
                    _ => DropPlace::After,
                };
                row(key, place)
            }
            DropLine::Row { key, .. } if drop.offset() < 0.5 => row(key, DropPlace::Before),
            DropLine::Row { key, .. } => row(key, DropPlace::After),
            DropLine::Caption(group) => DropTarget::Group(group.path().clone()),
            DropLine::Empty => match drop.last_row() {
                Some(key) => row(key, DropPlace::After),
                None => DropTarget::Empty,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::px;

    use super::*;

    fn ix(i: u16) -> ColumnIx {
        ColumnIx::new(i)
    }

    /// c0, band A [c1, band B [c2, c3], c4], c5, all visible.
    fn banded() -> (Vec<ColumnIx>, Vec<Box<[&'static str]>>) {
        let columns = (0..6).map(ix).collect();
        let bands: Vec<Box<[&'static str]>> = vec![
            Box::new([]),
            Box::new(["a"]),
            Box::new(["a", "b"]),
            Box::new(["a", "b"]),
            Box::new(["a"]),
            Box::new([]),
        ];
        (columns, bands)
    }

    #[test]
    fn test_banded_move_policy() {
        let (columns, bands) = banded();
        let visible = ColumnSet::first(6);
        let context = ColumnMoveContext::new(ColumnZone::Scrolling, &columns, visible, &bands);
        let allowed = |block: Range<usize>, gap: usize| BandedMove.is_allowed(&context, block, gap);

        // c2 moves only among the children of B.
        assert!(allowed(3..4, 2));
        assert!(allowed(2..3, 4));
        assert!(!allowed(2..3, 1), "out of B to the left");
        assert!(!allowed(2..3, 5), "out of B to the right");
        // c1 moves among the children of A: around B as a whole, never into it.
        assert!(allowed(1..2, 4));
        assert!(allowed(1..2, 5));
        assert!(!allowed(1..2, 3), "inside the sibling band B");
        assert!(!allowed(1..2, 0), "out of A");
        // B as a whole is a child of A.
        assert!(allowed(2..4, 1));
        assert!(allowed(2..4, 5));
        assert!(!allowed(2..4, 0));
        // A as a whole moves among the zone's top-level items.
        assert!(allowed(1..5, 0));
        assert!(allowed(1..5, 6));
        // c0 cannot land inside A.
        assert!(allowed(0..1, 5));
        assert!(allowed(0..1, 6));
        assert!(!allowed(0..1, 2));
        assert!(!allowed(0..1, 4));

        // A zone change takes the whole top-level band.
        assert_eq!(BandedMove.zone_block(&context, ix(2)), 1..5);
        assert_eq!(BandedMove.zone_block(&context, ix(4)), 1..5);
        assert_eq!(BandedMove.zone_block(&context, ix(0)), 0..1);
        assert_eq!(BandedMove.zone_block(&context, ix(9)), 0..0);

        // Free moves: every gap of the zone, one column at a time.
        assert!(FreeMove.is_allowed(&context, 0..1, 2));
        assert!(FreeMove.is_allowed(&context, 2..3, 6));
        assert!(!FreeMove.is_allowed(&context, 2..3, 7));
        assert_eq!(FreeMove.zone_block(&context, ix(2)), 2..3);

        // `normalize` gathers each band at the place of its first column,
        // nested bands too, in the zone of its first column.
        let mut zones = vec![ColumnZone::Scrolling; 6];
        zones[4] = ColumnZone::FrozenRight;
        let mut order = vec![ix(2), ix(0), ix(1), ix(3), ix(5), ix(4)];
        BandedMove.normalize(&mut ColumnOrder::new(&mut order, &mut zones, &bands));
        assert_eq!(order, [2, 3, 1, 4, 0, 5].map(ix));
        assert_eq!(zones[4], ColumnZone::Scrolling);
        // A valid order stays as it is.
        let mut order = vec![ix(5), ix(1), ix(3), ix(2), ix(4), ix(0)];
        let before = order.clone();
        BandedMove.normalize(&mut ColumnOrder::new(&mut order, &mut zones, &bands));
        assert_eq!(order, before);
        // Free moves keep any order.
        let mut order = vec![ix(2), ix(0), ix(1), ix(3), ix(5), ix(4)];
        FreeMove.normalize(&mut ColumnOrder::new(&mut order, &mut zones, &bands));
        assert_eq!(order, [2, 0, 1, 3, 5, 4].map(ix));

        // `reorder` accepts permutations only.
        let mut order = (0..6).map(ix).collect::<Vec<_>>();
        let mut view = ColumnOrder::new(&mut order, &mut zones, &bands);
        assert!(!view.reorder(&[ix(0), ix(0), ix(1), ix(2), ix(3), ix(4)]));
        assert!(!view.reorder(&[ix(0)]));
        assert!(view.reorder(&[5, 4, 3, 2, 1, 0].map(ix)));
        assert_eq!(view.columns(), [5, 4, 3, 2, 1, 0].map(ix));
        assert_eq!(view.bands(ix(3)), ["a", "b"]);
    }

    #[test]
    fn test_visible_rows_autofit() {
        let mut measured = Vec::new();
        let mut measure = |display_ix: usize| {
            measured.push(display_ix);
            // Row 4 was removed; row 5 is the widest painted one.
            (display_ix != 4).then(|| px(display_ix as f32 * 10.))
        };
        let mut context = AutofitContext::new(px(25.), 3..6, 1_000, &mut measure);
        assert_eq!(context.row_count(), 1_000);
        assert_eq!(VisibleRowsAutofit.content_width(&mut context), px(50.));
        assert_eq!(measured, [3, 4, 5], "rows off screen are not measured");

        // The header is wider than every painted row.
        let mut measure = |_: usize| Some(px(10.));
        let mut context = AutofitContext::new(px(80.), 0..3, 3, &mut measure);
        assert_eq!(VisibleRowsAutofit.content_width(&mut context), px(80.));
        // An empty projection fits the header alone.
        let mut measure = |_: usize| None;
        let mut context = AutofitContext::new(px(42.), 0..0, 0, &mut measure);
        assert_eq!(context.pinned_width(), px(0.));
        assert_eq!(VisibleRowsAutofit.content_width(&mut context), px(42.));

        // A pinned row (a total) wider than the header and the data rows.
        let mut measure = |_: usize| Some(px(30.));
        let mut context =
            AutofitContext::new(px(20.), 0..3, 3, &mut measure).with_pinned_width(px(64.));
        assert_eq!(context.pinned_width(), px(64.));
        assert_eq!(VisibleRowsAutofit.content_width(&mut context), px(64.));

        // A total wider than everything else.
        let mut measure = |_: usize| Some(px(30.));
        let mut context = AutofitContext::new(px(20.), 0..3, 3, &mut measure)
            .with_pinned_width(px(64.))
            .with_summary_width(px(90.));
        assert_eq!(context.summary_width(), px(90.));
        assert_eq!(VisibleRowsAutofit.content_width(&mut context), px(90.));
    }

    #[test]
    fn test_standard_sort_cycle() {
        use SortDirection::{Ascending, Descending};
        let next = |sort: &[(ColumnIx, SortDirection)], column: u16, shift: bool| {
            let modifiers = if shift {
                Modifiers::shift()
            } else {
                Modifiers::default()
            };
            StandardSortCycle.next_sort(&SortCycleContext::new(sort, ix(column), modifiers))
        };

        // Without Shift: ascending, descending, unsorted.
        assert_eq!(next(&[], 1, false), [(ix(1), Ascending)]);
        assert_eq!(next(&[(ix(1), Ascending)], 1, false), [(ix(1), Descending)]);
        assert_eq!(next(&[(ix(1), Descending)], 1, false), []);
        // Another column replaces the sort.
        assert_eq!(next(&[(ix(1), Descending)], 2, false), [(ix(2), Ascending)]);
        // From a multi-column sort, the column steps from its own direction
        // and the others go (as AG Grid).
        let multi = [(ix(1), Ascending), (ix(2), Ascending), (ix(3), Descending)];
        assert_eq!(next(&multi, 2, false), [(ix(2), Descending)]);
        assert_eq!(next(&multi, 3, false), []);

        // Shift adds the column last.
        assert_eq!(
            next(&multi, 4, true),
            [
                (ix(1), Ascending),
                (ix(2), Ascending),
                (ix(3), Descending),
                (ix(4), Ascending)
            ]
        );
        // Shift on an ascending column flips it in place, keeping its priority.
        assert_eq!(
            next(&multi, 1, true),
            [(ix(1), Descending), (ix(2), Ascending), (ix(3), Descending)]
        );
        // Shift on a descending column removes it; the rest move up.
        assert_eq!(
            next(&[(ix(3), Descending), (ix(1), Ascending)], 3, true),
            [(ix(1), Ascending)]
        );
        assert_eq!(next(&[], 1, true), [(ix(1), Ascending)]);

        let context = SortCycleContext::new(&multi, ix(3), Modifiers::shift());
        assert_eq!(context.sort(), multi);
        assert_eq!(context.column(), ix(3));
        assert!(context.modifiers().shift);
        assert_eq!(context.direction(), Some(Descending));
    }

    #[test]
    fn test_reorder_policies() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let context =
            |now: u64, inside: bool, user: bool| ReorderContext::new(at(now), at(0), inside, user);

        assert_eq!(
            LiveReorder.decide(&context(0, true, false)),
            ReorderDecision::Apply
        );

        let hold = HoldReorder::new(Duration::from_millis(500));
        assert_eq!(
            hold.decide(&context(100, true, false)),
            ReorderDecision::HoldUntil(at(500))
        );
        assert_eq!(
            hold.decide(&context(500, true, false)),
            ReorderDecision::Apply
        );
        assert_eq!(
            hold.decide(&context(100, false, false)),
            ReorderDecision::Apply
        );
        assert_eq!(
            hold.decide(&context(100, true, true)),
            ReorderDecision::Apply
        );

        assert_eq!(
            ManualReorder.decide(&context(100, false, false)),
            ReorderDecision::Hold
        );
        assert_eq!(
            ManualReorder.decide(&context(100, true, true)),
            ReorderDecision::Apply
        );

        // An open menu holds the order under every built-in policy, except
        // for a user action.
        let menu =
            |now: u64, inside: bool, user: bool| context(now, inside, user).with_menu_open(true);
        let policies: [&dyn ReorderPolicy; 3] = [&LiveReorder, &hold, &ManualReorder];
        for policy in policies {
            for inside in [false, true] {
                assert_eq!(
                    policy.decide(&menu(100, inside, false)),
                    ReorderDecision::Hold
                );
                assert_eq!(
                    policy.decide(&menu(100, inside, true)),
                    ReorderDecision::Apply
                );
            }
        }
        // Past `max`, `HoldReorder` still holds while the menu is open, and
        // applies once it closes.
        assert_eq!(hold.decide(&menu(900, true, false)), ReorderDecision::Hold);
        assert_eq!(
            hold.decide(&context(900, true, false)),
            ReorderDecision::Apply
        );
        // A shown tooltip changes nothing for the built-in policies.
        let tooltip = |now: u64, inside: bool| context(now, inside, false).with_tooltip_shown(true);
        assert!(tooltip(0, true).is_tooltip_shown());
        assert!(!context(0, true, false).is_tooltip_shown());
        assert!(!context(0, true, false).is_menu_open());
        assert_eq!(
            LiveReorder.decide(&tooltip(100, true)),
            ReorderDecision::Apply
        );
        assert_eq!(
            hold.decide(&tooltip(100, true)),
            ReorderDecision::HoldUntil(at(500))
        );
        assert_eq!(hold.decide(&tooltip(100, false)), ReorderDecision::Apply);
        assert_eq!(
            ManualReorder.decide(&tooltip(100, false)),
            ReorderDecision::Hold
        );
    }

    #[test]
    fn test_standard_selection_gesture() {
        use SelectionEdit::*;
        let edit = |input, modifiers: Modifiers, extending, selected| {
            StandardSelectionGesture.edit(&SelectionGestureContext::new(
                input,
                modifiers,
                extending,
                selected,
                SelectionMode::RowSet,
            ))
        };
        let secondary = Modifiers::secondary_key();
        let shift_secondary = Modifiers {
            shift: true,
            ..secondary
        };
        // The standard gesture, case by case.
        let pointer = GestureInput::Pointer;
        assert_eq!(edit(pointer, Modifiers::default(), false, false), Replace);
        assert_eq!(edit(pointer, Modifiers::default(), false, true), Replace);
        assert_eq!(edit(pointer, Modifiers::shift(), false, false), Extend);
        assert_eq!(edit(pointer, secondary, false, true), Toggle);
        assert_eq!(edit(pointer, shift_secondary, false, false), AddRange);
        let context = GestureInput::ContextPointer;
        assert_eq!(edit(context, Modifiers::default(), false, false), Replace);
        assert_eq!(edit(context, Modifiers::default(), false, true), Keep);
        let keyboard = GestureInput::Keyboard;
        assert_eq!(edit(keyboard, Modifiers::default(), false, true), Replace);
        assert_eq!(edit(keyboard, Modifiers::default(), true, false), Extend);

        let context = SelectionGestureContext::new(
            keyboard,
            Modifiers::shift(),
            true,
            true,
            SelectionMode::CellSet,
        );
        assert_eq!(context.input(), keyboard);
        assert!(context.modifiers().shift);
        assert!(context.is_extending());
        assert!(context.is_target_selected());
        assert_eq!(context.mode(), SelectionMode::CellSet);
    }

    #[test]
    fn test_standard_context_menu_gesture() {
        let opens = |button, modifiers, click_count, area| {
            StandardContextMenuGesture.opens_menu(&ContextMenuGestureContext::new(
                button,
                modifiers,
                click_count,
                area,
            ))
        };
        let control = Modifiers::control();
        let control_command = Modifiers {
            platform: true,
            ..control
        };
        let macos = cfg!(target_os = "macos");
        for area in [GridArea::Body, GridArea::Header] {
            // The right button, whatever the modifiers and the series.
            for modifiers in [Modifiers::default(), Modifiers::shift(), control] {
                assert!(opens(MouseButton::Right, modifiers, 1, area));
            }
            assert!(opens(MouseButton::Right, Modifiers::default(), 2, area));
            // The left button: Ctrl without Cmd on macOS only. Cmd on macOS
            // and Ctrl elsewhere toggle the selection.
            assert!(!opens(MouseButton::Left, Modifiers::default(), 1, area));
            assert!(!opens(MouseButton::Left, Modifiers::shift(), 1, area));
            assert!(!opens(MouseButton::Left, Modifiers::command(), 1, area));
            assert_eq!(opens(MouseButton::Left, control, 1, area), macos);
            assert_eq!(opens(MouseButton::Left, control, 2, area), macos);
            assert!(!opens(MouseButton::Left, control_command, 1, area));
            // Other buttons never.
            assert!(!opens(MouseButton::Middle, Modifiers::default(), 1, area));
            assert!(!opens(MouseButton::Middle, control, 1, area));
        }

        let context =
            ContextMenuGestureContext::new(MouseButton::Middle, control, 2, GridArea::Header);
        assert_eq!(context.button(), MouseButton::Middle);
        assert!(context.modifiers().control);
        assert_eq!(context.click_count(), 2);
        assert_eq!(context.area(), GridArea::Header);
    }

    #[test]
    fn test_drop_policies() {
        use std::time::Instant;

        use crate::{
            highlight::HighlightPolicy,
            model::{CellValue, GridColumns},
            store::Ingest,
        };

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        struct Only;

        impl GridColumns for Only {
            fn all() -> &'static [Self] {
                &[Only]
            }

            fn id(self) -> &'static str {
                "only"
            }

            fn ix(self) -> ColumnIx {
                ColumnIx::new(0)
            }
        }

        struct Row(u32);

        impl GridRow for Row {
            type Key = u32;
            type Column = Only;

            fn key(&self) -> &u32 {
                &self.0
            }

            fn value(&self, _: Only) -> CellValue<'_> {
                CellValue::Empty
            }
        }

        // The copy cursor: Option or Ctrl by default; a policy may decide
        // otherwise.
        let held = |alt, control| Modifiers {
            alt,
            control,
            ..Modifiers::default()
        };
        assert!(RowDropPolicy::<Row>::is_copy(&NoRowDrop, held(true, false)));
        assert!(RowDropPolicy::<Row>::is_copy(&NoRowDrop, held(false, true)));
        assert!(!RowDropPolicy::<Row>::is_copy(
            &NoRowDrop,
            held(false, false)
        ));
        struct NeverCopy;
        impl RowDropPolicy<Row> for NeverCopy {
            fn place(&self, _: &RowDropContext<'_, Row>) -> Option<DropTarget<u32>> {
                None
            }

            fn is_copy(&self, _: Modifiers) -> bool {
                false
            }
        }
        assert!(!NeverCopy.is_copy(held(true, true)));

        let snapshot = Ingest::<Row>::new(HighlightPolicy::default(), Instant::now()).snapshot();
        let (key, last) = (3, 9);
        // A drag at `offset` over the row of `key` (`None`: below the rows)
        // in a view `(sorted, grouped, tree)`.
        let place = |policy: &dyn RowDropPolicy<Row>,
                     row: Option<f32>,
                     view: (bool, bool, bool),
                     last_row: Option<&u32>| {
            let line = match row {
                Some(_) => DropLine::Row {
                    key: &key,
                    branch: None,
                },
                None => DropLine::Empty,
            };
            policy.place(&RowDropContext::new(
                line,
                row.unwrap_or(0.),
                None,
                None,
                false,
                Modifiers::default(),
                view,
                last_row,
                &snapshot,
            ))
        };
        let at = |key, place| Some(DropTarget::Row { key, place });
        let flat = (false, false, false);
        assert!(!RowDropPolicy::<Row>::is_enabled(&NoRowDrop));
        assert_eq!(place(&NoRowDrop, Some(0.5), flat, None), None);
        assert_eq!(
            place(&DropOnRows, Some(0.1), flat, None),
            at(3, DropPlace::Onto)
        );
        assert_eq!(
            place(&DropOnRows, None, flat, None),
            Some(DropTarget::Empty)
        );
        // Between rows by the half, in a tree by the third.
        assert_eq!(
            place(&DropBetweenRows, Some(0.4), flat, None),
            at(3, DropPlace::Before)
        );
        assert_eq!(
            place(&DropBetweenRows, Some(0.6), flat, None),
            at(3, DropPlace::After)
        );
        let tree = (false, false, true);
        assert_eq!(
            place(&DropBetweenRows, Some(0.2), tree, None),
            at(3, DropPlace::Before)
        );
        assert_eq!(
            place(&DropBetweenRows, Some(0.5), tree, None),
            at(3, DropPlace::Onto)
        );
        assert_eq!(
            place(&DropBetweenRows, Some(0.8), tree, None),
            at(3, DropPlace::After)
        );
        // Below the rows: after the last one; an empty grid takes `Empty`.
        assert_eq!(
            place(&DropBetweenRows, None, flat, Some(&last)),
            at(9, DropPlace::After)
        );
        assert_eq!(
            place(&DropBetweenRows, None, flat, None),
            Some(DropTarget::Empty)
        );
        // Sorted: refused when flat, kept when grouped.
        assert_eq!(
            place(&DropBetweenRows, Some(0.6), (true, false, false), None),
            None
        );
        assert_eq!(
            place(&DropBetweenRows, Some(0.6), (true, true, false), None),
            at(3, DropPlace::After)
        );
    }

    #[test]
    fn test_selection_scopes() {
        use std::{sync::Arc, time::Instant};

        use crate::{
            engine::{ProjectionEngine as _, Rebuild, ViewSpec},
            highlight::HighlightPolicy,
            model::{CellValue, GridColumns},
            store::{Ingest, Op},
        };

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        struct Price;

        impl GridColumns for Price {
            fn all() -> &'static [Self] {
                &[Price]
            }

            fn id(self) -> &'static str {
                "price"
            }

            fn ix(self) -> ColumnIx {
                ColumnIx::new(0)
            }
        }

        struct Row(u32, i64);

        impl GridRow for Row {
            type Key = u32;
            type Column = Price;

            fn key(&self) -> &u32 {
                &self.0
            }

            fn value(&self, _: Price) -> CellValue<'_> {
                CellValue::Int(self.1)
            }
        }

        // Ten rows sorted by price descending (key 9 first); odd keys shown.
        let mut ingest = Ingest::new(HighlightPolicy::default(), Instant::now());
        ingest.apply(
            (0..10).map(|id| Op::Upsert(Arc::new(Row(id, id.into())))),
            Instant::now(),
        );
        let view = ViewSpec::new()
            .with_sort([(Price, SortDirection::Descending)])
            .with_filter(ColumnSet::first(1), |row: &Row| row.0 % 2 == 1);
        let projection = Rebuild::default().rebuild(&ingest.snapshot(), &view);
        // Removed after the projection was built: a tombstone on screen.
        ingest.apply([Op::Remove(5), Op::Remove(4)], Instant::now());
        let snapshot = ingest.snapshot();

        let row = ColumnSet::first(1);
        let resolve = |scope: &dyn SelectionScope<Row>, keys: &[u32]| {
            // Key 42 never existed; 4 and 5 are removed; 2 and 6 hidden.
            let selection = GridSelection::from_rows(keys.iter().map(|key| (*key, row)));
            let scoped = scope.resolve(&SelectionScopeContext::new(
                &selection,
                &projection,
                &snapshot,
                SelectionMode::RowSet,
            ));
            assert!((0..scoped.len()).all(|ix| scoped.columns(ix) == row));
            assert_eq!(scoped.columns(scoped.len()), ColumnSet::EMPTY);
            scoped.keys().to_vec()
        };
        let few = [1, 6, 2, 5, 9, 4, 42];
        let many: Vec<u32> = (0..10).chain([42]).collect();
        // The selection within the screen, in display order, both paths.
        assert_eq!(resolve(&ProjectionScope, &few), [9, 1]);
        assert_eq!(resolve(&ProjectionScope, &many), [9, 7, 3, 1]);
        assert!(resolve(&ProjectionScope, &[]).is_empty());
        // Hidden rows after the shown ones, by insertion; removed never.
        assert_eq!(resolve(&StoreScope, &few), [9, 1, 2, 6]);
        assert_eq!(resolve(&StoreScope, &many), [9, 7, 3, 1, 0, 2, 6, 8]);

        let empty = GridSelection::default();
        let context =
            SelectionScopeContext::new(&empty, &projection, &snapshot, SelectionMode::CellSet);
        assert_eq!(context.mode(), SelectionMode::CellSet);
        assert_eq!(context.projection().len(), 5);
        assert_eq!(context.snapshot().len(), 8);
        assert!(context.selection().is_empty());
        // A scope is collected from (key, columns) pairs.
        let scoped: ScopedSelection<u32> = [(3, row)].into_iter().collect();
        assert_eq!((scoped.len(), scoped.is_empty()), (1, false));
    }

    #[test]
    fn test_standard_expand_gesture() {
        let edit = |area, button, modifiers, click_count| {
            StandardExpandGesture.edit(&ExpandGestureContext::new(
                area,
                button,
                modifiers,
                click_count,
            ))
        };
        let (left, plain) = (MouseButton::Left, Modifiers::default());
        let alt = Modifiers::alt();
        // A chevron toggles on the first press of a series; with Alt, deep.
        assert_eq!(
            edit(ExpandArea::Chevron, left, plain, 1),
            Some(ExpandEdit::Toggle)
        );
        assert_eq!(
            edit(ExpandArea::Chevron, left, alt, 1),
            Some(ExpandEdit::ToggleDeep)
        );
        assert_eq!(edit(ExpandArea::Chevron, left, plain, 2), None);
        // A caption toggles on the second press of a double click.
        assert_eq!(edit(ExpandArea::Caption, left, plain, 1), None);
        assert_eq!(
            edit(ExpandArea::Caption, left, plain, 2),
            Some(ExpandEdit::Toggle)
        );
        assert_eq!(edit(ExpandArea::Caption, left, plain, 3), None);
        // A tree cell never: a double click confirms its row.
        assert_eq!(edit(ExpandArea::TreeCell, left, plain, 1), None);
        assert_eq!(edit(ExpandArea::TreeCell, left, plain, 2), None);
        // Other buttons never.
        assert_eq!(
            edit(ExpandArea::Chevron, MouseButton::Right, plain, 1),
            None
        );
        assert_eq!(
            edit(ExpandArea::Chevron, MouseButton::Middle, plain, 1),
            None
        );

        let context = ExpandGestureContext::new(ExpandArea::TreeCell, left, alt, 2);
        assert_eq!(context.area(), ExpandArea::TreeCell);
        assert_eq!(context.button(), left);
        assert!(context.modifiers().alt);
        assert_eq!(context.click_count(), 2);
    }

    #[test]
    fn test_focus_captions() {
        use std::{sync::Arc, time::Instant};

        use crate::{
            engine::{GroupBy, ProjectionEngine as _, Rebuild, ViewSpec},
            highlight::HighlightPolicy,
            model::{CellValue, GridColumns},
            store::{Ingest, Op},
        };

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        enum Col {
            Side,
            Qty,
        }

        impl GridColumns for Col {
            fn all() -> &'static [Self] {
                &[Col::Side, Col::Qty]
            }

            fn id(self) -> &'static str {
                match self {
                    Col::Side => "side",
                    Col::Qty => "qty",
                }
            }

            fn ix(self) -> ColumnIx {
                ColumnIx::new(self as u16)
            }
        }

        struct Row(u32, &'static str);

        impl GridRow for Row {
            type Key = u32;
            type Column = Col;

            fn key(&self) -> &u32 {
                &self.0
            }

            fn value(&self, column: Col) -> CellValue<'_> {
                match column {
                    Col::Side => CellValue::Text(self.1),
                    Col::Qty => CellValue::Int(self.0.into()),
                }
            }
        }

        // Six rows in two groups, Buy (even keys) and Sell (odd keys).
        let mut ingest = Ingest::new(HighlightPolicy::default(), Instant::now());
        ingest.apply(
            (0..6).map(|id| {
                let side = if id % 2 == 0 { "Buy" } else { "Sell" };
                Op::Upsert(Arc::new(Row(id, side)))
            }),
            Instant::now(),
        );
        let view = ViewSpec::new()
            .with_sort([(Col::Qty, SortDirection::Descending)])
            .with_group_by([GroupBy::column(Col::Side)]);
        let projection = Rebuild::default().rebuild(&ingest.snapshot(), &view);
        // Removed after the projection was built: left out of the scope.
        ingest.apply([Op::Remove(2)], Instant::now());
        let snapshot = ingest.snapshot();
        let buy = projection.groups().iter().next().expect("the Buy group");
        let rows = projection.order_ix(buy.rows()[0]).expect("in the order");

        let policy: &dyn SelectionPolicy<Row> = &FocusCaptions;
        let caption = SelectableLine::Caption { group: buy };
        assert!(!policy.is_selectable(&caption));
        let context = LineScopeContext::new(
            caption,
            rows..rows + buy.rows().len(),
            &projection,
            &snapshot,
            SelectionMode::CellSet,
        );
        assert_eq!(context.mode(), SelectionMode::CellSet);
        assert_eq!(context.projection().len(), projection.len());
        assert_eq!(context.snapshot().len(), 5);
        // Every live row of the group in order, with every column, in the
        // cell modes too.
        let scope = policy.line_scope(&context).expect("a caption's scope");
        assert_eq!(scope.keys(), [4, 0]);
        assert_eq!(scope.columns(0), ColumnSet::first(2));

        // A row is selectable, and a command over it acts on the selection.
        let row = snapshot.get(&3).expect("row 3");
        let line = SelectableLine::Row {
            row: &**row,
            branch: None,
        };
        assert!(policy.is_selectable(&line));
        let context =
            LineScopeContext::new(line, 0..0, &projection, &snapshot, SelectionMode::RowSet);
        assert_eq!(context.rows().count(), 0);
        assert!(policy.line_scope(&context).is_none());
    }

    #[test]
    fn test_focus_branches_policy() {
        use std::{sync::Arc, time::Instant};

        use crate::{
            engine::{ExpandDepth, GroupBy, ProjectionEngine as _, Rebuild, TreeSpec, ViewSpec},
            highlight::HighlightPolicy,
            model::{CellValue, GridColumns},
            store::{Ingest, Op},
        };

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        enum Col {
            Id,
            Kind,
        }

        impl GridColumns for Col {
            fn all() -> &'static [Self] {
                &[Col::Id, Col::Kind]
            }

            fn id(self) -> &'static str {
                match self {
                    Col::Id => "id",
                    Col::Kind => "kind",
                }
            }

            fn ix(self) -> ColumnIx {
                ColumnIx::new(self as u16)
            }
        }

        struct Node {
            id: u32,
            parent: Option<u32>,
        }

        impl GridRow for Node {
            type Key = u32;
            type Column = Col;

            fn key(&self) -> &u32 {
                &self.id
            }

            fn value(&self, column: Col) -> CellValue<'_> {
                match column {
                    Col::Id => CellValue::Int(self.id.into()),
                    Col::Kind => CellValue::Text(if self.id % 2 == 0 { "even" } else { "odd" }),
                }
            }

            fn parent_key(&self) -> Option<&u32> {
                self.parent.as_ref()
            }
        }

        // 0 ─┬─ 1 ─┬─ 3
        //    │     └─ 4
        //    ├─ 2 ─── 5   (2 collapsed)
        //    └─ 6
        // 7
        let parent = |id| match id {
            1 | 2 | 6 => Some(0),
            3 | 4 => Some(1),
            5 => Some(2),
            _ => None,
        };
        let mut ingest = Ingest::new(HighlightPolicy::default(), Instant::now());
        ingest.apply(
            (0..8).map(|id| {
                Op::Upsert(Arc::new(Node {
                    id,
                    parent: parent(id),
                }))
            }),
            Instant::now(),
        );
        let view = ViewSpec::new()
            .with_sort([(Col::Id, SortDirection::Ascending)])
            .with_tree(TreeSpec::new())
            .with_tree_expansion(ExpandDepth::All, [2]);
        let projection = Rebuild::default().rebuild(&ingest.snapshot(), &view);
        let snapshot = ingest.snapshot();
        let order: Vec<u32> = projection
            .rows()
            .iter()
            .map(|id| snapshot.row(*id).expect("live").id)
            .collect();
        assert_eq!(order, [0, 1, 3, 4, 2, 5, 6, 7]);

        let policy: &dyn SelectionPolicy<Node> = &FocusBranches;
        let line = |key: u32| {
            let id = snapshot.row_id(&key).expect("a row");
            SelectableLine::Row {
                row: &**snapshot.row(id).expect("live"),
                branch: projection.branch(id),
            }
        };
        let scope = |key: u32, snapshot: &StoreSnapshot<Node>| {
            let id = snapshot.row_id(&key).expect("a row");
            let order_ix = projection.order_ix(id).expect("in the order");
            let under = projection
                .branch(id)
                .map_or(0, |branch| branch.descendant_count());
            let context = LineScopeContext::new(
                line(key),
                order_ix + 1..order_ix + 1 + under,
                &projection,
                snapshot,
                SelectionMode::CellSet,
            );
            policy.line_scope(&context)
        };
        // A branch is not selectable, a leaf is: 2 too, though collapsed.
        for (key, selectable) in [(0, false), (1, false), (2, false), (3, true), (6, true)] {
            assert_eq!(policy.is_selectable(&line(key)), selectable, "{key}");
        }
        assert!(FocusCaptions.is_selectable(&line(0)), "a node is a row");
        // A command over a branch: the leaves of its subtree in the order of
        // `rows()`, the one under the collapsed branch 2 too, without the
        // branches themselves; every column, in the cell modes too.
        let all = scope(0, &snapshot).expect("a branch's scope");
        assert_eq!(all.keys(), [3, 4, 5, 6]);
        assert_eq!(all.columns(0), ColumnSet::first(2));
        assert_eq!(scope(1, &snapshot).expect("a branch").keys(), [3, 4]);
        assert_eq!(scope(2, &snapshot).expect("a branch").keys(), [5]);
        // Over a leaf, the scoped selection.
        assert!(scope(3, &snapshot).is_none());
        assert!(scope(7, &snapshot).is_none());
        // A row removed since the projection was built is left out.
        ingest.apply([Op::Remove(4)], Instant::now());
        let after = ingest.snapshot();
        assert_eq!(scope(1, &after).expect("a branch").keys(), [3]);

        // Group captions as with `FocusCaptions`: focus, no selection, the
        // group's rows.
        let grouped = Rebuild::default().rebuild(
            &snapshot,
            &ViewSpec::new()
                .with_sort([(Col::Id, SortDirection::Ascending)])
                .with_group_by([GroupBy::column(Col::Kind)]),
        );
        let even = grouped.groups().iter().next().expect("the even group");
        let start = grouped.order_ix(even.rows()[0]).expect("in the order");
        let caption = SelectableLine::Caption { group: even };
        assert!(!policy.is_selectable(&caption));
        let context = LineScopeContext::new(
            caption,
            start..start + even.rows().len(),
            &grouped,
            &snapshot,
            SelectionMode::RowSet,
        );
        assert_eq!(
            policy
                .line_scope(&context)
                .expect("a caption's scope")
                .keys(),
            [0, 2, 4, 6]
        );
        assert_eq!(context.lines().count(), 4);
    }
}
