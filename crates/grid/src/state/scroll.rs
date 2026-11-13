//! Vertical and horizontal scrolling: the position as "first row plus the
//! offset inside it", scrolling into view, pages, the scroll anchor kept
//! when a new display order is shown, and the handle the scrollbars use.
//!
//! Only the data rows scroll vertically: their area is the body without the
//! pinned bands (`state/pinned.rs`), and pages, scrolling into view and the
//! end of the scroll range are measured in it.

use std::{cell::RefCell, ops::Range, rc::Rc};

use gpui_kit::base::ScrollbarHandle;
use gpui_kit::{Bounds, Context, Pixels, Point, ScrollDelta, Size, point, px, size};

use super::{
    ColumnZone, KeyOf, LiveGridDelegate, LiveGridState, frame::PaintedLine, selection::successor,
};
use crate::{
    engine::{GroupPath, Projection},
    model::{ColumnIx, ColumnSet},
    store::RowId,
};

/// The rows a page key moves by: the rows that fit in the row area, at
/// least one (also before the first frame, when the area is empty).
pub(super) fn page_size(rows_height: Pixels, row_height: Pixels) -> usize {
    if row_height <= px(0.) {
        return 1;
    }
    ((rows_height / row_height).floor().max(0.) as usize).max(1)
}

/// A scroll position as GPUI's pixel offset of the content: negative, from
/// the first row and the offset inside it.
fn offset_from_position(row: usize, offset: Pixels, row_height: Pixels) -> Pixels {
    -(row_height * row as f32 + offset)
}

/// A pixel offset of the content (negative, as GPUI's) back as "first row
/// plus the offset inside it", held at `max_row`. `f32` pixels are exact
/// enough for a scrollbar thumb: about half a pixel at 100k rows of 40 px.
fn position_from_offset(offset: Pixels, row_height: Pixels, max_row: usize) -> (usize, Pixels) {
    let y = (-offset).max(px(0.));
    if row_height <= px(0.) {
        return (0, px(0.));
    }
    let row = (y / row_height).floor() as usize;
    if row >= max_row {
        return (max_row, px(0.));
    }
    (row, (y - row_height * row as f32).max(px(0.)))
}

#[derive(Default)]
struct ScrollHandleState {
    /// The content offset of the last frame (negative).
    offset: Point<Pixels>,
    content: Size<Pixels>,
    viewport: Bounds<Pixels>,
    /// An offset a scrollbar thumb asked for, applied by the next render.
    requested: Option<Point<Pixels>>,
}

/// The grid's scroll position for `gpui_kit::base::Scrollbar`. The
/// scrollbar sets an offset with `&self` and no context, so the request
/// waits here until the next render applies it to the grid's own model;
/// the scrollbar repaints the grid view itself.
#[derive(Clone, Default)]
pub(super) struct GridScrollHandle(Rc<RefCell<ScrollHandleState>>);

impl GridScrollHandle {
    /// What the frame shows, for the scrollbars painted after the body.
    fn publish(&self, offset: Point<Pixels>, content: Size<Pixels>, viewport: Bounds<Pixels>) {
        let mut state = self.0.borrow_mut();
        state.offset = offset;
        state.content = content;
        state.viewport = viewport;
    }

    fn take_request(&self) -> Option<Point<Pixels>> {
        self.0.borrow_mut().requested.take()
    }
}

impl ScrollbarHandle for GridScrollHandle {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }

    fn offset(&self) -> Point<Pixels> {
        let state = self.0.borrow();
        state.requested.unwrap_or(state.offset)
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.0.borrow_mut().requested = Some(offset);
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.borrow().content
    }
}

/// How the next display order of a new view takes the scroll position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ScrollReset {
    /// The same view with new data: the anchor line keeps its screen line.
    #[default]
    Keep,
    /// A new sort or filter: from the top, unless a painted focused line
    /// stays on its screen line.
    NewView,
    /// A new grouping or tree: from the top, so the first caption shows,
    /// if the body was at the top; else a painted focused line by key.
    NewStructure,
}

impl ScrollReset {
    /// Returns true for a new view or structure.
    pub(super) fn is_reset(self) -> bool {
        self != Self::Keep
    }
}

/// A shown line by identity: a data row, or a group caption by path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AnchorLine {
    Row(RowId),
    Group(GroupPath),
}

/// The line that keeps its screen line when a new display order is shown:
/// a toggled caption or tree row while its lines come and go, else the
/// focused line if the last frame painted it, else the first painted line.
pub(super) struct LineAnchor {
    line: AnchorLine,
    screen: usize,
    focused: bool,
}

impl LineAnchor {
    /// `line` on screen line `screen`; a `focused` anchor keeps its line at
    /// the very top of the body too.
    pub(super) fn new(line: AnchorLine, screen: usize, focused: bool) -> Self {
        Self {
            line,
            screen,
            focused,
        }
    }
}

/// The visible scrolling columns from the first one on screen, with their
/// left edges relative to the scroll position: a layout change keeps the
/// first of them that is still there at its place. A zone
/// scrolled to its start stays at its start, so a column shown or moved
/// before the first one is in view, as new rows on top are.
pub(super) struct ScrollAnchor {
    columns: Vec<(ColumnIx, Pixels)>,
    at_start: bool,
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// The display rows currently in the viewport: the data rows' area,
    /// between the pinned bands.
    pub fn visible_rows(&self) -> Range<usize> {
        let count =
            ((self.rows_area_height() + self.scroll_offset) / self.row_height).ceil() as usize;
        self.scroll_row..(self.scroll_row + count).min(self.projection.len())
    }

    /// Scrolls the row with `key` into view, between the pinned bands. O(1):
    /// key index of the snapshot, then the display index of the projection.
    /// Returns false if the row is not displayed (unknown key, filtered out,
    /// or not yet projected). Pinned rows are always in view.
    pub fn scroll_to_key(&mut self, key: &KeyOf<D>, cx: &mut Context<Self>) -> bool {
        let Some(display_ix) = self
            .snapshot
            .row_id(key)
            .and_then(|id| self.projection.display_ix(id))
        else {
            return false;
        };
        self.reveal_row(display_ix);
        cx.notify();
        true
    }

    /// Scrolls a display row into view by the nearest edge.
    pub(super) fn reveal_row(&mut self, display_ix: usize) {
        let viewport_rows = page_size(self.rows_area_height(), self.row_height);
        if display_ix < self.scroll_row
            || (display_ix == self.scroll_row && self.scroll_offset > px(0.))
        {
            self.scroll_row = display_ix;
            self.scroll_offset = px(0.);
        } else if display_ix >= self.scroll_row + viewport_rows {
            self.scroll_row = display_ix + 1 - viewport_rows;
            self.scroll_offset = px(0.);
        }
        self.clamp_scroll();
    }

    /// The height of the data rows' area: the body below the header without
    /// the pinned bands. Page keys move by the rows that fit in it.
    pub(super) fn rows_area_height(&self) -> Pixels {
        self.rows_bounds().size.height
    }

    /// Scrolls so `display_ix` is the first visible row.
    pub fn scroll_to_row(&mut self, display_ix: usize, cx: &mut Context<Self>) {
        self.scroll_row = display_ix;
        self.scroll_offset = px(0.);
        self.clamp_scroll();
        cx.notify();
    }

    /// Scrolls by a pixel delta (positive moves content up and left).
    pub fn scroll_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        self.scroll_x = (self.scroll_x + delta.x).max(px(0.));
        self.scroll_rows_by(delta.y);
        cx.notify();
    }

    /// Scrolls by a wheel or trackpad delta. A wheel line is one row at
    /// every row height, so a line wheel (X11, Windows, a mouse on macOS)
    /// stops on a whole row; a pixel delta scrolls by its pixels.
    pub(crate) fn on_scroll_wheel(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        let delta = delta.pixel_delta(self.row_height);
        self.scroll_by(point(-delta.x, -delta.y), cx);
    }

    /// Scrolls the rows by a pixel delta (positive moves content up).
    pub(super) fn scroll_rows_by(&mut self, delta: Pixels) {
        // Keep the position as "first row + offset inside it": exact at any
        // depth, where one f32 of total pixels would lose sub-pixel precision.
        let mut offset = self.scroll_offset + delta;
        while offset < px(0.) && self.scroll_row > 0 {
            self.scroll_row -= 1;
            offset += self.row_height;
        }
        let rows = (offset / self.row_height).floor() as usize;
        self.scroll_row += rows;
        self.scroll_offset = (offset - self.row_height * rows as f32).max(px(0.));
        self.clamp_scroll();
    }

    /// Applies an offset a scrollbar thumb asked for since the last render.
    pub(super) fn apply_scroll_request(&mut self) {
        let Some(offset) = self.scroll_handle.take_request() else {
            return;
        };
        let (row, row_offset) =
            position_from_offset(offset.y, self.row_height, self.max_scroll_row());
        self.scroll_row = row;
        self.scroll_offset = row_offset;
        self.scroll_x = (-offset.x).max(px(0.));
        self.clamp_scroll();
    }

    /// Tells the scrollbars what this frame shows: the offset, the rows'
    /// height and the scrolling columns' width, and the data rows' area.
    pub(super) fn publish_scroll(&self) {
        let content = size(
            self.columns
                .zone_columns(ColumnZone::Scrolling)
                .content_width(),
            self.row_height * self.projection.len() as f32,
        );
        let offset = point(
            -self.scroll_x,
            offset_from_position(self.scroll_row, self.scroll_offset, self.row_height),
        );
        self.scroll_handle
            .publish(offset, content, self.rows_bounds());
    }

    /// The first row of the body when it is scrolled to the end.
    fn max_scroll_row(&self) -> usize {
        let viewport_rows = (self.rows_area_height() / self.row_height).floor() as usize;
        self.projection.len().saturating_sub(viewport_rows.max(1))
    }

    /// The line that keeps its screen line through a projection swap: the
    /// painted focused line (a row or a caption), else the first painted
    /// row or caption. A group footer is never the anchor: it has no
    /// identity of its own across expansions. A fading row is never the
    /// anchor either: it is leaving.
    pub(super) fn line_anchor(&self) -> Option<LineAnchor> {
        let anchor = |screen: usize, focused: bool| {
            let line = match &self.painted[screen] {
                PaintedLine::Row(id) => AnchorLine::Row(*id),
                PaintedLine::Caption(path) => AnchorLine::Group(path.clone()),
                PaintedLine::Footer(_) => return None,
                // A fading row is no anchor.
                PaintedLine::Ghost(_) => return None,
            };
            Some(LineAnchor::new(line, screen, focused))
        };
        let focused = self.selection.focus().and_then(|focus| {
            let screen = match (focus.row(), focus.group()) {
                (Some(key), _) => {
                    let id = self.snapshot.row_id(key)?;
                    self.painted
                        .iter()
                        .position(|painted| painted.row() == Some(id))?
                }
                (None, Some(path)) => self
                    .painted
                    .iter()
                    .position(|painted| painted.group() == Some(path))?,
                (None, None) => return None,
            };
            anchor(screen, true)
        });
        focused.or_else(|| (0..self.painted.len()).find_map(|screen| anchor(screen, false)))
    }

    /// Puts the anchor line back on its screen line after `previous` was
    /// replaced. A body at the very top stays there unless the
    /// anchor is the focused line, so new rows on top show. A gone anchor
    /// row is replaced by its successor in the `previous` order. After a
    /// new view (`ScrollReset::NewView`) only a painted focused line keeps
    /// its screen line; otherwise the body shows the new order from the
    /// top. After a new grouping or tree (`ScrollReset::NewStructure`) a
    /// body at the top shows the new order from the top, its first caption
    /// first, even with a focused line on screen; a scrolled body keeps a
    /// painted focused line by key if the new order shows it, and shows
    /// the new order from the top otherwise.
    pub(super) fn restore_line_anchor(
        &mut self,
        anchor: Option<LineAnchor>,
        previous: &Projection,
        scroll_reset: ScrollReset,
    ) {
        let shown = |line: &AnchorLine| match line {
            AnchorLine::Row(id) => self.projection.display_ix(*id),
            AnchorLine::Group(path) => {
                let group = self.projection.groups().find(path)?;
                self.projection.group_line(group.id())
            }
        };
        let at_top = self.scroll_row == 0 && self.scroll_offset == px(0.);
        if scroll_reset == ScrollReset::NewStructure {
            // The lines around the anchor are not the ones it had: at the
            // top the first caption shows; scrolled, a painted focused line
            // stays on its screen line by key, as after a new view.
            match anchor
                .filter(|anchor| anchor.focused && !at_top)
                .and_then(|anchor| {
                    shown(&anchor.line).map(|display_ix| (display_ix, anchor.screen))
                }) {
                Some((display_ix, screen)) => self.place_row(display_ix, screen),
                None => self.scroll_to_top(),
            }
            return;
        }
        if scroll_reset.is_reset() {
            match anchor.filter(|anchor| anchor.focused).and_then(|anchor| {
                shown(&anchor.line).map(|display_ix| (display_ix, anchor.screen))
            }) {
                Some((display_ix, screen)) => self.place_row(display_ix, screen),
                None => self.scroll_to_top(),
            }
            return;
        }
        let Some(anchor) = anchor else {
            return;
        };
        if at_top && !anchor.focused {
            return;
        }
        let display_ix = shown(&anchor.line).or_else(|| {
            let AnchorLine::Row(id) = anchor.line else {
                return None;
            };
            let snapshot = &self.snapshot;
            successor(previous, Some(id), |id| {
                snapshot
                    .row(id)
                    .and_then(|_| self.projection.display_ix(id))
            })
        });
        if let Some(display_ix) = display_ix {
            self.place_row(display_ix, anchor.screen);
        }
    }

    /// Scrolls so the display row is on screen line `line`, with the offset
    /// inside the first row unchanged.
    fn place_row(&mut self, display_ix: usize, line: usize) {
        if display_ix >= line {
            self.scroll_row = display_ix - line;
        } else {
            self.scroll_row = 0;
            self.scroll_offset = px(0.);
        }
    }

    /// Shows the first row at the top. A new view (sort, filter, quick
    /// filter) has no relation to the old position, so its order is shown
    /// from the top, unless a painted focused row stays.
    pub(super) fn scroll_to_top(&mut self) {
        self.scroll_row = 0;
        self.scroll_offset = px(0.);
    }

    /// The horizontal scroll offset of the scrolling zone: 0 at its
    /// start.
    pub fn scroll_x(&self) -> Pixels {
        self.scroll_x
    }

    /// Sets the horizontal scroll offset.
    pub fn set_scroll_x(&mut self, scroll_x: Pixels, cx: &mut Context<Self>) {
        self.scroll_x = scroll_x.max(px(0.));
        self.clamp_scroll();
        cx.notify();
    }

    pub(super) fn clamp_scroll(&mut self) {
        let max_row = self.max_scroll_row();
        if self.scroll_row >= max_row {
            self.scroll_row = max_row;
            self.scroll_offset = px(0.);
        }
        // Only the scrolling zone scrolls: its content past its own width.
        let content = self
            .columns
            .zone_columns(ColumnZone::Scrolling)
            .content_width();
        let max_x = (content - self.scrolling_zone_width()).max(px(0.));
        self.scroll_x = self.scroll_x.min(max_x).max(px(0.));
    }

    /// The width of the scrolling zone in the body.
    pub(super) fn scrolling_zone_width(&self) -> Pixels {
        self.columns
            .geometry(self.body_bounds.size.width, self.scroll_x)
            .zone(ColumnZone::Scrolling)
            .width
    }

    pub(super) fn scroll_anchor(&self) -> ScrollAnchor {
        let zone = self.columns.zone_columns(ColumnZone::Scrolling);
        let first = zone.offsets[1..].partition_point(|&end| end <= self.scroll_x);
        ScrollAnchor {
            columns: (first..zone.len())
                .map(|i| (zone.columns[i], zone.offsets[i] - self.scroll_x))
                .collect(),
            at_start: self.scroll_x <= px(0.),
        }
    }

    /// Scrolls so the anchor column (or, if it left the zone, the next one
    /// still there) is where it was on screen; a zone at its start stays
    /// there.
    pub(super) fn restore_scroll_anchor(&mut self, anchor: ScrollAnchor) {
        if anchor.at_start {
            self.scroll_x = px(0.);
            return;
        }
        let zone = self.columns.zone_columns(ColumnZone::Scrolling);
        for (column, screen_x) in anchor.columns {
            if let Some((ColumnZone::Scrolling, i)) = self.columns.zone_position(column) {
                self.scroll_x = (zone.offsets[i] - screen_x).max(px(0.));
                return;
            }
        }
    }

    /// Scrolls the scrolling-zone columns of `columns` into view (their left
    /// edge first if they do not fit). Moved columns would otherwise land
    /// off screen when dropped left of the anchor column.
    pub(super) fn reveal_columns(&mut self, columns: ColumnSet) {
        let width = self.scrolling_zone_width();
        if width <= px(0.) {
            return;
        }
        let zone = self.columns.zone_columns(ColumnZone::Scrolling);
        let mut span: Option<(Pixels, Pixels)> = None;
        for (i, column) in zone.columns.iter().enumerate() {
            if columns.contains(*column) {
                let (left, right) = (zone.offsets[i], zone.offsets[i + 1]);
                span = Some(span.map_or((left, right), |(l, r)| (l.min(left), r.max(right))));
            }
        }
        let Some((left, right)) = span else {
            return;
        };
        if left < self.scroll_x {
            self.scroll_x = left;
        } else if right > self.scroll_x + width {
            self.scroll_x = (right - width).min(left);
        }
    }

    /// Keeps the right edge of a scrolling column in view while it is
    /// resized with the mouse.
    pub(super) fn reveal_right_edge(&mut self, column: ColumnIx) {
        let width = self.scrolling_zone_width();
        let Some((ColumnZone::Scrolling, i)) = self.columns.zone_position(column) else {
            return;
        };
        let right = self.columns.zone_columns(ColumnZone::Scrolling).offsets[i + 1];
        if width > px(0.) && right > self.scroll_x + width {
            self.scroll_x = right - width;
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::base::ScrollbarHandle as _;
    use gpui_kit::{point, px};

    use super::{GridScrollHandle, offset_from_position, page_size, position_from_offset};

    #[test]
    fn test_page_size() {
        // The `xsmall`, `small`, `medium` and `large` rows in a 260 px area.
        for (row_height, page) in [(26., 10), (30., 8), (32., 8), (40., 6)] {
            assert_eq!(page_size(px(260.), px(row_height)), page, "{row_height} px");
        }
        // A partly visible row is not part of the page.
        assert_eq!(page_size(px(259.), px(26.)), 9);
        // Before the first frame the area is empty: a page is one row.
        assert_eq!(page_size(px(0.), px(26.)), 1);
        assert_eq!(page_size(px(10.), px(26.)), 1);
        assert_eq!(page_size(px(260.), px(0.)), 1);
    }

    #[test]
    fn test_grid_scroll_handle() {
        let row = px(26.);
        // GPUI's convention: the content moves up, so the offset is negative.
        assert_eq!(offset_from_position(0, px(0.), row), px(0.));
        assert_eq!(offset_from_position(3, px(5.), row), px(-83.));
        // There and back, at the top and deep into 100k rows.
        for (first, inside) in [(0, 0.), (3, 5.), (12_345, 13.5), (99_990, 0.)] {
            let offset = offset_from_position(first, px(inside), row);
            let (back, back_inside) = position_from_offset(offset, row, 99_990);
            assert_eq!(back, first);
            assert!(
                (back_inside - px(inside)).abs() < px(0.5),
                "{first}: {back_inside:?}"
            );
        }
        // Held at the last first row; never above the top.
        assert_eq!(
            position_from_offset(px(-26. * 200.), row, 100),
            (100, px(0.))
        );
        assert_eq!(position_from_offset(px(40.), row, 100), (0, px(0.)));

        // A thumb drag waits in the handle until the next render takes it.
        let handle = GridScrollHandle::default();
        handle.publish(
            point(px(0.), px(-52.)),
            gpui_kit::size(px(500.), px(2_600.)),
            Default::default(),
        );
        assert_eq!(handle.offset(), point(px(0.), px(-52.)));
        assert_eq!(handle.content_size().height, px(2_600.));
        handle.set_offset(point(px(-10.), px(-104.)));
        assert_eq!(handle.offset(), point(px(-10.), px(-104.)));
        assert_eq!(handle.take_request(), Some(point(px(-10.), px(-104.))));
        assert_eq!(handle.take_request(), None);
        assert_eq!(handle.offset(), point(px(0.), px(-52.)));
    }
}
