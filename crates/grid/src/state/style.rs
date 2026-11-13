//! Cell styling: the delegate hooks, what they are told about a cell, and
//! the theme palette the body is planned with.

use gpui_kit::component::menu::PopupMenu;
use gpui_kit::{AnyElement, App, Context, FontWeight, Hsla, SharedString, Window};

use super::{
    drag::{DragPayload, RowDragContext},
    edit::{EditError, EditMessageContext, EditState, EditValidateContext, EditableContext},
    groups::{CaptionTag, GroupCaptionContext},
    menu::GridMenuContext,
    progress::ProgressContext,
    tooltip::{CellTooltip, CellTooltipContext, HeaderTooltipContext},
    widgets::CellElementContext,
};
use crate::{
    engine::GroupInfo,
    highlight::{ChangeDirection, RowChange},
    model::{CellValue, ColumnIx, GridRow},
    store::{GridProgress, PinnedEdge},
};

/// Style of one cell, filled by the grid's change paint
/// ([`ChangePaint`](crate::ChangePaint)) and then by
/// [`LiveGridDelegate::cell_style`]. Unset fields use the theme.
#[derive(Clone, Copy, Debug, Default)]
pub struct CellStyle {
    pub(super) background: Option<Hsla>,
    pub(super) foreground: Option<Hsla>,
    pub(super) font_weight: Option<FontWeight>,
    pub(super) trend: Option<ChangeDirection>,
    pub(super) error_mark: bool,
}

impl CellStyle {
    /// Sets the background.
    pub fn set_background(&mut self, color: Hsla) {
        self.background = Some(color);
    }

    /// Sets the text colour.
    pub fn set_foreground(&mut self, color: Hsla) {
        self.foreground = Some(color);
    }

    /// The background, if set.
    pub fn background(&self) -> Option<Hsla> {
        self.background
    }

    /// The text colour, if set.
    pub fn foreground(&self) -> Option<Hsla> {
        self.foreground
    }

    /// Sets the weight of the cell's text, such as `FontWeight::BOLD` for a
    /// total. Each weight is cached apart, so a bold cell does not make
    /// other cells shape their text again.
    pub fn set_font_weight(&mut self, weight: FontWeight) {
        self.font_weight = Some(weight);
    }

    /// The weight of the cell's text; `None` is the grid's font.
    pub fn font_weight(&self) -> Option<FontWeight> {
        self.font_weight
    }

    /// Shows an arrow for the direction of the last change, or none.
    /// [`DirectionalChanges`](crate::DirectionalChanges) sets it for a hot
    /// cell that rose or fell; a style may set or clear it. The arrow takes
    /// a lane on the side opposite the text's alignment: the text does not
    /// move, it only has less room.
    pub fn set_trend(&mut self, trend: Option<ChangeDirection>) {
        self.trend = trend;
    }

    /// The direction the cell shows an arrow for.
    pub fn trend(&self) -> Option<ChangeDirection> {
        self.trend
    }

    /// Shows or hides the error mark of a refused edit: an icon in the trend
    /// arrow's lane, which it takes from the arrow. The grid sets it on a
    /// cell whose commit failed ([`EditState::Failed`]); `false` hides it.
    pub fn set_error_mark(&mut self, shown: bool) {
        self.error_mark = shown;
    }

    /// Returns true if the error mark of a refused edit shows.
    pub fn has_error_mark(&self) -> bool {
        self.error_mark
    }
}

/// A highlighted change of a cell: a data cell's, a pinned cell's, an
/// aggregate cell's, or every cell of a removed row while it fades out
/// ([`RowChange::Removed`], with
/// [`HighlightPolicy::with_removal_fade`](crate::HighlightPolicy::with_removal_fade)).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellChange {
    pub(super) intensity: f32,
    pub(super) change: RowChange,
    pub(super) direction: Option<ChangeDirection>,
    pub(super) column_ix: Option<ColumnIx>,
}

impl CellChange {
    /// From 1.0 (just changed) down to the last fade step; for a removed
    /// row, down the steps of its removal fade.
    pub fn intensity(&self) -> f32 {
        self.intensity
    }

    /// What happened to the row. [`RowChange::Removed`] for every cell of a
    /// removed row while it fades out: the row is no longer in the store,
    /// the cell shows its last value.
    pub fn change(&self) -> RowChange {
        self.change
    }

    /// The direction of an update, as [`GridRow::change_direction`] gave it
    /// when the store wrote the change, or as two numeric aggregates
    /// compare; `None` for an insert, an update without a direction (a
    /// text, `mark_changed`, a group's new row count) and a removed row.
    pub fn direction(&self) -> Option<ChangeDirection> {
        self.direction
    }

    /// The column of the cell: a data or pinned cell's, an aggregate
    /// cell's. `None` for a caption's own change (a new group, a new row
    /// count), which tints the caption lane across the columns. Compare it
    /// with `MyColumn::Price.ix()` to paint some columns only.
    pub fn column_ix(&self) -> Option<ColumnIx> {
        self.column_ix
    }

    /// `color` faded by the current intensity.
    pub fn fade(&self, color: Hsla) -> Hsla {
        color.opacity(self.intensity)
    }
}

/// What [`LiveGridDelegate::cell_style`] knows about a cell.
pub struct CellStyleContext<'a, R: GridRow> {
    pub(super) row: &'a R,
    pub(super) column: R::Column,
    pub(super) value: CellValue<'a>,
    pub(super) change: Option<CellChange>,
    pub(super) display_ix: usize,
    pub(super) pinned: Option<PinnedEdge>,
    pub(super) selected: bool,
    pub(super) focused: bool,
    pub(super) edit_state: Option<&'a EditState>,
}

impl<'a, R: GridRow> CellStyleContext<'a, R> {
    /// The row.
    pub fn row(&self) -> &'a R {
        self.row
    }

    /// The column.
    pub fn column(&self) -> R::Column {
        self.column
    }

    /// The value shown in the cell.
    pub fn value(&self) -> CellValue<'a> {
        self.value
    }

    /// The highlighted change, if the cell changed recently.
    pub fn change(&self) -> Option<CellChange> {
        self.change
    }

    /// The display position of the row; for a pinned row, its position in
    /// its band.
    pub fn display_ix(&self) -> usize {
        self.display_ix
    }

    /// The band of a pinned row, or `None` for a data row. Pinned rows are
    /// painted with the theme's `table_foot` colors; a style can tell them
    /// apart here, e.g. to set off a total row.
    pub fn pinned_edge(&self) -> Option<PinnedEdge> {
        self.pinned
    }

    /// Returns true if the cell is selected: in the row modes, if its row
    /// is. Pinned rows are never selected.
    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// Returns true if this is the focused cell (the keyboard target),
    /// whether or not the grid has keyboard focus.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// The cell's edits: a value whose commit waits for an answer
    /// (`Pending`: the grid paints it muted) or for the store (`Accepted`),
    /// or a refused commit (`Failed`: the grid shows the error mark). `None`
    /// for a cell with no edits. The style may paint over the grid's look.
    pub fn edit_state(&self) -> Option<&'a EditState> {
        self.edit_state
    }
}

/// The line of an aggregate cell, as [`LiveGridDelegate::aggregate_style`]
/// sees it.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum AggregateLine<'a> {
    /// A group caption.
    Caption(GroupInfo<'a>),
    /// A group footer.
    Footer(GroupInfo<'a>),
    /// The totals row in the band at its edge.
    Totals(PinnedEdge),
}

/// What [`LiveGridDelegate::aggregate_style`] knows about a visible
/// aggregate cell.
pub struct AggregateStyleContext<'a, R: GridRow> {
    pub(super) line: AggregateLine<'a>,
    pub(super) column: R::Column,
    pub(super) value: Option<CellValue<'a>>,
    pub(super) change: Option<CellChange>,
    pub(super) focused: bool,
}

impl<'a, R: GridRow> AggregateStyleContext<'a, R> {
    /// The line the cell is on.
    pub fn line(&self) -> AggregateLine<'a> {
        self.line
    }

    /// The aggregated column.
    pub fn column(&self) -> R::Column {
        self.column
    }

    /// The aggregate's value, as the cell shows it.
    pub fn value(&self) -> Option<CellValue<'a>> {
        self.value
    }

    /// The highlighted change of the aggregate, if it changed recently:
    /// [`RowChange::Updated`], with the direction of two numbers, against
    /// the projection shown before (matched by the group's path). The grid's
    /// change paint has painted it already, as for a data cell.
    pub fn change(&self) -> Option<CellChange> {
        self.change
    }

    /// Returns true if the cell's line has the focus: a focused caption.
    pub fn is_focused(&self) -> bool {
        self.focused
    }
}

/// Presentation and command hooks of a grid.
///
/// Every hook takes `&self` and must not read, update or notify the grid
/// entity: the grid is borrowed while it calls them, so that panics.
/// Commands leave the grid as Actions and `LiveGridEvent`s instead.
///
/// | Hook | Runs | Rule |
/// |---|---|---|
/// | [`cell_style`](Self::cell_style) | In prepaint, per visible cell, every frame | No allocation, no lock |
/// | [`group_caption`](Self::group_caption), [`group_caption_tag`](Self::group_caption_tag) | In prepaint, per visible group caption, every frame | No allocation (write into `out`), no lock |
/// | [`aggregate_style`](Self::aggregate_style) | In prepaint, per visible aggregate cell, every frame | No allocation, no lock |
/// | [`tooltip`](Self::tooltip) | In prepaint, for the hovered cell and, while a screen reader listens, the focused one: at most twice a frame | May allocate |
/// | [`header_tooltip`](Self::header_tooltip) | In `render`, for the hovered header cell | May allocate |
/// | [`primary_command`](Self::primary_command), [`context_menu`](Self::context_menu) | Once per menu opening, from `window.defer` after the press | May allocate |
/// | [`render_progress`](Self::render_progress) | In `render`, while progress is shown | May allocate |
/// | [`render_cell`](Self::render_cell) | In prepaint, per visible cell of an `Element` or `Control` column, every frame | May allocate: an element is one |
/// | [`is_editable`](Self::is_editable) | Once per edit start, per commit with no field, and per `+`/`-` on a cell with an editor | Command hook, outside prepaint |
/// | [`validate_edit`](Self::validate_edit) | Once per commit attempt | May allocate |
/// | [`render_edit_message`](Self::render_edit_message) | In prepaint, once a frame while the open editor shows a message | May allocate |
/// | [`drag_payload`](Self::drag_payload) | Once per row drag, when it starts | Command hook |
pub trait LiveGridDelegate: Sized + 'static {
    /// The row type.
    type Row: GridRow;

    /// Adjusts the style of a visible cell, after the grid's change paint.
    /// A removed row that fades out is planned too, with
    /// [`RowChange::Removed`] as every cell's change and its last version
    /// as the row.
    fn cell_style(
        &self,
        _cell: &CellStyleContext<'_, Self::Row>,
        _style: &mut CellStyle,
        _cx: &App,
    ) {
    }

    /// Writes the text of a visible group caption into `out`, which is
    /// empty on entry and reused between captions. Default:
    /// [`GroupCaptionContext::default_caption`], "Instrument: AAPL (120)".
    fn group_caption(
        &self,
        caption: &GroupCaptionContext<'_, Self::Row>,
        out: &mut String,
        _cx: &App,
    ) {
        caption.default_caption(out);
    }

    /// Writes the text of a tag shown before a visible group caption's
    /// text into `out`, which is empty on entry and reused between
    /// captions, and returns its colours: a pill such as a transit line's,
    /// in the line's colour. `None`, or no text, shows no tag. A screen
    /// reader hears the tag before the caption. In prepaint, per visible
    /// caption, every frame: no allocation, no lock. A custom key kept as
    /// itself ([`OwnedValue::custom`](crate::OwnedValue::custom)) gives
    /// its data through [`GroupCaptionContext::value`]. Default: `None`.
    fn group_caption_tag(
        &self,
        _caption: &GroupCaptionContext<'_, Self::Row>,
        _out: &mut String,
        _cx: &App,
    ) -> Option<CaptionTag> {
        None
    }

    /// Adjusts the style of a visible aggregate cell: on a group caption,
    /// on a group footer and on the totals row. Called
    /// after the grid's change paint, like [`Self::cell_style`].
    fn aggregate_style(
        &self,
        _cell: &AggregateStyleContext<'_, Self::Row>,
        _style: &mut CellStyle,
        _cx: &App,
    ) {
    }

    /// The label of the primary command: the first, bold item of a cell's
    /// or a group caption's context menu, shown when the command scope is
    /// not empty. It
    /// dispatches `Confirm`, so the item, Enter and a double click all emit
    /// one `LiveGridEvent::Confirm`. Default: no item (Enter and a double
    /// click still confirm).
    fn primary_command(
        &self,
        _menu: &GridMenuContext<'_, Self::Row>,
        _cx: &App,
    ) -> Option<SharedString> {
        None
    }

    /// Builds the context menu below the primary command. Items do their
    /// work later, through Actions dispatched to the grid (or its
    /// ancestors) or `on_click` handlers; a handler reads the command scope
    /// when it runs, not [`GridMenuContext::selection`]. Default:
    /// [`GridMenuContext::standard_items`]. An empty menu is not shown.
    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, Self::Row>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        menu_context.standard_items(menu, window, cx)
    }

    /// The tooltip of the hovered body cell, asked again every frame so a
    /// shown tooltip follows live values in place. Default:
    /// [`CellTooltipContext::default_tooltip`], the full text of a
    /// truncated cell.
    ///
    /// While the accessibility tree is built, it is also asked for the
    /// focused cell (once a frame; once in all if that cell is hovered): a
    /// [`CellTooltip::text`] describes the cell to a screen reader after its
    /// column title, unless it only repeats the cell's text. Write tooltip
    /// text that reads without the screen.
    fn tooltip(&self, cell: &CellTooltipContext<'_, Self::Row>, _cx: &App) -> Option<CellTooltip> {
        cell.default_tooltip()
    }

    /// The tooltip of the hovered header cell. Default:
    /// [`HeaderTooltipContext::default_tooltip`], the full title of a
    /// truncated one.
    fn header_tooltip(
        &self,
        header: &HeaderTooltipContext<'_, <Self::Row as GridRow>::Column>,
        _cx: &App,
    ) -> Option<CellTooltip> {
        header.default_tooltip()
    }

    /// Presents the shown progress ([`LiveGridState::progress`]) over the
    /// body, below the header. The element fills the body and has no
    /// hitbox of its own unless the delegate adds one: presses and the
    /// wheel should reach the rows. Default:
    /// [`ProgressContext::default_overlay`], a thin bar under the header and,
    /// on an empty grid, the label.
    ///
    /// [`LiveGridState::progress`]: super::LiveGridState::progress
    fn render_progress(
        &self,
        progress: &GridProgress,
        overlay: &ProgressContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        overlay.default_overlay(progress, window, cx)
    }

    /// Builds the element of a visible cell of an
    /// [`Element`](super::CellContent::Element) or
    /// [`Control`](super::CellContent::Control) column; `None` paints the
    /// value as text. In prepaint, per visible cell of such a column, every
    /// frame: it may allocate (an element is an allocation), and must not
    /// read, update or notify the grid entity.
    /// The element's own handlers run later, outside the grid's borrow, and
    /// may write to the store. An element that can take the keyboard focus
    /// sets `tab_stop(false)`: the grid is one Tab stop. A removed row that
    /// fades out is asked too, with
    /// [`RowChange::Removed`] as the cell's change: fade the element by
    /// [`CellChange::intensity`]; it takes no press. Default: `None`.
    fn render_cell(
        &self,
        _cell: &CellElementContext<'_, Self::Row>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        None
    }

    /// Returns true if a cell of a column with an editor takes an edit now:
    /// a filled order's price does not. Asked once per edit start, per
    /// commit with no field and per `+`/`-` on such a cell, outside
    /// prepaint. Default: `true`.
    fn is_editable(&self, _cell: &EditableContext<'_, Self::Row>, _cx: &App) -> bool {
        true
    }

    /// Checks a value before its `EditCommit`: `Err` keeps the editor open
    /// with the message. Once per commit attempt, outside prepaint; may
    /// allocate. Default: every value the editor parsed.
    fn validate_edit(
        &self,
        _edit: &EditValidateContext<'_, Self::Row>,
        _cx: &App,
    ) -> Result<(), EditError> {
        Ok(())
    }

    /// Presents the open editor's message: a refused draft or a conflict
    /// with the store. In prepaint, once a frame while a message is shown;
    /// may allocate, as `tooltip`. `None` shows none (an application that
    /// shows `LiveGridState::edit_message` itself). Default:
    /// [`EditMessageContext::default_popup`], a line under the cell.
    fn render_edit_message(
        &self,
        message: &EditMessageContext<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        Some(message.default_popup(window, cx))
    }

    /// The application's part of a row drag that is about to start: data
    /// for the drop target and the ghost's label. `None` refuses the drag.
    /// Called once per drag, when the pointer passes the drag threshold,
    /// inside the grid's update: it must not read or notify the grid.
    /// Default: an empty payload, so the keys alone are dragged.
    fn drag_payload(
        &self,
        _drag: &RowDragContext<'_, Self::Row>,
        _cx: &App,
    ) -> Option<DragPayload> {
        Some(DragPayload::new())
    }
}

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) foreground: Hsla,
    pub(super) row_border: Hsla,
    pub(super) stripe: Hsla,
    pub(super) selected: Hsla,
    pub(super) ring: Hsla,
    pub(super) divider: Hsla,
    /// The background and text of pinned rows.
    pub(super) pinned: Hsla,
    pub(super) pinned_foreground: Hsla,
    /// The text of a value whose commit waits, and the error mark.
    pub(super) muted: Hsla,
    pub(super) danger: Hsla,
    /// The text of a link value.
    pub(super) link: Hsla,
}
