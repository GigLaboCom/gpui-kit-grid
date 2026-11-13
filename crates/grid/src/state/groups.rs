//! Group captions on screen: a caption line of the frame, its texts from the
//! catalog, what the `group_caption` hook knows, and where a press on a
//! caption lands.
//!
//! A caption is the header of its rows (`table_head`, no stripes). Its
//! chevron and text sit in a lane of their own over the zones, at the
//! body's left edge, so they read at any horizontal scroll; the aggregates
//! sit under their columns, in their zones, and scroll with them. The text
//! ends at the first aggregate cell on screen that starts at least
//! [`CAPTION_MIN_WIDTH`] past it; aggregates left of that are not painted
//! on the caption.
//!
//! What changed shows on a caption, expanded or collapsed: an aggregate
//! that changed in its cell, a new group or a new row count in the caption
//! lane, from the projection's stamps through the change paint, as a data
//! cell's change.

use std::{fmt::Write as _, marker::PhantomData, ops::Range};

use gpui_kit::{Bounds, FontWeight, Hsla, Pixels, Point, SharedString, Window, point, px, size};
use rust_i18n::t;

use super::{
    CELL_PADDING, CellStyle, ColumnAlign, LiveGridDelegate, LiveGridState, ViewColumnIx,
    a11y::{A11yTexts, PaintedRow, RowContent, RowOutline},
    change::{ChangePainter as _, stamp_change},
    disclosure::INDENT,
    frame::{
        CellText, CellTextContext, LEAD_ICON_LANE, RowPlanner, focus_outline, place_cell_text,
    },
    group_hash,
    style::{AggregateLine, AggregateStyleContext},
    tooltip::{CellTooltip, HoveredCell},
};
use crate::{
    element::{IconItem, Region},
    engine::{Aggregate, GroupInfo, GroupPath},
    highlight::Stamp,
    model::{
        CellValue, ColumnIx, GridColumns as _, GridRow, OwnedValue, display_text, display_text_in,
    },
};

/// The least room a caption's text keeps before the first aggregate cell
/// painted on its line.
pub(super) const CAPTION_MIN_WIDTH: Pixels = px(120.);

/// The room on each side of a caption tag's text.
const TAG_PADDING: Pixels = px(5.);
/// The corner radius of a caption tag.
const TAG_RADIUS: Pixels = px(3.);
/// The gap between a caption tag and the caption's text.
const TAG_GAP: Pixels = px(6.);
/// A caption tag's height, as a share of the row height: 18 px in 26 px
/// rows, 22 px in 32 px rows.
const TAG_HEIGHT: f32 = 0.68;

/// A tag before a group caption's text: a pill in the application's
/// colours, such as a transit line's, whose text
/// [`LiveGridDelegate::group_caption_tag`] writes.
///
/// [`LiveGridDelegate::group_caption_tag`]: super::LiveGridDelegate::group_caption_tag
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptionTag {
    background: Hsla,
    foreground: Hsla,
    font_weight: Option<FontWeight>,
    min_width: Pixels,
}

impl CaptionTag {
    /// A tag of `foreground` text on `background`, in the grid's font, as
    /// wide as its text.
    pub fn new(background: Hsla, foreground: Hsla) -> Self {
        Self {
            background,
            foreground,
            font_weight: None,
            min_width: px(0.),
        }
    }

    /// Sets the weight of the tag's text, such as `FontWeight::BOLD`.
    pub fn with_font_weight(mut self, weight: FontWeight) -> Self {
        self.font_weight = Some(weight);
        self
    }

    /// Sets the least width of the tag: tags of short texts line up. The
    /// text is centred in it.
    pub fn with_min_width(mut self, width: Pixels) -> Self {
        self.min_width = width;
        self
    }

    /// The tag's colour.
    pub fn background(&self) -> Hsla {
        self.background
    }

    /// The colour of the tag's text.
    pub fn foreground(&self) -> Hsla {
        self.foreground
    }

    /// The weight of the tag's text; `None` is the grid's font.
    pub fn font_weight(&self) -> Option<FontWeight> {
        self.font_weight
    }

    /// The least width of the tag.
    pub fn min_width(&self) -> Pixels {
        self.min_width
    }
}

/// The tooltip slot of a caption's text: no column has this index.
const CAPTION_COLUMN: ColumnIx = ColumnIx::new(u16::MAX);

/// The element id name of a caption text's tooltip div (`NamedInteger(this,
/// group_hash)`).
const CAPTION_TOOLTIP_ID: &str = "live-grid-group";

/// The colours of a caption: the header's.
#[derive(Clone, Copy, Debug)]
pub(super) struct CaptionPalette {
    pub(super) background: Hsla,
    pub(super) foreground: Hsla,
    pub(super) chevron: Hsla,
}

/// A part of a catalog template.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Text(Range<usize>),
    Title,
    Value,
    Count,
    Name,
}

/// A catalog text with `%{…}` placeholders, parsed once: filling it writes
/// into a buffer and allocates nothing more.
#[derive(Clone, Debug, Default)]
struct Template {
    text: String,
    parts: Vec<Part>,
}

impl Template {
    fn parse(text: String) -> Self {
        let mut parts = Vec::new();
        let mut literal = 0;
        let mut rest = 0;
        while let Some(start) = text[rest..].find("%{").map(|at| rest + at) {
            let Some(end) = text[start..].find('}').map(|at| start + at) else {
                break;
            };
            let part = match &text[start + 2..end] {
                "title" => Part::Title,
                "value" => Part::Value,
                "count" => Part::Count,
                "name" => Part::Name,
                // An unknown placeholder stays as text.
                _ => {
                    rest = end + 1;
                    continue;
                }
            };
            if start > literal {
                parts.push(Part::Text(literal..start));
            }
            parts.push(part);
            literal = end + 1;
            rest = end + 1;
        }
        if literal < text.len() {
            parts.push(Part::Text(literal..text.len()));
        }
        Self { text, parts }
    }

    /// Writes the template into `out`, each placeholder by `fill`.
    fn write(&self, out: &mut String, mut fill: impl FnMut(&Part, &mut String)) {
        for part in &self.parts {
            match part {
                Part::Text(range) => out.push_str(&self.text[range.clone()]),
                part => fill(part, out),
            }
        }
    }
}

/// The texts of captions, aggregates and expansion states, from this crate's
/// catalog: read once per locale change, never per frame or cell.
#[derive(Debug, Default)]
pub(crate) struct CaptionTexts {
    /// The locale the texts are of; empty before the first read.
    locale: String,
    caption: Template,
    blank: SharedString,
    yes: SharedString,
    no: SharedString,
    sum: SharedString,
    avg: SharedString,
    min: SharedString,
    max: SharedString,
    count: SharedString,
    tooltip: Template,
    a11y: A11yTexts,
}

impl CaptionTexts {
    /// Reads the texts again if the locale changed since the last read.
    pub(super) fn refresh(&mut self) {
        let locale = rust_i18n::locale();
        if *locale == *self.locale {
            return;
        }
        let text = |key: &str| SharedString::from(t!(key, locale = &*locale).into_owned());
        *self = Self {
            locale: locale.to_string(),
            caption: Template::parse(t!("live_grid.group.caption", locale = &*locale).into_owned()),
            blank: text("live_grid.group.blank"),
            yes: text("live_grid.group.true"),
            no: text("live_grid.group.false"),
            sum: text("live_grid.aggregate.sum"),
            avg: text("live_grid.aggregate.avg"),
            min: text("live_grid.aggregate.min"),
            max: text("live_grid.aggregate.max"),
            count: text("live_grid.aggregate.count"),
            tooltip: Template::parse(
                t!("live_grid.aggregate.tooltip", locale = &*locale).into_owned(),
            ),
            a11y: A11yTexts {
                rising: text("live_grid.a11y.up"),
                falling: text("live_grid.a11y.down"),
                expanded: text("live_grid.a11y.expanded"),
                collapsed: text("live_grid.a11y.collapsed"),
                link: text("live_grid.a11y.link"),
            },
        };
    }

    /// The texts of the accessibility plan.
    pub(super) fn a11y(&self) -> &A11yTexts {
        &self.a11y
    }

    /// The text of a group's key value: as a cell shows it, with "(Blank)"
    /// for an empty key and Yes/No for a boolean.
    pub(super) fn value_text<'a>(
        &'a self,
        value: CellValue<'a>,
        buffer: &'a mut String,
    ) -> &'a str {
        match value {
            CellValue::Empty => &self.blank,
            CellValue::Bool(true) => &self.yes,
            CellValue::Bool(false) => &self.no,
            value => match display_text(value, buffer) {
                "" => &self.blank,
                text => text,
            },
        }
    }

    /// The name of an aggregate: the catalog's for a built-in one,
    /// `AggregateFn::name` for a custom one.
    pub(super) fn aggregate_name<'a, R: GridRow>(&'a self, aggregate: &'a Aggregate<R>) -> &'a str {
        match aggregate {
            Aggregate::Sum => &self.sum,
            Aggregate::Avg => &self.avg,
            Aggregate::Min => &self.min,
            Aggregate::Max => &self.max,
            Aggregate::Count => &self.count,
            Aggregate::Custom(function) => function.name(),
        }
    }

    /// The tooltip of an aggregate cell: "Sum: 1234.00"; the value alone
    /// for an aggregate without a name.
    pub(super) fn aggregate_tooltip(&self, name: &str, value: &str) -> SharedString {
        if name.is_empty() {
            return SharedString::from(value.to_owned());
        }
        let mut out = String::new();
        self.tooltip.write(&mut out, |part, out| match part {
            Part::Name => out.push_str(name),
            Part::Value => out.push_str(value),
            _ => {}
        });
        out.into()
    }
}

/// What [`LiveGridDelegate::group_caption`] knows about a visible caption.
///
/// [`LiveGridDelegate::group_caption`]: super::LiveGridDelegate::group_caption
pub struct GroupCaptionContext<'a, R: GridRow> {
    group: GroupInfo<'a>,
    title: &'a str,
    value_text: &'a str,
    texts: &'a CaptionTexts,
    row: PhantomData<fn() -> R>,
}

impl<'a, R: GridRow> GroupCaptionContext<'a, R> {
    /// The group, in the projection on screen.
    pub fn group(&self) -> GroupInfo<'a> {
        self.group
    }

    /// The title of the group's level: its column's title, or the
    /// `GroupKey` title.
    pub fn title(&self) -> &'a str {
        self.title
    }

    /// The group's key value.
    pub fn value(&self) -> &'a OwnedValue {
        self.group.value()
    }

    /// The text of the key value as the default caption writes it: as a
    /// cell shows it, "(Blank)" for an empty key, Yes/No for a boolean.
    pub fn value_text(&self) -> &'a str {
        self.value_text
    }

    /// The group's live rows, its subgroups' included.
    pub fn row_count(&self) -> usize {
        self.group.row_count()
    }

    /// The group's aggregate of `column`, if the view aggregates it.
    pub fn aggregate(&self, column: R::Column) -> Option<&'a OwnedValue> {
        self.group.aggregates().get(column.ix())
    }

    /// Writes the default caption, the catalog's `live_grid.group.caption`:
    /// "Instrument: AAPL (120)".
    pub fn default_caption(&self, out: &mut String) {
        let (title, value, count) = (self.title, self.value_text, self.row_count());
        self.texts.caption.write(out, |part, out| match part {
            Part::Title => out.push_str(title),
            Part::Value => out.push_str(value),
            Part::Count => {
                let _ = write!(out, "{count}");
            }
            _ => {}
        });
    }
}

/// A caption line to plan.
pub(super) struct PlannedCaption<'a> {
    pub(super) group: GroupInfo<'a>,
    pub(super) depth: u16,
    /// The top edge in window coordinates.
    pub(super) y: Pixels,
    pub(super) display_ix: usize,
    /// The title of the group's level.
    pub(super) title: &'a str,
    /// By the view's expansion: the chevron turns in the frame after the
    /// press, before the lines follow.
    pub(super) expanded: bool,
    pub(super) focused: bool,
    /// The pointer's x, if it is over this line and a tooltip may show.
    pub(super) hover_x: Option<Pixels>,
    /// The change stamps of the group's aggregates by `ColumnIx`, while any
    /// is hot (`Projection::group_stamps`).
    pub(super) stamps: Option<&'a [Stamp]>,
    /// The caption's own stamp: a new group, a new row count
    /// (`Projection::caption_stamp`).
    pub(super) stamp: Option<Stamp>,
}

impl<D: LiveGridDelegate> RowPlanner<'_, D> {
    /// Plans a caption line: the background across the zones and the row
    /// line; in the caption lane the caption's change, the chevron, the
    /// hook's text and the focus outline; in the zones the aggregate cells
    /// right of the text with their changes. While the accessibility tree is
    /// built, the caption row with its level and state, its text cell and
    /// its aggregate cells.
    pub(super) fn plan_caption(
        &mut self,
        caption: &PlannedCaption<'_>,
        regions: &mut [Region; 3],
        lane: &mut Region,
        window: &mut Window,
    ) {
        let (y, row_height) = (caption.y, self.row_height);
        let group = caption.group;
        let key_hash = group_hash(group.path());
        let aggregates = group.aggregates();
        let palette = self.captions.palette;
        let body_x = self.captions.body_x;
        let row_right = self.captions.row_right;
        for (slot, region) in regions.iter_mut().enumerate() {
            let row_x = region.mask().origin.x;
            let row_width = self.row_extents[slot];
            region.quads.push((
                Bounds::new(point(row_x, y), size(row_width, row_height)),
                palette.background,
            ));
            region.lines.push((
                Bounds::new(
                    point(row_x, y + row_height - px(1.)),
                    size(row_width, px(1.)),
                ),
                self.palette.row_border,
            ));
        }

        // The chevron lane, then the text; the text stops at the first
        // aggregate cell far enough right of it.
        let lane_x = body_x + CELL_PADDING + INDENT * f32::from(caption.depth);
        let text_x = lane_x + LEAD_ICON_LANE;
        let threshold = text_x + CAPTION_MIN_WIDTH;
        let visible_x = |slot: usize, x: Pixels| x.max(regions[slot].mask().origin.x);
        let mut caption_right = row_right;
        let mut end = None;
        for (slot, range) in self.zone_ranges.iter().enumerate() {
            for planned in &self.planned[range.clone()] {
                let x = visible_x(slot, planned.x);
                if aggregates.get(planned.column.ix()).is_some()
                    && x >= threshold
                    && x < caption_right
                {
                    caption_right = x;
                    end = Some(planned.view);
                }
            }
        }
        // A new group or a new row count tints the lane from the caption's
        // indent to its end, in the change paint.
        if let Some(change) = caption
            .stamp
            .and_then(|stamp| stamp_change(stamp, None, &self.policy, self.tick))
        {
            self.any_hot = true;
            let mut style = CellStyle::default();
            self.change_paint
                .paint(&change, &self.change_palette, &mut style);
            let left = lane_x - CELL_PADDING;
            if let Some(background) = style.background
                && caption_right > left
            {
                lane.quads.push((
                    Bounds::new(point(left, y), size(caption_right - left, row_height)),
                    background,
                ));
            }
        }
        let chevron = self.captions.chevron_size;
        lane.icons.push(IconItem {
            bounds: Bounds::new(
                point(
                    lane_x + (LEAD_ICON_LANE - chevron) / 2.,
                    y + (row_height - chevron) / 2.,
                ),
                size(chevron, chevron),
            ),
            path: self.captions.chevrons.path(caption.expanded),
            color: palette.chevron,
        });
        self.icons += 1;

        let captions = &mut self.captions;
        captions.caption.clear();
        captions.tag.clear();
        let value_text = captions
            .texts
            .value_text(group.value().as_cell(), &mut captions.value);
        let context = GroupCaptionContext {
            group,
            title: caption.title,
            value_text,
            texts: captions.texts,
            row: PhantomData,
        };
        self.delegate
            .group_caption(&context, &mut captions.caption, self.app);
        let tag = self
            .delegate
            .group_caption_tag(&context, &mut captions.tag, self.app)
            .filter(|_| !captions.tag.is_empty());
        // The tag, a pill before the text, if it fits before the first
        // aggregate.
        let mut text_start = text_x;
        if let Some(tag) = tag {
            let width = self
                .text_cache
                .for_weight(tag.font_weight)
                .layout(&captions.tag, window, &mut self.glyphs)
                .width();
            let pill_width = (width + TAG_PADDING * 2.).max(tag.min_width);
            let pill_height = (row_height * TAG_HEIGHT).round();
            if text_x + pill_width <= caption_right {
                lane.pills.push((
                    Bounds::new(
                        point(text_x, y + (row_height - pill_height) / 2.),
                        size(pill_width, pill_height),
                    ),
                    tag.background,
                    TAG_RADIUS,
                ));
                place_cell_text(
                    CellText {
                        text: &captions.tag,
                        x: text_x + (pill_width - width) / 2. - CELL_PADDING,
                        // A pixel more: the text is never cut by rounding.
                        width: width + CELL_PADDING * 2. + px(1.),
                        y,
                        right: false,
                        lead: None,
                        trend: None,
                        weight: tag.font_weight,
                        color: tag.foreground,
                    },
                    &mut CellTextContext {
                        text_cache: &mut *self.text_cache,
                        glyphs: &mut self.glyphs,
                        arrows: &self.arrows,
                        row_height,
                    },
                    lane,
                    window,
                );
                text_start = text_x + pill_width + TAG_GAP;
            }
        }
        let lane_width = (caption_right - body_x).max(px(0.));
        let (truncated, _) = place_cell_text(
            CellText {
                text: &captions.caption,
                x: text_start - CELL_PADDING,
                width: (caption_right - text_start + CELL_PADDING).max(px(0.)),
                y,
                right: false,
                lead: None,
                trend: None,
                weight: None,
                color: palette.foreground,
            },
            &mut CellTextContext {
                text_cache: &mut *self.text_cache,
                glyphs: &mut self.glyphs,
                arrows: &self.arrows,
                row_height,
            },
            lane,
            window,
        );
        let lane_bounds = Bounds::new(point(body_x, y), size(lane_width, row_height));
        if let Some(a11y) = &mut self.a11y {
            a11y.push_outlined_row(
                PaintedRow {
                    pinned: None,
                    position: caption.display_ix,
                    key_hash,
                    y,
                    height: row_height,
                    selected: false,
                },
                RowOutline {
                    content: RowContent::Caption,
                    level: Some(caption.depth.saturating_add(1)),
                    expanded: Some(caption.expanded),
                },
            );
            let end = end.unwrap_or(ViewColumnIx::new(captions.visible_columns as u16));
            if tag.is_some() {
                // The tag reads first: "U1 4 under way".
                let label = format!("{} {}", captions.tag, captions.caption);
                a11y.push_caption(&label, end, lane_bounds);
            } else {
                a11y.push_caption(&captions.caption, end, lane_bounds);
            }
            if caption.focused && self.a11y_focus {
                a11y.set_caption_focus(key_hash, None);
            }
        }
        if let Some(hover_x) = caption.hover_x
            && hover_x >= body_x
            && hover_x < caption_right
            && truncated
        {
            self.hovered = Some(HoveredCell {
                key_hash,
                column: CAPTION_COLUMN,
                id: CAPTION_TOOLTIP_ID,
                bounds: lane_bounds.intersect(&lane.mask()),
                tooltip: Some(CellTooltip::text(SharedString::from(
                    captions.caption.clone(),
                ))),
            });
        }
        if caption.focused && self.focus_visible {
            let row = Bounds::new(
                point(body_x, y),
                size((row_right - body_x).max(px(0.)), row_height),
            );
            lane.outline
                .extend(focus_outline(row).map(|bounds| (bounds, self.palette.ring)));
        }

        // The aggregate cells, as cells of their columns: change paint (from
        // the projection's marks), then the delegate's style.
        for (slot, region) in regions.iter_mut().enumerate() {
            for column_slot in self.zone_ranges[slot].clone() {
                let planned = &self.planned[column_slot];
                let column = planned.column;
                let Some(owned) = aggregates.get(column.ix()) else {
                    continue;
                };
                let value = owned.as_cell();
                let text = display_text_in(value, planned.number_format, &mut self.buffer);
                let name = self
                    .captions
                    .aggregates
                    .iter()
                    .find(|(aggregated, _)| *aggregated == column)
                    .map_or("", |(_, aggregate)| {
                        self.captions.texts.aggregate_name(aggregate)
                    });
                if let Some(a11y) = &mut self.a11y {
                    a11y.push_aggregate_cell(column_slot, text, name);
                }
                if planned.x.max(region.mask().origin.x) < threshold {
                    continue;
                }
                // The change paint first, then the delegate's style, as a
                // data cell's.
                let change = caption
                    .stamps
                    .and_then(|stamps| stamps.get(column.ix().get()))
                    .and_then(|stamp| {
                        stamp_change(*stamp, Some(column.ix()), &self.policy, self.tick)
                    });
                let mut style = CellStyle::default();
                if let Some(change) = &change {
                    self.any_hot = true;
                    self.change_paint
                        .paint(change, &self.change_palette, &mut style);
                }
                self.delegate.aggregate_style(
                    &AggregateStyleContext {
                        line: AggregateLine::Caption(group),
                        column,
                        value: Some(value),
                        change,
                        focused: caption.focused,
                    },
                    &mut style,
                    self.app,
                );
                // The arrow's direction in the cell's description.
                if let (Some(a11y), Some(trend)) = (&mut self.a11y, style.trend) {
                    a11y.set_trend(trend);
                }
                let cell_bounds = Bounds::new(point(planned.x, y), size(planned.width, row_height));
                if let Some(background) = style.background {
                    region.quads.push((cell_bounds, background));
                }
                let right = match planned.align {
                    ColumnAlign::End => true,
                    ColumnAlign::Start => false,
                    ColumnAlign::Auto => value.is_number(),
                };
                let (_, icons) = place_cell_text(
                    CellText {
                        text,
                        x: planned.x,
                        width: planned.width,
                        y,
                        right,
                        lead: None,
                        trend: style.trend,
                        weight: style.font_weight,
                        color: style.foreground.unwrap_or(palette.foreground),
                    },
                    &mut CellTextContext {
                        text_cache: &mut *self.text_cache,
                        glyphs: &mut self.glyphs,
                        arrows: &self.arrows,
                        row_height,
                    },
                    region,
                    window,
                );
                self.icons += icons;
                self.cells += 1;
                if let Some(hover_x) = caption.hover_x
                    && (planned.x..planned.x + planned.width).contains(&hover_x)
                    && hover_x >= region.mask().origin.x
                {
                    self.hovered = Some(HoveredCell {
                        key_hash,
                        column: column.ix(),
                        id: column.id(),
                        bounds: cell_bounds.intersect(&region.mask()),
                        tooltip: Some(CellTooltip::text(
                            self.captions.texts.aggregate_tooltip(name, text),
                        )),
                    });
                }
            }
        }
        self.captions.painted += 1;
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// The painted caption under a window position, and the column of the
    /// aggregate cell painted under it, if any.
    pub(super) fn caption_hit(
        &self,
        position: Point<Pixels>,
    ) -> Option<(GroupPath, Option<ColumnIx>)> {
        let path = self.painted.get(self.line_at(position)?)?.group()?.clone();
        let threshold = self.caption_lane_x(path.depth()) + LEAD_ICON_LANE + CAPTION_MIN_WIDTH;
        let column = self.column_at_position(position).filter(|ix| {
            let has_aggregate = self
                .projection
                .groups()
                .find(&path)
                .is_some_and(|group| group.aggregates().get(*ix).is_some());
            has_aggregate && self.cell_visible_x(*ix).is_some_and(|x| x >= threshold)
        });
        Some((path, column))
    }

    /// The window x where a visible column's cell shows: its left edge, or
    /// its zone's for a cell partly under a frozen zone.
    fn cell_visible_x(&self, column: ColumnIx) -> Option<Pixels> {
        let geometry = self
            .columns
            .geometry(self.body_bounds.size.width, self.scroll_x);
        let (zone, i) = self.columns.zone_position(column)?;
        let frame = geometry.zone(zone);
        let x = frame
            .column_x(self.columns.zone_columns(zone), i)
            .max(frame.x);
        Some(self.body_bounds.origin.x + x)
    }
}

#[cfg(test)]
mod tests {
    use super::{Part, Template};

    #[test]
    fn test_caption_template() {
        let template = Template::parse("%{title}: %{value} (%{count})".to_owned());
        assert_eq!(
            template.parts,
            [
                Part::Title,
                Part::Text(8..10),
                Part::Value,
                Part::Text(18..20),
                Part::Count,
                Part::Text(28..29),
            ]
        );
        let mut out = String::new();
        template.write(&mut out, |part, out| {
            out.push_str(match part {
                Part::Title => "Instrument",
                Part::Value => "AAPL",
                Part::Count => "120",
                _ => "?",
            })
        });
        assert_eq!(out, "Instrument: AAPL (120)");
        // An unknown placeholder and an unclosed one stay as text.
        let template = Template::parse("%{other} %{value} %{".to_owned());
        let mut out = String::new();
        template.write(&mut out, |_, out| out.push_str("v"));
        assert_eq!(out, "%{other} v %{");
    }
}
