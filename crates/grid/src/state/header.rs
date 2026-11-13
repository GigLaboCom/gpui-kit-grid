//! The column header: one strip per zone of header cells under the band
//! rows, resize handles, the drop gap of a column drag, the arithmetic hit
//! test of the header, the column gestures (resize, reorder), and sorting by
//! a click on a header cell with its sort indicators.
//!
//! Element ids: `"live-grid-header"` holds the column cells (`column.id()`)
//! and `"live-grid-bands"`, which holds the band cells
//! (`NamedChild(<band id>, <first column id>)`); `"live-grid-resize"`, its
//! sibling, holds the handles (`column.id()`). So a column id is unambiguous
//! within each of them. A sorted column's cell holds `"sort-indicator"` (the
//! direction icon) and, when two or more columns are sorted,
//! `"sort-priority"` (its number, 1 the most significant). A header icon is
//! `"header-icon"` in its column's cell.
//!
//! The header is one `Row` of the accessibility tree (row index 1), its
//! cells and band cells `ColumnHeader`s named by their titles, with the
//! column index of their (first) column, a sorted column's direction and a
//! band's span. Band cells sit inside the header row's element, so their
//! nodes are in that row.
//!
//! A right press (on macOS also Ctrl+press) opens the header menu of the
//! column under it; the hovered cell shows the delegate's `header_tooltip`
//! (by default the full title when it is truncated), and the sort indicator
//! tells the direction and priority.
//!
//! A header drag over the group panel groups by the column instead of moving
//! it (`state/group_panel.rs`). A column that is not movable but groupable
//! is dragged too while the panel shows; it lands only in the panel.

use std::sync::{Arc, LazyLock};

use gpui_kit::base::{AutoScroll, TestSupportExt as _};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Size as ComponentSize, tooltip::Tooltip,
};
use gpui_kit::{
    AnyElement, AppContext as _, Bounds, ClickEvent, Context, CursorStyle, Div, DragMoveEvent,
    ElementId, Empty, EntityId, Font, FontFeatures, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement as _, Pixels, Point, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TextRun, Window, accesskit, black, div, point,
    prelude::FluentBuilder as _, px, size,
};

use rust_i18n::t;

use super::{
    CELL_PADDING, ColumnAlign, ColumnOf, ColumnZone, HeaderLabel, LiveGridDelegate, LiveGridState,
    SortOrigin,
    a11y::{HEADER_ROW_INDEX, column_index},
    columns::{Boundary, LayoutDelta, ZoneColumns, ZoneFrame, ZoneGeometry, column_of},
    events::{LayoutOrigin, LayoutPart},
    ghost::GhostTextStyle,
    menu::GridTarget,
    stats::{profile_elapsed, profile_start},
    tooltip::CellTooltip,
};
use crate::{
    engine::SortDirection,
    model::{ColumnIx, GridColumns},
    policy::{GridArea, SortCycleContext},
};

/// The width the header reserves for the sort indicator of a sortable
/// column: a 12 px icon, a 4 px gap and two priority digits. Autofit adds it
/// to the title, so a later sort does not truncate a fitted title.
pub(crate) const HEADER_SORT_LANE: Pixels = px(28.);

/// The gap between a header title and its sort indicator.
pub(super) const SORT_GAP: Pixels = px(4.);

/// The width of the sort direction icon (`size_3`).
pub(super) const SORT_ICON: Pixels = px(12.);

/// The gap between a header icon and its title.
pub(super) const HEADER_ICON_GAP: Pixels = px(4.);

/// The element id of a header icon, inside its column's cell.
const HEADER_ICON_ID: &str = "header-icon";

/// The size of a header icon: 14 px in the `xsmall` and `small` grids, 16 px
/// in the `medium` and `large` ones.
pub(super) fn header_icon_size(row_height: Pixels) -> Pixels {
    if row_height < ComponentSize::Medium.table_row_height() {
        px(14.)
    } else {
        px(16.)
    }
}

/// The width a header icon takes beside the title: the icon and its gap
/// before a title, the icon alone when the header shows it alone, nothing
/// without one.
pub(super) fn header_icon_lane(label: &HeaderLabel, row_height: Pixels) -> Pixels {
    match label {
        HeaderLabel::Title => px(0.),
        HeaderLabel::IconAndTitle(_) => header_icon_size(row_height) + HEADER_ICON_GAP,
        HeaderLabel::Icon(_) => header_icon_size(row_height),
    }
}

/// Sort priorities as text, so a header build does not format them.
const PRIORITIES: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Digits of equal width (OpenType `tnum`) for the priority number.
static TABULAR_FIGURES: LazyLock<FontFeatures> =
    LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));

/// The tooltip of a sort indicator: "Sorted ascending", and in a
/// multi-column sort "Sorted ascending, priority 2" (`priority` counts from
/// 0, the tooltip from 1).
fn sort_tooltip(direction: SortDirection, priority: Option<usize>) -> SharedString {
    let text = match (direction, priority.map(|priority| priority + 1)) {
        (SortDirection::Ascending, None) => t!("live_grid.sort.ascending"),
        (SortDirection::Descending, None) => t!("live_grid.sort.descending"),
        (SortDirection::Ascending, Some(priority)) => {
            t!("live_grid.sort.ascending_priority", priority = priority)
        }
        (SortDirection::Descending, Some(priority)) => {
            t!("live_grid.sort.descending_priority", priority = priority)
        }
    };
    SharedString::from(text.into_owned())
}

/// A sorted column's direction on its header node: how a screen reader
/// learns the sort (GPUI has no API for announcements).
fn sort_direction(direction: SortDirection) -> accesskit::SortDirection {
    match direction {
        SortDirection::Ascending => accesskit::SortDirection::Ascending,
        SortDirection::Descending => accesskit::SortDirection::Descending,
    }
}

/// The sort indicator of a header cell: the direction icon (`"sort-indicator"`)
/// and, in a multi-column sort, the priority (`"sort-priority"`). Direction is
/// carried by the icon's shape and priority by a digit, never by color alone;
/// the icon's tooltip says both in words.
fn sort_indicator(direction: SortDirection, priority: Option<usize>, color: Hsla) -> AnyElement {
    let icon = match direction {
        SortDirection::Ascending => IconName::SortAscending,
        SortDirection::Descending => IconName::SortDescending,
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .child(
            div()
                .id("sort-indicator")
                .test_support()
                .flex_none()
                .tooltip(move |window, cx| {
                    Tooltip::new(sort_tooltip(direction, priority)).build(window, cx)
                })
                .child(Icon::new(icon).size_3().text_color(color)),
        )
        .when_some(priority, |this, priority| {
            let text: SharedString = match PRIORITIES.get(priority) {
                Some(text) => SharedString::new_static(text),
                None => (priority + 1).to_string().into(),
            };
            this.child(
                div()
                    .id("sort-priority")
                    .test_support()
                    .flex_none()
                    .text_xs()
                    .font_features(TABULAR_FIGURES.clone())
                    .child(text),
            )
        })
        .into_any_element()
}

/// Half of a resize handle: 8 px across the column edge, like `DataTable`.
const HANDLE_HALF: Pixels = px(4.);

/// The font and size of header labels. The header inherits them from the
/// grid's text style; autofit measures titles with them. A header font of
/// its own goes here, and the header applies it.
pub(super) fn header_label_style(window: &Window) -> (Font, Pixels) {
    let style = window.text_style();
    (style.font(), style.font_size.to_pixels(window.rem_size()))
}

/// Widths of header titles by column, shaped with the header font outside
/// the body's text cache (so that cache is not split by font). Used by
/// autofit and by the tooltip of a truncated title.
#[derive(Default)]
pub(super) struct HeaderLabels {
    font: Option<Font>,
    font_size: Pixels,
    widths: Vec<Option<(SharedString, Pixels)>>,
}

impl HeaderLabels {
    /// The width of `title`, cached per column until the font, the size or
    /// the title change.
    pub(super) fn width(
        &mut self,
        column: ColumnIx,
        title: &SharedString,
        font: &Font,
        font_size: Pixels,
        window: &Window,
    ) -> Pixels {
        if self.font.as_ref() != Some(font) || self.font_size != font_size {
            self.widths.clear();
            self.font = Some(font.clone());
            self.font_size = font_size;
        }
        if self.widths.len() <= column.get() {
            self.widths.resize(column.get() + 1, None);
        }
        if let Some((cached, width)) = &self.widths[column.get()]
            && cached == title
        {
            return *width;
        }
        let width = if title.is_empty() {
            px(0.)
        } else {
            let runs = [TextRun {
                len: title.len(),
                font: font.clone(),
                color: black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }];
            window
                .text_system()
                .layout_line(title, font_size, &runs, None)
                .width
        };
        self.widths[column.get()] = Some((title.clone(), width));
        width
    }
}

/// What a column drag moves, by identity: the drop recomputes the columns
/// from it, so a layout change during the drag cannot retarget it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DragSource {
    Column(ColumnIx),
    Band { band: &'static str, first: ColumnIx },
}

/// The payload of a header drag; the group panel takes it too (`pub(crate)`
/// until an application's own panel needs it). Renders the drag ghost in the
/// header's font.
#[derive(Clone)]
pub(crate) struct DraggedColumns {
    grid: EntityId,
    source: DragSource,
    zone: ColumnZone,
    title: SharedString,
    width: Pixels,
    height: Pixels,
    text: GhostTextStyle,
}

impl Render for DraggedColumns {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        self.text.root().child(
            div()
                .h(self.height)
                .w(self.width)
                .min_w(px(100.))
                .max_w(px(450.))
                .px(CELL_PADDING)
                .flex()
                .items_center()
                .overflow_hidden()
                .bg(theme.table_head)
                .text_color(theme.table_head_foreground)
                .border_1()
                .border_color(theme.border)
                .shadow_md()
                .opacity(0.9)
                .child(div().min_w(px(0.)).truncate().child(self.title.clone())),
        )
    }
}

/// The payload of a resize drag; it draws nothing.
#[derive(Clone)]
pub(crate) struct ResizingColumn {
    grid: EntityId,
    column: ColumnIx,
}

impl Render for ResizingColumn {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Where a dragged block would land.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DropTarget {
    /// At `gap` of its zone order; the indicator is at `x` from the body's
    /// left edge.
    Gap { gap: usize, x: Pixels },
    /// Where it already is: no indicator, nothing happens on release.
    Here,
    /// Outside its zone or the grid, or refused: the "not allowed" cursor.
    Outside,
}

/// A column gesture in progress. While there is one, the pointer over the
/// body does not count as hovering rows (`ReorderContext::is_pointer_inside`
/// is false), so dragging a header over the body never holds the order.
pub(super) enum ColumnGesture {
    Resize {
        column: ColumnIx,
        start_x: Pixels,
        start_width: Pixels,
        resized: bool,
    },
    Move {
        source: DragSource,
        zone: ColumnZone,
        target: DropTarget,
        cursor: Option<CursorStyle>,
    },
}

/// The header's interaction state.
#[derive(Default)]
pub(super) struct HeaderState {
    pub(super) gesture: Option<ColumnGesture>,
    /// The handle of the last first press: a double click autofits the
    /// column of its first press (as a double click on a row).
    handle_press: Option<ColumnIx>,
    /// The resize handle under the pointer: its hairline is highlighted.
    hovered_handle: Option<ColumnIx>,
    /// Scrolls the scrolling zone while a header is dragged near its edge.
    auto_scroll: AutoScroll,
    pub(super) labels: HeaderLabels,
    /// The last primary press on the header closed the context menu or
    /// opened it (a Ctrl+click on macOS): its click does not sort, whatever
    /// the modifiers when the button goes up.
    menu_press: bool,
}

/// What a header position hits. The header menu finds its target with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderHit {
    /// A column's header cell.
    Cell(ColumnIx),
    /// A band cell, with the first visible column of its run.
    Band { band: &'static str, first: ColumnIx },
    /// A resize handle, by the column it resizes.
    ResizeHandle(ColumnIx),
    /// The header right of the columns.
    Empty,
}

/// The target of a header press: the column of a cell, a band (its first
/// visible column) or a handle; `Empty` right of the columns.
fn header_target<K, C: GridColumns>(hit: Option<HeaderHit>) -> GridTarget<K, C> {
    match hit {
        Some(HeaderHit::Cell(ix) | HeaderHit::ResizeHandle(ix)) => GridTarget::Header {
            column: column_of(ix),
        },
        Some(HeaderHit::Band { first, .. }) => GridTarget::Header {
            column: column_of(first),
        },
        Some(HeaderHit::Empty) | None => GridTarget::Empty,
    }
}

/// The horizontal extents (`x`, `width` from the body's left edge) that clip
/// each zone's resize handles. A handle on the edge of a frozen zone belongs
/// to the frozen zone, so frozen extents reach half a handle outward.
fn handle_clips(geometry: &ZoneGeometry, viewport: Pixels) -> [(Pixels, Pixels); 3] {
    let [left, _, right] = &geometry.zones;
    let left_end = if left.width > px(0.) {
        left.width + HANDLE_HALF
    } else {
        px(0.)
    };
    let right_start = if right.width > px(0.) {
        right.x - HANDLE_HALF
    } else {
        viewport
    };
    [
        (px(0.), left_end),
        (left_end, (right_start - left_end).max(px(0.))),
        (right_start, (viewport - right_start).max(px(0.))),
    ]
}

/// The column edge a column's handle sits on, from the body's left edge:
/// the right edge, except in the right zone, which is pinned to the body's
/// right edge and so is resized from the left.
fn handle_edge(frame: &ZoneFrame, columns: &ZoneColumns, i: usize) -> Pixels {
    match frame.zone {
        ColumnZone::FrozenRight => frame.column_x(columns, i),
        _ => frame.column_x(columns, i) + columns.width(i),
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// The header's height: one grid row per band level plus the columns.
    pub(super) fn header_height(&self) -> Pixels {
        self.row_height * (self.columns.depth() + 1) as f32
    }

    /// Where the body starts below the grid's top: under the group panel and
    /// the header. The scrollbars and the progress overlay sit there.
    pub(super) fn body_top(&self) -> Pixels {
        self.group_panel_height() + self.header_height()
    }

    /// The width the header lays its zones out in: the body's, or before
    /// the first paint the columns' own.
    fn header_viewport(&self) -> Pixels {
        if self.body_bounds.size.width > px(0.) {
            self.body_bounds.size.width
        } else {
            self.columns.total_width()
        }
    }

    /// The header, with the tooltip of the hovered cell (from
    /// `header_tooltip`, asked for that cell only); a header drag's ghost
    /// draws in `ghost_text`. `a11y`: the frame builds the accessibility
    /// tree, so the cells set what a div has no `aria_*` for (sort
    /// direction, band span) on their nodes.
    pub(super) fn render_header(
        &mut self,
        tooltip: Option<(ColumnIx, CellTooltip)>,
        ghost_text: &GhostTextStyle,
        a11y: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let started = profile_start();
        let header = self.build_header(tooltip, ghost_text, a11y, cx);
        self.stats.last_header_render = profile_elapsed(started);
        header
    }

    fn build_header(
        &self,
        tooltip: Option<(ColumnIx, CellTooltip)>,
        ghost_text: &GhostTextStyle,
        a11y: bool,
        cx: &Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let viewport = self.header_viewport();
        let geometry = self.columns.geometry(viewport, self.scroll_x);
        let row_height = self.row_height;
        let height = self.header_height();
        let right_zone = geometry.zone(ColumnZone::FrozenRight).width > px(0.);
        let (resizing, moving) = match &self.header.gesture {
            Some(ColumnGesture::Resize { column, .. }) => (Some(*column), false),
            Some(ColumnGesture::Move { .. }) => (None, true),
            None => (None, false),
        };
        let sort = self.view.sort();
        let icon_size = header_icon_size(row_height);
        // A column the user groups by but does not move is dragged too while
        // the group panel shows; it lands only there (`drag_members`).
        let panel = self.group_panel.is_shown();
        let grid = cx.entity_id();
        let this = cx.weak_entity();
        let drag = |source: DragSource, zone: ColumnZone, title: SharedString, width: Pixels| {
            (
                DraggedColumns {
                    grid,
                    source,
                    zone,
                    title,
                    width,
                    height: row_height,
                    text: ghost_text.clone(),
                },
                {
                    let this = this.clone();
                    move |drag: &DraggedColumns,
                          _: Point<Pixels>,
                          _: &mut Window,
                          cx: &mut gpui_kit::App| {
                        let _ = this.update(cx, |state, cx| {
                            state.begin_column_move(drag.source, drag.zone, cx)
                        });
                        cx.new(|_| drag.clone())
                    }
                },
            )
        };

        let cells = ColumnZone::ALL.map(|zone| {
            let frame = geometry.zone(zone);
            let columns = self.columns.zone_columns(zone);
            div()
                .absolute()
                .top_0()
                .left(frame.x)
                .w(frame.width)
                .h_full()
                .overflow_hidden()
                .children(frame.columns.clone().map(|i| {
                    let ix = columns.columns[i];
                    let column = self.columns.column(ix);
                    let x = columns.offsets[i] - frame.scroll;
                    let width = columns.width(i);
                    let top = row_height * self.columns.bands_of(ix).len() as f32;
                    // The zone's dividers own its outer edges (Design Guides,
                    // "Hairlines belong on the boundary owner").
                    let right_line = match zone {
                        ColumnZone::FrozenLeft => i + 1 < columns.len(),
                        ColumnZone::Scrolling => !(right_zone && x + width >= frame.width),
                        ColumnZone::FrozenRight => false,
                    };
                    let left_line = zone == ColumnZone::FrozenRight && i > 0;
                    let end = column.align == ColumnAlign::End;
                    let tooltip = tooltip
                        .as_ref()
                        .filter(|(hovered, _)| *hovered == ix)
                        .map(|(_, tooltip)| tooltip.clone());
                    let position = sort.iter().position(|(sorted, _)| sorted.ix() == ix);
                    let direction = position.map(|position| sort[position].1);
                    let indicator = position.map(|position| {
                        let priority = (sort.len() >= 2).then_some(position);
                        sort_indicator(sort[position].1, priority, theme.table_head_foreground)
                    });
                    // The header's icon, before the title or alone. The
                    // title stays the cell's accessible name.
                    let icon = column.icon().map(|icon| {
                        div()
                            .id(HEADER_ICON_ID)
                            .test_support()
                            .flex_none()
                            .size(icon_size)
                            .child(
                                Icon::empty()
                                    .path(icon.clone())
                                    .size(icon_size)
                                    .text_color(theme.table_head_foreground),
                            )
                    });
                    let icon_only = matches!(column.header_label, HeaderLabel::Icon(_));
                    // Unsorted and without an icon, one element per cell:
                    // the title is a block one line tall, so it truncates
                    // with an ellipsis. Otherwise the title truncates and the
                    // icon and the indicator keep their widths: the icon
                    // before the title, the indicator after a start-aligned
                    // title and before an end-aligned one, so a number
                    // column's title stays over its numbers. An icon alone
                    // is centered unless the column is aligned.
                    div()
                        .id(column.column.id())
                        .test_support()
                        .role(Role::ColumnHeader)
                        .aria_label(column.title.clone())
                        .when_some(self.columns.view_column_ix(ix), |this, view| {
                            this.aria_column_index(column_index(view))
                        })
                        .when_some(direction.filter(|_| a11y), |this, direction| {
                            this.a11y_synthetic_children(move |builder| {
                                builder
                                    .parent_node()
                                    .set_sort_direction(sort_direction(direction))
                            })
                        })
                        .absolute()
                        .top(top)
                        .left(x)
                        .w(width)
                        .h(height - top)
                        .px(CELL_PADDING)
                        .line_height(height - top)
                        .truncate()
                        .border_color(theme.table_row_border)
                        .when(right_line, |this| this.border_r_1())
                        .when(left_line, |this| this.border_l_1())
                        .when(end, |this| this.text_right())
                        .when(column.movable || (panel && column.groupable), |this| {
                            let (value, constructor) =
                                drag(DragSource::Column(ix), zone, column.title.clone(), width);
                            this.on_drag(value, constructor)
                        })
                        .when(column.sortable, |this| {
                            this.on_click(cx.listener(move |state, event: &ClickEvent, _, cx| {
                                state.on_header_click(ix, event, cx)
                            }))
                        })
                        .on_hover(cx.listener(move |state, hovered: &bool, _, cx| {
                            state.on_header_cell_hover(ix, *hovered, cx)
                        }))
                        .when_some(tooltip, |this, tooltip| {
                            this.tooltip(move |window, cx| tooltip.clone().into_view(window, cx))
                        })
                        .map(|this| match (icon, indicator) {
                            (None, None) => this.child(column.title.clone()),
                            (icon, indicator) => {
                                let title = (!icon_only).then(|| {
                                    div().min_w(px(0.)).truncate().child(column.title.clone())
                                });
                                let center = icon_only && column.align == ColumnAlign::Auto;
                                // `SORT_GAP` and `HEADER_ICON_GAP` are both 4 px.
                                this.flex()
                                    .items_center()
                                    .gap(SORT_GAP)
                                    .when(end, |this| this.justify_end())
                                    .when(center, |this| this.justify_center())
                                    .map(|this| {
                                        if end {
                                            this.children(indicator).children(icon).children(title)
                                        } else {
                                            this.children(icon).children(title).children(indicator)
                                        }
                                    })
                            }
                        })
                }))
        });

        let bands = (self.columns.depth() > 0).then(|| {
            ColumnZone::ALL.map(|zone| {
                let frame = geometry.zone(zone);
                let columns = self.columns.zone_columns(zone);
                div()
                    .absolute()
                    .top_0()
                    .left(frame.x)
                    .w(frame.width)
                    .h_full()
                    .overflow_hidden()
                    .children(
                        self.columns
                            .runs()
                            .iter()
                            .filter(|run| {
                                run.zone == zone
                                    && run.columns.start < frame.columns.end
                                    && frame.columns.start < run.columns.end
                            })
                            .map(|run| {
                                let first = columns.columns[run.columns.start];
                                let x = columns.offsets[run.columns.start] - frame.scroll;
                                let width = columns.offsets[run.columns.end]
                                    - columns.offsets[run.columns.start];
                                // The title centers in the part of the band on screen.
                                let label_left = x.max(px(0.));
                                let label_right = (x + width).min(frame.width);
                                let first_id: SharedString =
                                    <ColumnOf<D> as GridColumns>::all()[first.get()].id().into();
                                let span = run.columns.len();
                                div()
                                    .id(ElementId::NamedChild(
                                        Arc::new(ElementId::Name(run.band.into())),
                                        first_id,
                                    ))
                                    .test_support()
                                    // A column header spanning the run's
                                    // visible columns, from the first.
                                    .role(Role::ColumnHeader)
                                    .aria_label(run.title.clone())
                                    .when_some(self.columns.view_column_ix(first), |this, view| {
                                        this.aria_column_index(column_index(view))
                                    })
                                    .when(a11y, |this| {
                                        this.a11y_synthetic_children(move |builder| {
                                            builder.parent_node().set_column_span(span)
                                        })
                                    })
                                    .absolute()
                                    .top(row_height * run.level as f32)
                                    .left(x)
                                    .w(width)
                                    .h(row_height)
                                    .border_b_1()
                                    .border_r_1()
                                    .border_color(theme.table_row_border)
                                    .when(run.movable, |this| {
                                        let (value, constructor) = drag(
                                            DragSource::Band {
                                                band: run.band,
                                                first,
                                            },
                                            zone,
                                            run.title.clone(),
                                            width,
                                        );
                                        this.on_drag(value, constructor)
                                    })
                                    .child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .h_full()
                                            .left(label_left - x)
                                            .w((label_right - label_left).max(px(0.)))
                                            .px(CELL_PADDING)
                                            .line_height(row_height)
                                            .text_center()
                                            .truncate()
                                            .child(run.title.clone()),
                                    )
                            }),
                    )
            })
        });

        // No handles while a header is dragged: nothing then blocks the drop
        // on the grid, wherever the pointer is released.
        let clips = handle_clips(&geometry, viewport);
        let handles = (!moving).then(|| {
            ColumnZone::ALL.map(|zone| {
                let frame = geometry.zone(zone);
                let columns = self.columns.zone_columns(zone);
                let (clip_x, clip_width) = clips[zone.slot()];
                // The hairline of the hovered or resized handle turns to
                // `border`, over the cell's own line (one element, not one
                // per handle).
                let lit = resizing.or(self.header.hovered_handle);
                let line = frame.columns.clone().find_map(|i| {
                    let ix = columns.columns[i];
                    (Some(ix) == lit).then(|| {
                        let edge = handle_edge(frame, columns, i);
                        let x = match zone {
                            ColumnZone::FrozenRight => edge,
                            _ => edge - px(1.),
                        };
                        let top = row_height * self.columns.bands_of(ix).len() as f32;
                        div()
                            .absolute()
                            .left(x - clip_x)
                            .top(top)
                            .w(px(1.))
                            .h(height - top)
                            .bg(theme.border)
                    })
                });
                div()
                    .absolute()
                    .top_0()
                    .left(clip_x)
                    .w(clip_width)
                    .h_full()
                    .overflow_hidden()
                    .children(line)
                    .children(frame.columns.clone().filter_map(|i| {
                        let ix = columns.columns[i];
                        let column = self.columns.column(ix);
                        if !column.resizable {
                            return None;
                        }
                        let edge = handle_edge(frame, columns, i);
                        let top = row_height * self.columns.bands_of(ix).len() as f32;
                        Some(
                            div()
                                .id(column.column.id())
                                .test_support()
                                .absolute()
                                .left(edge - clip_x - HANDLE_HALF)
                                .top(top)
                                .w(HANDLE_HALF * 2.)
                                .h(height - top)
                                .occlude()
                                .cursor_col_resize()
                                .on_hover(cx.listener(move |state, hovered: &bool, _, cx| {
                                    state.on_resize_handle_hover(ix, *hovered, cx)
                                }))
                                // The handle occludes the header: it takes
                                // every press, and its menu is the column's.
                                .on_any_mouse_down(cx.listener(
                                    move |state, event: &MouseDownEvent, window, cx| {
                                        state.on_resize_handle_down(ix, event, window, cx)
                                    },
                                ))
                                .on_drag(ResizingColumn { grid, column: ix }, |drag, _, _, cx| {
                                    cx.new(|_| drag.clone())
                                }),
                        )
                    }))
            })
        });

        let [left, _, right] = &geometry.zones;
        let divider = |x: Pixels| {
            div()
                .absolute()
                .top_0()
                .h_full()
                .left(x)
                .w(px(1.))
                .bg(theme.border)
        };
        div()
            .relative()
            .flex_none()
            .w_full()
            .h(height)
            .overflow_hidden()
            .bg(theme.table_head)
            .text_color(theme.table_head_foreground)
            // A newer order held back by the reorder policy shows as an
            // accent rule under the header.
            .map(|this| {
                if self.held.is_some() {
                    this.border_b_2().border_color(theme.primary)
                } else {
                    this.border_b_1().border_color(theme.border)
                }
            })
            .on_any_mouse_down(cx.listener(Self::on_header_mouse_down))
            .child(
                div()
                    .id("live-grid-header")
                    .test_support()
                    .role(Role::Row)
                    .aria_row_index(HEADER_ROW_INDEX)
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .children(cells)
                    // Over the column cells, as before; inside the header
                    // row, so band cells are column headers of that row.
                    .when_some(bands, |this, bands| {
                        this.child(
                            div()
                                .id("live-grid-bands")
                                .test_support()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .children(bands),
                        )
                    }),
            )
            .when(left.width > px(0.), |this| {
                this.child(divider(left.width - px(1.)))
            })
            .when(right.width > px(0.), |this| this.child(divider(right.x)))
            .child(
                div()
                    .id("live-grid-resize")
                    .test_support()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .when_some(handles, |this, handles| this.children(handles)),
            )
            .when_some(self.drop_gap_x(cx), |this, x| {
                this.child(
                    div()
                        .id("live-grid-drop-gap")
                        .test_support()
                        .absolute()
                        .top_0()
                        .h_full()
                        .left(x - px(1.))
                        .w(px(2.))
                        .bg(theme.drag_border),
                )
            })
    }

    /// The x (from the body's left edge) of the drop gap shown while a
    /// header is dragged.
    pub(super) fn drop_gap_x(&self, cx: &gpui_kit::App) -> Option<Pixels> {
        match &self.header.gesture {
            Some(ColumnGesture::Move {
                target: DropTarget::Gap { x, .. },
                ..
            }) if cx.has_active_drag() => Some(*x),
            _ => None,
        }
    }

    /// What a window position in the header hits: arithmetic over the zone
    /// geometry, so it needs no element (the header menu finds its target
    /// with it).
    pub(crate) fn header_hit(&self, position: Point<Pixels>) -> Option<HeaderHit> {
        let height = self.header_height();
        let viewport = self.header_viewport();
        let origin = point(
            self.body_bounds.origin.x,
            self.body_bounds.origin.y - height,
        );
        if !Bounds::new(origin, size(viewport, height)).contains(&position) {
            return None;
        }
        let x = position.x - origin.x;
        let y = position.y - origin.y;
        let geometry = self.columns.geometry(viewport, self.scroll_x);
        let clips = handle_clips(&geometry, viewport);
        for zone in [
            ColumnZone::FrozenLeft,
            ColumnZone::FrozenRight,
            ColumnZone::Scrolling,
        ] {
            let (clip_x, clip_width) = clips[zone.slot()];
            if x < clip_x || x >= clip_x + clip_width {
                continue;
            }
            let frame = geometry.zone(zone);
            let columns = self.columns.zone_columns(zone);
            for i in frame.columns.clone() {
                let ix = columns.columns[i];
                let edge = handle_edge(frame, columns, i);
                let top = self.row_height * self.columns.bands_of(ix).len() as f32;
                if self.columns.column(ix).resizable
                    && x >= edge - HANDLE_HALF
                    && x < edge + HANDLE_HALF
                    && y >= top
                {
                    return Some(HeaderHit::ResizeHandle(ix));
                }
            }
        }
        let Some(zone) = geometry.zone_at(x) else {
            return Some(HeaderHit::Empty);
        };
        let frame = geometry.zone(zone);
        let columns = self.columns.zone_columns(zone);
        let Some(i) = columns.index_at(x - frame.x + frame.scroll) else {
            return Some(HeaderHit::Empty);
        };
        let ix = columns.columns[i];
        let level = (y / self.row_height).floor() as usize;
        let Some(&band) = self.columns.bands_of(ix).get(level) else {
            return Some(HeaderHit::Cell(ix));
        };
        let first = self
            .columns
            .runs()
            .iter()
            .find(|run| {
                run.zone == zone
                    && run.level == level
                    && run.columns.contains(&i)
                    && run.band == band
            })
            .map_or(ix, |run| columns.columns[run.columns.start]);
        Some(HeaderHit::Band { band, first })
    }

    /// A left click on a sortable column's header cell: the [`SortCycle`]
    /// decides the new sort (`SortOrigin::User`). A double click is two
    /// clicks, two steps of the cycle. A drag (reorder) or a press on a
    /// resize handle never clicks.
    ///
    /// [`SortCycle`]: crate::SortCycle
    fn on_header_click(&mut self, ix: ColumnIx, event: &ClickEvent, cx: &mut Context<Self>) {
        // The press closed the menu, or opened one (the context menu
        // gesture took it). Decided when the button went down: on macOS a
        // Ctrl+click whose Ctrl is let go first ends without Ctrl.
        if self.header.menu_press
            || !event.standard_click()
            || !self.columns.get(ix).is_some_and(|column| column.sortable)
        {
            return;
        }
        let current: Vec<(ColumnIx, SortDirection)> = self
            .view
            .sort()
            .iter()
            .map(|(column, direction)| (column.ix(), *direction))
            .collect();
        let context = SortCycleContext::new(&current, ix, event.modifiers());
        let next = self.sort_cycle.next_sort(&context);
        let columns = <ColumnOf<D> as GridColumns>::all();
        let sort = next
            .into_iter()
            .filter(|(column, _)| column.get() < columns.len())
            .map(|(column, direction)| (column_of::<ColumnOf<D>>(column), direction));
        self.apply_sort(sort, SortOrigin::User, cx);
    }

    /// A press on the header. One the [`ContextMenuGesture`] takes opens
    /// the menu; any other may end in a click on its target, unless it
    /// closes the menu. A primary one focuses the grid (so the column
    /// Actions reach it) and makes the pressed column current. A click's
    /// sorting is left to the cells.
    ///
    /// [`ContextMenuGesture`]: crate::ContextMenuGesture
    fn on_header_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let primary = event.button == MouseButton::Left;
        // The primary press that closes the menu changes nothing else, even
        // a context one; another context press opens a new menu.
        if primary {
            self.header.menu_press = self.is_menu_open();
            if self.header.menu_press {
                return;
            }
        }
        if self.is_context_press(
            event.button,
            event.modifiers,
            event.click_count,
            GridArea::Header,
        ) {
            // A primary context press (Ctrl+click on macOS) does not sort
            // when it ends.
            self.header.menu_press = primary;
            self.on_header_context_down(event, window, cx);
            return;
        }
        // Another button's press that closes the menu is no click either.
        if self.is_menu_open() {
            return;
        }
        let hit = self.header_hit(event.position);
        self.begin_click(header_target(hit), event);
        if !primary {
            return;
        }
        window.focus(&self.focus_handle, cx);
        match hit {
            Some(HeaderHit::Cell(ix)) => self.set_current_column(ix),
            Some(HeaderHit::Band { first, .. }) => self.set_current_column(first),
            _ => {}
        }
    }

    fn on_resize_handle_hover(&mut self, ix: ColumnIx, hovered: bool, cx: &mut Context<Self>) {
        let handle = if hovered {
            Some(ix)
        } else if self.header.hovered_handle == Some(ix) {
            None
        } else {
            return;
        };
        if self.header.hovered_handle != handle {
            self.header.hovered_handle = handle;
            cx.notify();
        }
    }

    /// A context press on the header: the menu of the column under it (a
    /// band's first visible column, a handle's column), or the whole grid's
    /// right of the columns. The selection does not change.
    fn on_header_context_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let target = header_target(self.header_hit(event.position));
        self.open_menu(target, event.position, window, cx);
    }

    fn on_resize_handle_down(
        &mut self,
        ix: ColumnIx,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let primary = event.button == MouseButton::Left;
        if primary && self.is_menu_open() {
            return;
        }
        if self.is_context_press(
            event.button,
            event.modifiers,
            event.click_count,
            GridArea::Header,
        ) {
            self.on_header_context_down(event, window, cx);
            return;
        }
        if !primary {
            return;
        }
        window.focus(&self.focus_handle, cx);
        if event.click_count >= 2 {
            let target = self.header.handle_press.take().unwrap_or(ix);
            let mut columns = crate::model::ColumnSet::EMPTY;
            columns.insert(target);
            self.request_autofit(columns, LayoutOrigin::User, cx);
            return;
        }
        self.header.handle_press = Some(ix);
        self.begin_resize(ix, event.position.x, cx);
    }

    fn begin_resize(&mut self, column: ColumnIx, x: Pixels, cx: &mut Context<Self>) {
        self.header.gesture = Some(ColumnGesture::Resize {
            column,
            start_x: x,
            start_width: self.columns.width(column),
            resized: false,
        });
        self.column_gesture_started(cx);
        cx.notify();
    }

    /// The width follows the pointer from where the press was, so the edge
    /// does not jump when the gesture starts. Repaints at once; the
    /// `LayoutChange` waits for the release.
    pub(super) fn on_resize_drag_move(
        &mut self,
        event: &DragMoveEvent<ResizingColumn>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx);
        if drag.grid != cx.entity_id() {
            return;
        }
        let column = drag.column;
        let x = event.event.position.x;
        let Some(ColumnGesture::Resize {
            column: resized_column,
            start_x,
            start_width,
            ..
        }) = self.header.gesture
        else {
            self.begin_resize(column, x, cx);
            return;
        };
        if resized_column != column {
            return;
        }
        let delta = x - start_x;
        let width = match self.columns.zone_of(column) {
            ColumnZone::FrozenRight => start_width - delta,
            _ => start_width + delta,
        };
        let changed = self.relayout(|layout, _| {
            if layout.set_width(column, width) {
                LayoutDelta::new(LayoutPart::Width, [column])
            } else {
                LayoutDelta::default()
            }
        });
        if changed.is_empty() {
            return;
        }
        self.reveal_right_edge(column);
        self.clamp_scroll();
        if let Some(ColumnGesture::Resize { resized, .. }) = &mut self.header.gesture {
            *resized = true;
        }
        cx.notify();
    }

    fn finish_resize(&mut self, cx: &mut Context<Self>) {
        let Some(ColumnGesture::Resize {
            column,
            start_width,
            resized,
            ..
        }) = self.header.gesture.take()
        else {
            return;
        };
        if resized && self.columns.width(column) != start_width {
            self.emit_layout_change(
                LayoutDelta::new(LayoutPart::Width, [column]),
                LayoutOrigin::User,
                cx,
            );
        }
        cx.notify();
    }

    fn begin_column_move(&mut self, source: DragSource, zone: ColumnZone, cx: &mut Context<Self>) {
        self.header.gesture = Some(ColumnGesture::Move {
            source,
            zone,
            target: DropTarget::Here,
            cursor: None,
        });
        self.column_gesture_started(cx);
        cx.notify();
    }

    /// The pointer stops counting as over the rows: a held order may go.
    /// The press is a drag now, not a click.
    pub(super) fn column_gesture_started(&mut self, cx: &mut Context<Self>) {
        self.cancel_click();
        if self.pointer_inside && self.held.is_some() {
            self.reconsider_held(cx);
        }
    }

    /// The columns a drag moves now, from its identity; `None` if they were
    /// hidden or left the zone since the drag began, or if the column is
    /// not movable (dragged only to the group panel).
    fn drag_members(&self, source: DragSource, zone: ColumnZone) -> Option<Vec<ColumnIx>> {
        match source {
            DragSource::Column(ix) => (self.columns.column(ix).movable
                && self.columns.is_visible(ix)
                && self.columns.zone_of(ix) == zone)
                .then(|| vec![ix]),
            DragSource::Band { band, first } => {
                if !self.columns.is_visible(first) || self.columns.zone_of(first) != zone {
                    return None;
                }
                let members = self.columns.band_segment(zone, band, first);
                (!members.is_empty()).then_some(members)
            }
        }
    }

    /// Where a block dropped at `position` would land. Candidates are the
    /// boundaries between the zone's visible columns on screen: first the
    /// one after the last column whose center is left of the pointer, else
    /// the nearest one the move policy allows.
    fn drop_target_at(
        &self,
        source: DragSource,
        zone: ColumnZone,
        position: Point<Pixels>,
    ) -> DropTarget {
        let Some(members) = self.drag_members(source, zone) else {
            return DropTarget::Outside;
        };
        let x = position.x - self.body_bounds.origin.x;
        let geometry = self
            .columns
            .geometry(self.body_bounds.size.width, self.scroll_x);
        if geometry.zone_at(x) != Some(zone) {
            return DropTarget::Outside;
        }
        let frame = geometry.zone(zone);
        let columns = self.columns.zone_columns(zone);
        let shift = self.columns.zone_move(zone, members);
        let policy = &*self.column_move;
        let mut first = frame.columns.start;
        for i in frame.columns.clone() {
            if x < frame.column_x(columns, i) + columns.width(i) / 2. {
                break;
            }
            first = i + 1;
        }
        let resolved = match shift.resolve(first, policy) {
            Boundary::Refused => (frame.columns.start..=frame.columns.end)
                .filter_map(|k| match shift.resolve(k, policy) {
                    Boundary::Refused => None,
                    boundary => Some((k, boundary)),
                })
                .min_by(|(a, _), (b, _)| {
                    let distance = |k: usize| f32::from((frame.column_x(columns, k) - x).abs());
                    distance(*a).total_cmp(&distance(*b))
                }),
            boundary => Some((first, boundary)),
        };
        match resolved {
            Some((k, Boundary::Gap(gap))) => DropTarget::Gap {
                gap,
                x: frame.column_x(columns, k),
            },
            Some((_, Boundary::Here)) => DropTarget::Here,
            _ => DropTarget::Outside,
        }
    }

    /// Recomputes the drop target of the drag at `position`. Returns the
    /// target, or `None` if there is no header drag.
    fn refresh_drop_target(
        &mut self,
        position: Point<Pixels>,
        inside: bool,
        cx: &mut Context<Self>,
    ) -> Option<DropTarget> {
        let Some(ColumnGesture::Move { source, zone, .. }) = self.header.gesture else {
            return None;
        };
        let target = if inside {
            self.drop_target_at(source, zone, position)
        } else {
            DropTarget::Outside
        };
        if let Some(ColumnGesture::Move {
            target: current, ..
        }) = &mut self.header.gesture
            && *current != target
        {
            *current = target;
            cx.notify();
        }
        Some(target)
    }

    pub(super) fn on_columns_drag_move(
        &mut self,
        event: &DragMoveEvent<DraggedColumns>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx);
        if drag.grid != cx.entity_id() {
            return;
        }
        let (source, zone) = (drag.source, drag.zone);
        if !matches!(self.header.gesture, Some(ColumnGesture::Move { .. })) {
            self.begin_column_move(source, zone, cx);
        }
        let position = event.event.position;
        let inside = event.bounds.contains(&position);
        // Over the group panel the drag groups by the column: it moves no
        // column and shows no insertion line in the header and the body.
        let panel = self.refresh_panel_column_target(source, inside.then_some(position), cx);
        let Some(target) = self.refresh_drop_target(position, inside && panel.is_none(), cx) else {
            return;
        };
        let refused = match panel {
            Some(panel) => !panel.is_accepted(),
            None => target == DropTarget::Outside,
        };
        let cursor = if refused {
            CursorStyle::OperationNotAllowed
        } else {
            CursorStyle::ClosedHand
        };
        if let Some(ColumnGesture::Move { cursor: shown, .. }) = &mut self.header.gesture
            && *shown != Some(cursor)
        {
            *shown = Some(cursor);
            cx.set_active_drag_cursor_style(cursor, window);
        }

        // Near or past an edge of the scrolling zone, scroll it; not over
        // the group panel.
        let vertical = position.y >= event.bounds.top() && position.y < event.bounds.bottom();
        let delta = (zone == ColumnZone::Scrolling && vertical && panel.is_none())
            .then(|| {
                let frame = self
                    .columns
                    .geometry(self.body_bounds.size.width, self.scroll_x)
                    .zone(ColumnZone::Scrolling)
                    .clone();
                // `compute_delta` works along y: give it the zone's extent.
                let extent = Bounds::new(
                    point(px(0.), self.body_bounds.origin.x + frame.x),
                    size(px(0.), frame.width),
                );
                (frame.width > px(0.))
                    .then(|| AutoScroll::compute_delta(position.x, extent))
                    .flatten()
            })
            .flatten();
        self.header.auto_scroll.last_drag_position = Some(position);
        self.header.auto_scroll.set(delta, cx, |delta, state, cx| {
            state.auto_scroll_columns(delta, cx)
        });
    }

    /// One auto-scroll step of a header drag; the drop target follows the
    /// last pointer position.
    fn auto_scroll_columns(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        if !matches!(self.header.gesture, Some(ColumnGesture::Move { .. })) {
            self.header.auto_scroll.stop();
            return;
        }
        let before = self.scroll_x;
        self.scroll_x = (self.scroll_x + delta).max(px(0.));
        self.clamp_scroll();
        if self.scroll_x == before {
            return;
        }
        if let Some(position) = self.header.auto_scroll.last_drag_position {
            self.refresh_drop_target(position, true, cx);
        }
        cx.notify();
    }

    pub(super) fn on_columns_drop(
        &mut self,
        drag: &DraggedColumns,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if drag.grid != cx.entity_id() {
            return;
        }
        let (source, zone) = (drag.source, drag.zone);
        let position = window.mouse_position();
        // Dropped on the group panel: grouped by, never moved.
        if self.is_over_group_panel(position) {
            self.end_column_move();
            self.drop_column_on_group_panel(source, position, cx);
            cx.notify();
            return;
        }
        let target = self.drop_target_at(source, zone, position);
        self.end_column_move();
        if let DropTarget::Gap { gap, .. } = target
            && let Some(members) = self.drag_members(source, zone)
        {
            self.change_layout(LayoutOrigin::User, cx, |layout, _| {
                if layout.move_members(zone, &members, gap) {
                    LayoutDelta::new(LayoutPart::Order, members.iter().copied()).revealing()
                } else {
                    LayoutDelta::default()
                }
            });
        }
        cx.notify();
    }

    fn end_column_move(&mut self) {
        if matches!(self.header.gesture, Some(ColumnGesture::Move { .. })) {
            self.header.gesture = None;
        }
        self.header.auto_scroll.stop();
        self.clear_panel_target();
    }

    /// The left button went up anywhere in the window. A resize ends here
    /// with its one `LayoutChange`. A header drag ends in the grid's drop
    /// handler, which runs later in this dispatch; if the release is outside
    /// the grid, nothing drops and the deferred cancel ends it. So does a
    /// chip drag of the group panel.
    pub(crate) fn on_column_mouse_up(&mut self, cx: &mut Context<Self>) {
        self.on_level_drag_mouse_up(cx);
        match self.header.gesture {
            Some(ColumnGesture::Resize { .. }) => self.finish_resize(cx),
            Some(ColumnGesture::Move { .. }) => {
                let this = cx.weak_entity();
                cx.defer(move |cx| {
                    let _ = this.update(cx, |state, cx| {
                        if matches!(state.header.gesture, Some(ColumnGesture::Move { .. })) {
                            state.end_column_move();
                            cx.notify();
                        }
                    });
                });
            }
            None => {}
        }
    }

    /// Returns true while a column gesture (resize, header drag, or a chip
    /// drag of the group panel) is in progress.
    pub(super) fn is_column_gesture_active(&self) -> bool {
        self.header.gesture.is_some() || self.is_level_dragged()
    }
}

#[cfg(test)]
mod tests {
    use super::sort_tooltip;
    use crate::engine::SortDirection::{Ascending, Descending};

    #[test]
    fn test_sort_tooltip() {
        // The priority counts from 1 and shows only in a multi-column sort.
        assert_eq!(sort_tooltip(Ascending, None), "Sorted ascending");
        assert_eq!(sort_tooltip(Descending, None), "Sorted descending");
        assert_eq!(
            sort_tooltip(Ascending, Some(0)),
            "Sorted ascending, priority 1"
        );
        assert_eq!(
            sort_tooltip(Descending, Some(1)),
            "Sorted descending, priority 2"
        );
    }
}
