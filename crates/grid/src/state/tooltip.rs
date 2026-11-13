//! Tooltips: what the tooltip hooks of the delegate know and return, the
//! hovered body cell with its tooltip slot, and the tooltips of header
//! cells.
//!
//! The body paints without an element per cell, so a cell tooltip hangs on
//! one transparent div over the hovered cell, planned with the frame. Its
//! id is the hovered row's key and column, so a row moving away from under
//! the pointer is a new element: GPUI drops the old tooltip, and a tooltip
//! never shows another row's data. While the id stays, the frame writes the
//! hook's fresh result into the slot the shown tooltip reads, so a ticking
//! price updates in place (Design Guides, "Motion"). A pinned row's key is
//! of its band's own key space, so its slot hashes the edge with the key: a
//! band row and a data row with the same key are different elements. A
//! group caption's text and aggregate cells hash its group path
//! (`group_hash`), a group footer's cells its path (`footer_hash`), the
//! totals row's cells its band (`totals_hash`).

use std::{
    cell::{Cell, RefCell},
    fmt,
    rc::Rc,
};

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, App, Bounds, Context, ElementId, Empty, InteractiveElement as _, IntoElement,
    Pixels, Point, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use rust_i18n::t;

use super::{
    CELL_PADDING, ColumnOf, HeaderLabel, LiveGridDelegate, LiveGridState,
    edit::EditState,
    header::{HEADER_SORT_LANE, SORT_GAP, SORT_ICON, header_icon_lane, header_label_style},
    pinned::BodyLine,
};
use crate::model::{CellValue, ColumnIx, GridColumns, GridRow};

/// Tooltip content of a cell or a header cell: text, or an element built
/// each time the tooltip draws.
#[derive(Clone)]
pub struct CellTooltip {
    content: TooltipContent,
}

#[derive(Clone)]
enum TooltipContent {
    Text(SharedString),
    Element(Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>),
}

impl CellTooltip {
    /// A text tooltip.
    pub fn text(text: impl Into<SharedString>) -> Self {
        Self {
            content: TooltipContent::Text(text.into()),
        }
    }

    /// A tooltip with custom content, built each time the tooltip draws.
    pub fn element(build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self {
            content: TooltipContent::Element(Rc::new(build)),
        }
    }

    /// The text of a text tooltip; `None` for custom content.
    pub fn as_text(&self) -> Option<&SharedString> {
        match &self.content {
            TooltipContent::Text(text) => Some(text),
            TooltipContent::Element(_) => None,
        }
    }

    /// The content as an element.
    fn render(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        match &self.content {
            TooltipContent::Text(text) => text.clone().into_any_element(),
            TooltipContent::Element(build) => build(window, cx),
        }
    }

    /// The tooltip view of gpui-kit with this content.
    pub(super) fn into_view(self, window: &mut Window, cx: &mut App) -> gpui_kit::AnyView {
        Tooltip::element(move |window, cx| self.render(window, cx)).build(window, cx)
    }
}

impl fmt::Debug for CellTooltip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.content {
            TooltipContent::Text(text) => f.debug_tuple("CellTooltip::Text").field(text).finish(),
            TooltipContent::Element(_) => f.write_str("CellTooltip::Element"),
        }
    }
}

/// What [`LiveGridDelegate::tooltip`] knows about the hovered cell.
pub struct CellTooltipContext<'a, R: GridRow> {
    pub(super) row: &'a R,
    pub(super) column: R::Column,
    pub(super) value: CellValue<'a>,
    pub(super) text: &'a str,
    pub(super) truncated: bool,
    pub(super) edit_state: Option<&'a EditState>,
}

impl<'a, R: GridRow> CellTooltipContext<'a, R> {
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

    /// The full text of the cell, as painted before truncation.
    pub fn text(&self) -> &str {
        self.text
    }

    /// Returns true if the painted text was cut with "…".
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The cell's edits: a commit that waits for an answer or for the
    /// store, or a refused one; `None` for a cell with no edits.
    pub fn edit_state(&self) -> Option<&'a EditState> {
        self.edit_state
    }

    /// The default tooltip: "Saving…" while a commit waits for an answer,
    /// "Not saved: " and the reason after a refused one; otherwise the full
    /// text of a truncated cell, and none for a cell that shows all its
    /// text.
    pub fn default_tooltip(&self) -> Option<CellTooltip> {
        match self.edit_state {
            Some(EditState::Pending) => {
                return Some(CellTooltip::text(SharedString::from(
                    t!("live_grid.edit.pending").into_owned(),
                )));
            }
            Some(EditState::Failed(error)) => {
                return Some(CellTooltip::text(SharedString::from(
                    t!("live_grid.edit.failed", message = error.message()).into_owned(),
                )));
            }
            _ => {}
        }
        self.truncated
            .then(|| CellTooltip::text(SharedString::from(self.text.to_owned())))
    }
}

/// What [`LiveGridDelegate::header_tooltip`] knows about the hovered header
/// cell.
pub struct HeaderTooltipContext<'a, C> {
    pub(super) column: C,
    pub(super) title: &'a SharedString,
    pub(super) truncated: bool,
    // The title, an icon or both.
    pub(super) label: &'a HeaderLabel,
}

impl<'a, C: GridColumns> HeaderTooltipContext<'a, C> {
    /// The column.
    pub fn column(&self) -> C {
        self.column
    }

    /// The column title.
    pub fn title(&self) -> &'a SharedString {
        self.title
    }

    /// Returns true if the title is cut with "…"; false for a header that
    /// shows its icon alone.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// What the header shows: the title, an icon and the title, or the icon
    /// alone.
    pub fn header_label(&self) -> &'a HeaderLabel {
        self.label
    }

    /// The full title when it is truncated, and always for a header that
    /// shows its icon alone, whose meaning would otherwise rest on the icon;
    /// otherwise none.
    pub fn default_tooltip(&self) -> Option<CellTooltip> {
        (self.truncated || matches!(self.label, HeaderLabel::Icon(_)))
            .then(|| CellTooltip::text(self.title.clone()))
    }
}

/// The tooltip of the hovered cell, shared by the frame, which writes the
/// hook's result every frame, and the shown tooltip view, which draws it.
#[derive(Default)]
pub(crate) struct TooltipSlot {
    content: RefCell<Option<CellTooltip>>,
    /// GPUI built the tooltip view for this slot: it is shown, or was until
    /// a press (GPUI does not report hiding it).
    shown: Cell<bool>,
}

impl TooltipSlot {
    fn render(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        // Cloned out first: the content may read anything but this slot.
        let content = self.content.borrow().clone();
        match content {
            Some(content) => content.render(window, cx),
            None => Empty.into_any_element(),
        }
    }
}

/// The hovered body cell of one frame: the transparent div the body lays
/// out, prepaints and paints over the cell (as `uniform_list` does its
/// items), carrying the GPUI tooltip.
pub(crate) struct CellHover {
    id: ElementId,
    bounds: Bounds<Pixels>,
    slot: Rc<TooltipSlot>,
}

impl CellHover {
    /// The cell within the mask of its zone, in window coordinates.
    pub(crate) fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// The div over the cell.
    pub(crate) fn into_element(self) -> AnyElement {
        let slot = self.slot;
        div()
            .id(self.id)
            .w(self.bounds.size.width)
            .h(self.bounds.size.height)
            .tooltip(move |window, cx| {
                slot.shown.set(true);
                let slot = slot.clone();
                Tooltip::element(move |window, cx| slot.render(window, cx)).build(window, cx)
            })
            .into_any_element()
    }
}

/// The hovered body cell as the frame found it.
pub(super) struct HoveredCell {
    pub(super) key_hash: u64,
    pub(super) column: ColumnIx,
    pub(super) id: &'static str,
    pub(super) bounds: Bounds<Pixels>,
    pub(super) tooltip: Option<CellTooltip>,
}

/// The slot of the hovered cell, by the identity its div's id is made of.
struct HoverSlot {
    key_hash: u64,
    column: ColumnIx,
    slot: Rc<TooltipSlot>,
}

/// The pointer over the body and the header, for tooltips.
#[derive(Default)]
pub(super) struct TooltipState {
    /// The last pointer position over the body.
    pub(super) position: Option<Point<Pixels>>,
    /// The body line and column under `position` when it was set: a move
    /// within one cell does not repaint.
    cell: Option<(BodyLine, Option<ColumnIx>)>,
    slot: Option<HoverSlot>,
    /// The header cell under the pointer.
    header: Option<ColumnIx>,
}

impl TooltipState {
    /// A press hides GPUI's tooltip.
    pub(super) fn on_press(&mut self) {
        if let Some(hover) = &self.slot {
            hover.slot.shown.set(false);
        }
    }

    /// Returns true while the tooltip of the hovered cell is shown.
    pub(super) fn is_shown(&self) -> bool {
        self.slot
            .as_ref()
            .is_some_and(|hover| hover.slot.shown.get())
    }

    /// The header cell under the pointer.
    pub(super) fn header(&self) -> Option<ColumnIx> {
        self.header
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// The screen line of the data rows under a window position, from the
    /// top of the data rows; `None` over a pinned band or outside the body.
    pub(super) fn line_at(&self, position: Point<Pixels>) -> Option<usize> {
        let rows = self.rows_bounds();
        let y = position.y - rows.origin.y + self.scroll_offset;
        (position.y >= rows.origin.y && position.y < rows.bottom())
            .then(|| (y / self.row_height).floor() as usize)
    }

    /// A button went down anywhere in the window, in the capture phase:
    /// GPUI hides the shown tooltip, and an open editor the press is
    /// outside of ends first.
    pub(crate) fn on_window_mouse_down(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tooltip.on_press();
        self.commit_edit_on_press(position, window, cx);
    }

    /// The pointer moved over the body (`Some`) or off it (`None`). Repaints
    /// only when the cell under it changes: O(1) for the line, O(log
    /// columns) for the column.
    pub(crate) fn set_hover_position(
        &mut self,
        position: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let cell = position.and_then(|position| {
            Some((
                self.body_line_at(position)?,
                self.column_at_position(position),
            ))
        });
        self.tooltip.position = position;
        self.update_link_hover(position, cx);
        if self.tooltip.cell != cell {
            self.tooltip.cell = cell;
            cx.notify();
        }
    }

    /// The line and column of the cell under the pointer this frame, if a
    /// tooltip may show: not while a press, a column gesture or the menu is
    /// in progress. Called after the frame has its bounds and scroll
    /// position, so a row moved under a resting pointer is the new row.
    pub(super) fn hover_target(&self) -> Option<(BodyLine, ColumnIx)> {
        if self.press.is_some() || self.is_column_gesture_active() || self.is_menu_open() {
            return None;
        }
        let position = self.tooltip.position?;
        Some((
            self.body_line_at(position)?,
            self.column_at_position(position)?,
        ))
    }

    /// The body line and the pointer position this frame, if a tooltip may
    /// show (as [`Self::hover_target`]): a group caption's text is not a
    /// column's cell.
    pub(super) fn hover_point(&self) -> Option<(BodyLine, Point<Pixels>)> {
        if self.press.is_some() || self.is_column_gesture_active() || self.is_menu_open() {
            return None;
        }
        let position = self.tooltip.position?;
        Some((self.body_line_at(position)?, position))
    }

    /// Keeps the slot of the hovered cell while its identity stays, fills
    /// it with the hook's fresh result, and plans the div. Without a
    /// tooltip there is no div.
    pub(super) fn finish_hover(&mut self, hovered: Option<HoveredCell>) -> Option<CellHover> {
        let Some(HoveredCell {
            key_hash,
            column,
            id,
            bounds,
            tooltip: Some(tooltip),
        }) = hovered.filter(|hovered| {
            hovered.bounds.size.width > px(0.) && hovered.bounds.size.height > px(0.)
        })
        else {
            self.tooltip.slot = None;
            return None;
        };
        let slot = match &self.tooltip.slot {
            Some(hover) if hover.key_hash == key_hash && hover.column == column => {
                hover.slot.clone()
            }
            _ => {
                let slot = Rc::new(TooltipSlot::default());
                self.tooltip.slot = Some(HoverSlot {
                    key_hash,
                    column,
                    slot: slot.clone(),
                });
                slot
            }
        };
        slot.content.replace(Some(tooltip));
        Some(CellHover {
            id: ElementId::NamedInteger(SharedString::new_static(id), key_hash),
            bounds,
            slot,
        })
    }

    /// Returns true while a cell tooltip is shown: GPUI built it for the
    /// hovered cell, which has not changed since, and no button was pressed
    /// (what `ReorderContext::is_tooltip_shown` tells the reorder policy).
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_tooltip_shown(&self) -> bool {
        self.tooltip.is_shown()
    }

    /// The text of the tooltip the last frame planned for the body cell
    /// under the pointer, shown or not yet; `None` without one, or for
    /// custom content.
    #[cfg(any(test, feature = "test-support"))]
    pub fn hovered_tooltip_text(&self) -> Option<SharedString> {
        let hover = self.tooltip.slot.as_ref()?;
        let content = hover.slot.content.borrow();
        content.as_ref()?.as_text().cloned()
    }

    /// The pointer entered or left a header cell.
    pub(super) fn on_header_cell_hover(
        &mut self,
        column: ColumnIx,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let header = if hovered {
            Some(column)
        } else if self.tooltip.header == Some(column) {
            None
        } else {
            return;
        };
        if self.tooltip.header != header {
            self.tooltip.header = header;
            cx.notify();
        }
    }

    /// The tooltip of the hovered header cell, from the delegate. Called in
    /// `render`, once, for that cell only. The title is truncated when its
    /// width (shaped once and cached, as autofit measures it) and the sort
    /// indicator do not fit between the cell's paddings.
    pub(super) fn header_tooltip(
        &mut self,
        window: &Window,
        cx: &App,
    ) -> Option<(ColumnIx, CellTooltip)> {
        let ix = self.tooltip.header()?;
        let column = self
            .columns
            .get(ix)
            .filter(|_| self.columns.is_visible(ix))?;
        let title = column.title.clone();
        let label = column.header_label.clone();
        let column: ColumnOf<D> = column.column;
        let width = self.columns.width(ix);
        let sort = self.view.sort();
        let indicator = match sort.iter().position(|(sorted, _)| sorted.ix() == ix) {
            None => px(0.),
            Some(_) if sort.len() >= 2 => HEADER_SORT_LANE,
            Some(_) => SORT_GAP + SORT_ICON,
        };
        let (font, font_size) = header_label_style(window);
        // An icon before the title takes room from it; a header that shows
        // the icon alone truncates no title.
        let truncated = match label {
            HeaderLabel::Icon(_) => false,
            HeaderLabel::Title | HeaderLabel::IconAndTitle(_) => {
                let title_width = self
                    .header
                    .labels
                    .width(ix, &title, &font, font_size, window);
                let icon = header_icon_lane(&label, self.row_height);
                title_width > width - CELL_PADDING * 2. - indicator - icon
            }
        };
        let context = HeaderTooltipContext {
            column,
            title: &title,
            truncated,
            label: &label,
        };
        let tooltip = self.delegate.header_tooltip(&context, cx)?;
        Some((ix, tooltip))
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{Hash as _, Hasher as _};

    use gpui_kit::SharedString;
    use rustc_hash::FxHasher;

    use super::{CellTooltip, CellTooltipContext, HeaderLabel, HeaderTooltipContext};
    use crate::{
        model::{CellValue, ColumnIx, GridColumns, GridRow},
        state::key_hash,
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct Note;

    impl GridColumns for Note {
        fn all() -> &'static [Self] {
            &[Note]
        }

        fn id(self) -> &'static str {
            "note"
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(0)
        }
    }

    struct Row(String);

    impl GridRow for Row {
        type Key = String;
        type Column = Note;

        fn key(&self) -> &String {
            &self.0
        }

        fn value(&self, _: Note) -> CellValue<'_> {
            CellValue::Text(&self.0)
        }
    }

    #[test]
    fn test_default_tooltips() {
        let row = Row("A note too long for its column".into());
        let cell = |truncated| CellTooltipContext {
            row: &row,
            column: Note,
            value: row.value(Note),
            text: &row.0,
            truncated,
            edit_state: None,
        };
        // The full text, only when truncated.
        let tooltip = cell(true).default_tooltip().expect("truncated");
        assert_eq!(tooltip.as_text().map(|text| &**text), Some(row.0.as_str()));
        assert!(cell(false).default_tooltip().is_none());
        assert_eq!(cell(true).text(), row.0);
        assert_eq!(cell(true).value(), CellValue::Text(&row.0));
        assert!(cell(true).is_truncated());

        let title = SharedString::from("Notes for the trader");
        let header = |truncated| HeaderTooltipContext {
            column: Note,
            title: &title,
            truncated,
            label: &HeaderLabel::Title,
        };
        let tooltip = header(true).default_tooltip().expect("truncated");
        assert_eq!(tooltip.as_text(), Some(&title));
        assert!(header(false).default_tooltip().is_none());
        assert_eq!(header(false).column(), Note);
        // A header that shows its icon alone names it.
        let icon = HeaderLabel::Icon("icons/check.svg".into());
        let icon_only = HeaderTooltipContext {
            column: Note,
            title: &title,
            truncated: false,
            label: &icon,
        };
        assert_eq!(icon_only.header_label(), &icon);
        let tooltip = icon_only.default_tooltip().expect("an icon alone");
        assert_eq!(tooltip.as_text(), Some(&title));

        let custom = CellTooltip::element(|_, _| unreachable!("never drawn here"));
        assert!(custom.as_text().is_none());
        assert_eq!(format!("{custom:?}"), "CellTooltip::Element");
    }

    #[test]
    fn test_key_hash() {
        // `FxHasher` over the key, as `state/a11y.rs` hashes row keys.
        let mut hasher = FxHasher::default();
        42u64.hash(&mut hasher);
        assert_eq!(key_hash(&42u64), hasher.finish());
        assert_eq!(key_hash("key"), key_hash(&String::from("key")));
        assert_ne!(key_hash(&1u32), key_hash(&2u32));
    }
}
