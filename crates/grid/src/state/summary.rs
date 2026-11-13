//! Totals and group footers on screen: where the totals row is shown, how a
//! summary line is planned, what a press on one targets, and what autofit
//! measures of them.
//!
//! A summary line shows aggregates of the projection on screen: the totals
//! row those of the whole view (`Projection::totals`), a group footer its
//! group's. Both read as the band's footer (`table_foot`, no stripes); each
//! aggregate sits under its column, in its zone, and scrolls sideways with
//! it. The totals row is the outermost line of its pinned band, in the
//! semibold weight by default, with the label "Total" in the first visible
//! column when that column has no aggregate. A footer follows its group's
//! lines in the data rows, in the grid's font, without a label: its group's
//! caption above names it. Neither takes the focus or the selection.

use gpui_kit::{
    App, Bounds, Context, FontWeight, Hsla, Pixels, Point, SharedString, Window, point, px, size,
};
use rust_i18n::t;

use super::{
    CellChange, CellStyle, ColumnAlign, ColumnOf, KeyOf, LiveGridDelegate, LiveGridState,
    ViewColumnIx,
    a11y::{PaintedRow, RowOutline},
    change::ChangePainter as _,
    columns::column_of,
    frame::{CellText, CellTextContext, RowPlanner, place_cell_text},
    menu::GridTarget,
    pinned::BodyLine,
    style::{AggregateLine, AggregateStyleContext},
    tooltip::{CellTooltip, HoveredCell},
};
use crate::{
    element::Region,
    engine::{AggregateRow, GroupPath},
    highlight::Stamp,
    model::{ColumnIx, GridColumns as _, NumberFormat, OwnedValue, display_text_in},
    store::PinnedEdge,
    text::{GlyphArena, TextCaches},
};

/// The weight of the totals row unless `aggregate_style` sets another.
/// Footers keep the grid's font: a subtotal reads quieter than the total.
pub(super) const TOTALS_WEIGHT: FontWeight = FontWeight::SEMIBOLD;

/// The label of the totals row, from this crate's catalog once per locale
/// change, never per frame.
#[derive(Debug, Default)]
pub(crate) struct SummaryTexts {
    /// The locale the texts are of; empty before the first read.
    locale: String,
    total: SharedString,
}

impl SummaryTexts {
    /// Reads the texts again if the locale changed since the last read.
    pub(super) fn refresh(&mut self) {
        let locale = rust_i18n::locale();
        if *locale == *self.locale {
            return;
        }
        self.total = t!("live_grid.totals.label", locale = &*locale)
            .into_owned()
            .into();
        self.locale = locale.to_string();
    }

    /// "Total": the label of the totals row and its accessible name.
    pub(super) fn total(&self) -> &str {
        &self.total
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Shows the totals row at `edge`: the aggregates of every row the view
    /// passes (rows of collapsed groups included), as the outermost row of
    /// that pinned band. Without aggregates (`ViewSpec::with_aggregates`)
    /// the row shows only its label.
    pub fn with_totals(mut self, edge: PinnedEdge) -> Self {
        self.totals = Some(edge);
        self
    }

    /// Shows the totals row at `edge`, or hides it (`None`). Takes effect
    /// with the next frame; no event, no rebuild.
    pub fn set_totals(&mut self, edge: Option<PinnedEdge>, cx: &mut Context<Self>) {
        if self.totals == edge {
            return;
        }
        self.totals = edge;
        // The data rows' area changed.
        self.clamp_scroll();
        cx.notify();
    }

    /// Where the totals row is shown, if it is.
    pub fn totals(&self) -> Option<PinnedEdge> {
        self.totals
    }

    /// The target of a press at a window position on a summary line: a
    /// cell of the totals row or of a painted group footer, in the column
    /// under the press (left of the columns the first visible one, right of
    /// them the last).
    pub(super) fn summary_target(
        &self,
        position: Point<Pixels>,
    ) -> Option<GridTarget<KeyOf<D>, ColumnOf<D>>> {
        let column = || {
            self.column_at_position(position)
                .or_else(|| {
                    if position.x < self.body_bounds.origin.x {
                        self.columns.visible().next()
                    } else {
                        self.columns.visible().last()
                    }
                })
                .map(column_of)
        };
        match self.body_line_at(position)? {
            BodyLine::Totals(edge) => Some(GridTarget::Totals {
                edge,
                column: column()?,
            }),
            BodyLine::Data(line) => Some(GridTarget::GroupFooter {
                group: self.painted.get(line)?.footer()?.clone(),
                column: column()?,
            }),
            BodyLine::Pinned(..) => None,
        }
    }

    /// Returns true if the shown projection has the footer of the group
    /// at `path` on a line: the group is there, expanded, under expanded
    /// ancestors, and `GroupFooter` gives it a footer.
    pub(super) fn is_footer_shown(&self, path: &GroupPath) -> bool {
        let projection = &self.projection;
        projection.groups().find(path).is_some_and(|group| {
            group.has_footer() && group.is_expanded() && projection.group_line(group.id()).is_some()
        })
    }
}

/// A summary line to plan: the totals row or a group footer.
pub(super) struct PlannedSummary<'a> {
    pub(super) line: AggregateLine<'a>,
    pub(super) aggregates: AggregateRow<'a>,
    /// The change stamps of the aggregates, by `ColumnIx`.
    pub(super) stamps: Option<&'a [Stamp]>,
    /// The top edge in window coordinates.
    pub(super) y: Pixels,
    /// The line's background across the zones; the totals row has its
    /// band's.
    pub(super) background: Option<Hsla>,
    /// The weight before the delegate's style.
    pub(super) weight: Option<FontWeight>,
    /// The label in the first visible column, if that column has no
    /// aggregate; also the row's accessible name.
    pub(super) label: Option<&'a str>,
    pub(super) hovered_column: Option<ColumnIx>,
    /// The line's accessibility row.
    pub(super) row: PaintedRow,
    pub(super) outline: RowOutline,
}

impl<D: LiveGridDelegate> RowPlanner<'_, D> {
    /// Plans a summary line across the three zone regions of its band:
    /// the background (a footer's), then every cell, as a data cell is:
    /// change paint from its stamp, the weight, `aggregate_style`, the
    /// aggregate's text or the label, a tooltip for the hovered cell; then
    /// the row line. While the accessibility tree is built, the row with
    /// its name, the label cell and the aggregate cells; empty cells are
    /// left out, as on a caption.
    pub(super) fn plan_summary(
        &mut self,
        summary: &PlannedSummary<'_>,
        regions: &mut [Region; 3],
        window: &mut Window,
    ) {
        let (y, row_height, palette) = (summary.y, self.row_height, self.palette);
        let key_hash = summary.row.key_hash;
        if let Some(a11y) = &mut self.a11y {
            a11y.push_outlined_row(summary.row, summary.outline);
            if let Some(label) = summary.label {
                a11y.set_row_label(label);
            }
        }
        for (slot, region) in regions.iter_mut().enumerate() {
            let row_x = region.mask().origin.x;
            let row_width = self.row_extents[slot];
            if let Some(background) = summary.background {
                region.quads.push((
                    Bounds::new(point(row_x, y), size(row_width, row_height)),
                    background,
                ));
            }
            for column_slot in self.zone_ranges[slot].clone() {
                let planned = &self.planned[column_slot];
                let column = planned.column;
                let (x, width) = (planned.x, planned.width);
                let owned = summary.aggregates.get(column.ix());
                let value = owned.map(OwnedValue::as_cell);
                let label = summary
                    .label
                    .filter(|_| owned.is_none() && planned.view == ViewColumnIx::new(0));
                let change = owned
                    .and(summary.stamps)
                    .and_then(|stamps| stamps.get(column.ix().get()))
                    .and_then(|stamp| {
                        self.policy
                            .intensity(*stamp, self.tick)
                            .map(|intensity| CellChange {
                                intensity,
                                change: stamp.change(),
                                direction: stamp.direction(),
                                column_ix: Some(column.ix()),
                            })
                    });
                self.any_hot |= change.is_some();

                let mut style = CellStyle::default();
                if let Some(weight) = summary.weight {
                    style.set_font_weight(weight);
                }
                if let Some(change) = &change {
                    self.change_paint
                        .paint(change, &self.change_palette, &mut style);
                }
                self.delegate.aggregate_style(
                    &AggregateStyleContext {
                        line: summary.line,
                        column,
                        value,
                        change,
                        focused: false,
                    },
                    &mut style,
                    self.app,
                );
                let cell_bounds = Bounds::new(point(x, y), size(width, row_height));
                if let Some(background) = style.background {
                    region.quads.push((cell_bounds, background));
                }
                let right = match planned.align {
                    ColumnAlign::End => true,
                    ColumnAlign::Start => false,
                    ColumnAlign::Auto => value.is_some_and(|value| value.is_number()),
                };
                let text = match (value, label) {
                    (Some(value), _) => {
                        display_text_in(value, planned.number_format, &mut self.buffer)
                    }
                    (None, Some(label)) => label,
                    (None, None) => "",
                };
                let (truncated, icons) = place_cell_text(
                    CellText {
                        text,
                        x,
                        width,
                        y,
                        right,
                        lead: None,
                        trend: style.trend,
                        weight: style.font_weight,
                        color: style.foreground.unwrap_or(palette.pinned_foreground),
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
                // The aggregate's name: the catalog's, or a custom one's.
                let name = owned.map(|_| {
                    self.captions
                        .aggregates
                        .iter()
                        .find(|(aggregated, _)| *aggregated == column)
                        .map_or("", |(_, aggregate)| {
                            self.captions.texts.aggregate_name(aggregate)
                        })
                });
                if let Some(a11y) = &mut self.a11y {
                    match name {
                        Some(name) => {
                            a11y.push_aggregate_cell(column_slot, text, name);
                            if let Some(trend) = style.trend {
                                a11y.set_trend(trend);
                            }
                        }
                        None if label.is_some() => a11y.push_cell(column_slot, text, false),
                        None => {}
                    }
                }
                if summary.hovered_column == Some(column.ix()) {
                    // An aggregate always tells its name and full value;
                    // the label only when truncated. The delegate's
                    // `tooltip` needs a row.
                    let tooltip = match name {
                        Some(name) => Some(CellTooltip::text(
                            self.captions.texts.aggregate_tooltip(name, text),
                        )),
                        None if truncated => {
                            Some(CellTooltip::text(SharedString::from(text.to_owned())))
                        }
                        None => None,
                    };
                    self.hovered = Some(HoveredCell {
                        key_hash,
                        column: column.ix(),
                        id: column.id(),
                        bounds: cell_bounds.intersect(&region.mask()),
                        tooltip,
                    });
                }
            }
            region.lines.push((
                Bounds::new(
                    point(row_x, y + row_height - px(1.)),
                    size(row_width, px(1.)),
                ),
                palette.row_border,
            ));
        }
    }
}

/// A summary line of the frame autofit measures: a group caption or footer
/// on screen, or the totals row.
pub(super) struct SummaryLine<'a> {
    pub(super) line: AggregateLine<'a>,
    pub(super) aggregates: AggregateRow<'a>,
    /// The weight the frame paints it in before the delegate's style.
    pub(super) weight: Option<FontWeight>,
    /// The totals row's label and the column it shows in.
    pub(super) label: Option<(&'a str, ColumnIx)>,
}

/// What measuring summary texts for autofit needs.
pub(super) struct SummaryMeasure<'a, D: LiveGridDelegate> {
    pub(super) delegate: &'a D,
    pub(super) app: &'a App,
    pub(super) text_cache: &'a mut TextCaches,
    pub(super) buffer: &'a mut String,
    pub(super) arena: &'a mut GlyphArena,
    /// How the measured column shows its numbers.
    pub(super) format: NumberFormat,
}

impl<D: LiveGridDelegate> SummaryMeasure<'_, D> {
    /// The widest text of `column` on `lines`: its aggregates and the
    /// totals label, each in the weight the frame paints it in (the line's,
    /// then `aggregate_style`'s). Zero without them. Once per autofit, never
    /// per frame.
    pub(super) fn width(
        &mut self,
        lines: &[SummaryLine<'_>],
        column: ColumnOf<D>,
        window: &Window,
    ) -> Pixels {
        let mut widest = Pixels::ZERO;
        for line in lines {
            let owned = line.aggregates.get(column.ix());
            let label = line
                .label
                .filter(|(_, label_column)| owned.is_none() && *label_column == column.ix())
                .map(|(label, _)| label);
            if owned.is_none() && label.is_none() {
                continue;
            }
            let value = owned.map(OwnedValue::as_cell);
            let mut style = CellStyle::default();
            if let Some(weight) = line.weight {
                style.set_font_weight(weight);
            }
            self.delegate.aggregate_style(
                &AggregateStyleContext {
                    line: line.line,
                    column,
                    value,
                    change: None,
                    focused: false,
                },
                &mut style,
                self.app,
            );
            let text = match (value, label) {
                (Some(value), _) => display_text_in(value, self.format, self.buffer),
                (None, Some(label)) => label,
                (None, None) => continue,
            };
            if text.is_empty() {
                continue;
            }
            self.arena.clear();
            let width = self
                .text_cache
                .for_weight(style.font_weight)
                .layout(text, window, self.arena)
                .width();
            widest = widest.max(width);
        }
        widest
    }
}
