//! Frame planning: what the body paints this frame, planned in prepaint,
//! and the highlight fade timer.

use std::{
    ops::Range,
    rc::Rc,
    sync::{Arc, LazyLock},
    time::Duration,
};

use gpui_kit::component::{ActiveTheme as _, IconName, IconNamed as _};
use gpui_kit::{
    App, Bounds, Context, FontFeatures, FontWeight, Hsla, Pixels, RenderImage, SharedString,
    WeakEntity, Window, point, px, size,
};

use rust_i18n::t;

use super::{
    CELL_PADDING, CellChange, CellStyle, CellStyleContext, ColumnAlign, ColumnOf, ColumnZone,
    KeyOf, LiveGridDelegate, LiveGridState, ViewColumnIx,
    a11y::{
        A11yInput, A11yPlan, PaintedColumn, PaintedRow, RowContent, RowOutline, data_row_id_name,
        plan_a11y,
    },
    change::{ChangePaint, ChangePainter as _, ChangePalette, RowChanges},
    columns::{CellContent, ColumnLayout, ZoneGeometry},
    disclosure::{Chevrons, INDENT, chevron_size},
    edit::{EditSlot, EditState},
    fade::FadingLine,
    footer_hash,
    groups::{CaptionPalette, CaptionTexts, PlannedCaption},
    images::{CellImage, CellImages, ImageFit},
    key_hash,
    pending::PendingEdits,
    pinned::{BodyLine, EDGES, band_slot, pinned_key_hash},
    selection::ShownRows,
    stats::{profile_elapsed, profile_start},
    style::{AggregateLine, Palette},
    summary::{PlannedSummary, TOTALS_WEIGHT},
    tooltip::{CellTooltip, CellTooltipContext, HoveredCell},
    totals_hash,
    widgets::WidgetSlot,
};
#[cfg(any(test, feature = "test-support"))]
use crate::element::{CellHost, HostLayer};
use crate::{
    element::{FramePlan, HostPlan, IconItem, ImageItem, Region},
    engine::{Aggregate, DisplayRow, Expansion, GroupPath, Projection},
    highlight::{ChangeDirection, HighlightPolicy},
    model::{
        CellValue, ColumnId, ColumnIx, ColumnSet, GridColumns, GridRow, NumberFormat,
        display_text_in,
    },
    policy::SelectableLine,
    store::{PinnedEdge, RowId, StoreSnapshot},
    text::{GlyphArena, TextCaches, TextItem},
};

/// The lane of an icon at the start of a cell: the last 20 px of the cell's
/// indent, where a disclosure chevron goes.
pub(super) const LEAD_ICON_LANE: Pixels = px(20.);
/// The lane of a change arrow, on the side opposite the text's alignment.
const TREND_LANE: Pixels = px(14.);
/// The change arrow in its lane.
const TREND_ICON: Pixels = px(12.);

/// Tabular figures (OpenType `tnum`) alone: the features of a grid that
/// inherits none, shared by every frame.
static TABULAR_FIGURES: LazyLock<FontFeatures> =
    LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));

/// The font features of the body's text: `inherited` with tabular figures
/// (`tnum`) added, so digits line up in columns. Features the application
/// sets on an ancestor of the grid, such as `zero` for a slashed zero, stay;
/// a `tnum` it sets itself, even `0`, is kept as it is.
fn with_tabular_figures(inherited: FontFeatures) -> FontFeatures {
    let features = inherited.tag_value_list();
    if features.is_empty() {
        return TABULAR_FIGURES.clone();
    }
    if features.iter().any(|(tag, _)| tag == "tnum") {
        return inherited;
    }
    let mut merged = Vec::with_capacity(features.len() + 1);
    merged.extend_from_slice(features);
    merged.push(("tnum".into(), 1));
    FontFeatures(Arc::new(merged))
}

/// What a frame painted on one line of the data rows, a row fading out
/// included: what hit tests, the drag-selection, tooltips and
/// accessibility read, rather than the projection, which may still hold
/// removed rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PaintedLine {
    /// A data row.
    Row(RowId),
    /// A group caption. Cloning the path is O(1).
    Caption(GroupPath),
    /// A group footer: no focus, no selection, no scroll anchor.
    Footer(GroupPath),
    /// A removed row while it fades out, by its id before removal: input
    /// on it lands on empty space.
    Ghost(RowId),
}

impl PaintedLine {
    /// The data row painted on the line, if it is one; not a fading row's.
    pub(super) fn row(&self) -> Option<RowId> {
        match self {
            Self::Row(id) => Some(*id),
            Self::Caption(_) | Self::Footer(_) | Self::Ghost(_) => None,
        }
    }

    /// The group of a caption painted on the line.
    pub(super) fn group(&self) -> Option<&GroupPath> {
        match self {
            Self::Caption(path) => Some(path),
            Self::Row(_) | Self::Footer(_) | Self::Ghost(_) => None,
        }
    }

    /// The group of a footer painted on the line.
    pub(super) fn footer(&self) -> Option<&GroupPath> {
        match self {
            Self::Footer(path) => Some(path),
            Self::Row(_) | Self::Caption(_) | Self::Ghost(_) => None,
        }
    }
}

/// The start of a cell: its text begins `indent` further in, with that
/// much less room, and an icon may sit in the indent's last
/// [`LEAD_ICON_LANE`] (a group's chevron, a tree row's).
pub(super) struct CellLead {
    pub(super) indent: Pixels,
    pub(super) icon: Option<LeadIcon>,
}

/// The icon of a [`CellLead`], centred in its lane.
pub(super) struct LeadIcon {
    pub(super) path: SharedString,
    pub(super) color: Hsla,
    pub(super) size: Pixels,
}

/// Gives the start of each cell of a shown line; `None` for an ordinary
/// cell.
pub(super) type LeadProvider<'a> = &'a dyn Fn(&DisplayRow, ColumnIx) -> Option<CellLead>;

impl<D: LiveGridDelegate> LiveGridState<D> {
    fn schedule_fade(&mut self, deadline_ms: u32, now_ms: u32, cx: &mut Context<Self>) {
        if let Some((existing, _)) = &self.fade
            && *existing <= deadline_ms
        {
            return;
        }
        let wait = Duration::from_millis(u64::from(deadline_ms.saturating_sub(now_ms)));
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |state, cx| {
                state.fade = None;
                state.stats.fade_wakeups += 1;
                cx.notify();
            });
        });
        self.fade = Some((deadline_ms, task));
    }

    /// Plans the body for this frame: every quad and text line, with style
    /// hooks run here and nowhere else.
    ///
    /// The body splits top to bottom into the top pinned band, the data
    /// rows and the bottom pinned band (`state/pinned.rs`), and left to
    /// right into the three zones: one region per zone and band, each with
    /// its own mask and layer. Cells are walked zone by zone, so no cell
    /// branches on its zone, and pinned rows take the same path as data rows
    /// ([`RowPlanner`]).
    ///
    /// The cell under the pointer is found in the walk: the delegate's
    /// `tooltip` runs for it once, with the text as painted and whether it
    /// was truncated, and the frame plans the div that carries the tooltip.
    ///
    /// While the accessibility tree is built, the same walk gathers the
    /// painted rows and cells with their full texts, the selection and the
    /// focused cell, whose `tooltip` text describes it: at most two
    /// `tooltip` calls a frame, one if the focused cell is hovered. The plan
    /// of the tree goes to the body with the frame.
    pub(crate) fn plan_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FramePlan {
        let started = profile_start();
        let previous_size = self.body_bounds.size;
        let previous_scroll_x = self.scroll_x;
        self.body_bounds = bounds;
        self.clamp_scroll();

        let theme = cx.theme();
        let palette = Palette {
            foreground: theme.foreground,
            row_border: theme.table_row_border,
            stripe: theme.table_even,
            selected: theme.table_active,
            ring: theme.ring,
            divider: theme.border,
            pinned: theme.table_foot,
            pinned_foreground: theme.table_foot_foreground,
            muted: theme.muted_foreground,
            danger: theme.danger,
            link: theme.link,
        };
        let drop_gap_color = theme.drag_border;
        // The drop indicator.
        let (drop_fill, drop_border) = (theme.drop_target, theme.drag_border);
        let change_palette = ChangePalette::new(theme);
        // A chevron's colour.
        let lead_color = theme.muted_foreground;
        // A group caption is the header of its rows: no new tokens.
        let caption_palette = CaptionPalette {
            background: theme.table_head,
            foreground: theme.table_head_foreground,
            chevron: lead_color,
        };
        // Captions, aggregate names, expansion states and the totals label,
        // from the catalog once per locale change.
        self.caption_texts.refresh();
        self.summary_texts.refresh();

        let text_style = window.text_style();
        let mut font = text_style.font();
        // Tabular figures keep digits in columns (Design Guides: data-heavy
        // screens), on top of the features the grid inherits.
        font.features = with_tabular_figures(font.features);
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        self.text_cache.begin_frame(&font, font_size);
        let ellipsis = self.text_cache.regular().ellipsis(window);

        let policy = self.store.options().highlight();
        let now_ms = self
            .store
            .now()
            .saturating_duration_since(self.snapshot.epoch())
            .as_millis() as u32;
        let tick = policy.tick(now_ms);

        let row_height = self.row_height;
        let bands = self.body_bands();
        let rows_area = bands.rows();
        let needed_lines =
            ((rows_area.size.height + self.scroll_offset) / row_height).ceil() as usize;

        // A requested autofit measures the rows of this frame before its
        // cells are laid out, so the frame paints the new widths. The header
        // of this frame is built already: the event and the repaint come
        // right after the frame, outside prepaint.
        if let Some((delta, origin)) = self.apply_pending_autofit(needed_lines, window, cx) {
            let this = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = this.update(cx, |state, cx| {
                    state.emit_layout_change(delta, origin, cx);
                    cx.notify();
                });
            });
        }
        // The header was rendered with the previous bounds and scroll; when
        // they changed, render once more so header and body agree.
        if previous_size != bounds.size || previous_scroll_x != self.scroll_x {
            cx.on_next_frame(window, |_, _, cx| cx.notify());
        }

        let geometry = self.columns.geometry(bounds.size.width, self.scroll_x);
        let has_right_zone = geometry.zone(ColumnZone::FrozenRight).width > px(0.);
        // One region per zone over an area of the body.
        let zone_regions = |area: Bounds<Pixels>| {
            ColumnZone::ALL.map(|zone| {
                let frame = geometry.zone(zone);
                Region::new(Bounds::new(
                    point(bounds.origin.x + frame.x, area.origin.y),
                    size(frame.width, area.size.height),
                ))
            })
        };
        let mut regions = zone_regions(rows_area);
        let mut band_regions = EDGES.map(|edge| zone_regions(bands.band(edge)));
        // How far row backgrounds and row lines reach in each zone: across
        // the zone, except that without a right zone the scrolling rows end
        // with the last column, as before.
        let row_extents = ColumnZone::ALL.map(|zone| {
            let frame = geometry.zone(zone);
            if zone == ColumnZone::Scrolling && !has_right_zone {
                (self.columns.zone_columns(zone).content_width() - frame.scroll)
                    .max(px(0.))
                    .min(frame.width)
            } else {
                frame.width
            }
        });
        // The columns painted this frame, zone by zone.
        let mut planned = Vec::with_capacity(64);
        let mut zone_ranges = [0..0, 0..0, 0..0];
        for zone in ColumnZone::ALL {
            let frame = geometry.zone(zone);
            let columns = self.columns.zone_columns(zone);
            let start = planned.len();
            for i in frame.columns.clone() {
                let ix = columns.columns[i];
                let column = self.columns.column(ix);
                planned.push(PlannedColumn {
                    column: column.column,
                    view: self
                        .columns
                        .view_column_ix(ix)
                        .expect("a painted column is visible"),
                    align: column.align,
                    content: column.content,
                    title: column.title.clone(),
                    image_fit: column.image_fit,
                    image_radius: column.image_corner_radius,
                    number_format: column.number_format,
                    x: bounds.origin.x + frame.column_x(columns, i),
                    width: columns.width(i),
                });
            }
            zone_ranges[zone.slot()] = start..planned.len();
        }

        let drop_gap_x = self.drop_gap_x(cx);
        // The cell under the pointer: the line and column now, so a row
        // moved under a resting pointer is the row hovered.
        let hover = self.hover_target();
        let snapshot = self.snapshot.clone();
        let projection = self.projection.clone();
        // Selection: one hash lookup per painted row, whatever its size; a
        // drag paints its range by comparing display positions.
        let visible_columns = self.columns.visible_set();
        let painter = self
            .selection
            .painter(ShownRows::new(&projection, &snapshot), &self.columns);
        // A drag paints its range by position: lines the selection policy
        // keeps out of the selection (a tree row with children) are
        // left unpainted, as the release leaves them unselected.
        let dragging = self.selection.is_dragging();
        let selection_policy = &*self.selection_policy;
        let focus = self
            .selection
            .focus()
            .and_then(|cell| Some((snapshot.row_id(cell.row()?)?, cell.column().ix())));
        let focus_group = self
            .selection
            .focus()
            .and_then(|cell| cell.group())
            .cloned();
        // A tree's rows have their indent and chevron in this column.
        let tree_column = projection.is_tree().then(|| self.tree_column()).flatten();
        let structured = projection.is_grouped() || projection.is_tree();
        // The outline (and the root's ring) only after keyboard input, as
        // CSS `:focus-visible`; GPUI repaints when the input changes. The
        // rows have the focus on the root or in an open editor.
        let grid_focused = self.focus_handle.is_focused(window);
        let focus_visible = self.is_body_focused(window, cx) && window.last_input_was_keyboard();
        // What the walk gathers for the accessibility tree, only while the
        // tree is built. Columns in `planned` order: a column's place there
        // is its slot.
        let a11y = self.is_a11y_planned(window).then(|| {
            let mut input = A11yInput::new(self.a11y_numbering(), window.scale_factor());
            input.reserve(self.stats.a11y_rows, self.stats.a11y_cells + 1);
            input.set_selection_unit(Some(self.selection.mode().selection_unit()));
            // A flat row's cell tells its trend too, so the words are there
            // in every view.
            input.set_texts(self.caption_texts.a11y());
            if let Some(column) = tree_column {
                input.set_tree_column(column);
            }
            input.set_band_clip(None, rows_area);
            for edge in EDGES {
                input.set_band_clip(Some(edge), bands.band(edge));
            }
            for zone in ColumnZone::ALL {
                let frame = geometry.zone(zone);
                input.set_zone(zone, bounds.origin.x + frame.x, frame.width);
                for planned in &planned[zone_ranges[zone.slot()].clone()] {
                    let ix = planned.column.ix();
                    input.push_column(PaintedColumn {
                        column: ix,
                        view: planned.view,
                        id: planned.column.id(),
                        title: &self.columns.column(ix).title,
                        zone,
                        x: planned.x,
                        width: planned.width,
                    });
                }
            }
            input
        });
        // The focused cell is the active descendant only while the grid
        // holds the focus (not while its menu does).
        let a11y_focus = a11y.is_some() && grid_focused;
        // The starts of cells: a tree row's indent and chevron in the tree
        // column; tests set their own.
        let expansion = self.view.expansion().clone();
        let chevron_icon = chevron_size(row_height);
        let tree_leads = tree_column.map(|column| TreeLeads {
            column,
            projection: &projection,
            snapshot: &snapshot,
            expansion: &expansion,
            chevrons: Chevrons::new(),
            color: lead_color,
            size: chevron_icon,
        });
        let tree_lead = tree_leads
            .as_ref()
            .map(|leads| move |line: &DisplayRow, column: ColumnIx| leads.lead(line, column));
        #[cfg(any(test, feature = "test-support"))]
        let test_lead = self
            .test_cell_lead
            .clone()
            .map(|(lead_column, indent, icon)| {
                move |_: &DisplayRow, column: ColumnIx| {
                    (column == lead_column).then(|| CellLead {
                        indent,
                        icon: icon.clone().map(|path| LeadIcon {
                            path,
                            color: lead_color,
                            size: px(12.),
                        }),
                    })
                }
            });
        #[cfg(any(test, feature = "test-support"))]
        let test_lead = test_lead.as_ref().map(|lead| lead as LeadProvider<'_>);
        #[cfg(not(any(test, feature = "test-support")))]
        let test_lead: Option<LeadProvider<'_>> = None;
        let lead = test_lead.or(tree_lead.as_ref().map(|lead| lead as LeadProvider<'_>));
        // The title of each grouping level, for captions.
        let titles: Vec<(ColumnId, &str)> = self
            .view
            .group_by()
            .iter()
            .map(|level| {
                let title = match (level.as_column(), level.as_key()) {
                    (Some(column), _) => self.columns.column(column.ix()).title.as_ref(),
                    (None, Some(key)) => key.title(),
                    (None, None) => "",
                };
                (level.id(), title)
            })
            .collect();
        let levels = self.view.group_by().len();
        // Where the painted rows end on the right: a caption's background
        // and text stop there.
        let row_right = bounds.origin.x
            + ColumnZone::ALL
                .iter()
                .map(|zone| {
                    let frame = geometry.zone(*zone);
                    frame.x + row_extents[zone.slot()]
                })
                .fold(px(0.), Pixels::max);
        let hover_point = self.hover_point();
        let mut fills: [FillRuns; 3] = Default::default();
        // The caption lane: over the zones, not scrolled sideways.
        let mut lane = Region::new(rows_area);
        // The screen lines of captions: grid lines leave them out.
        let mut caption_lines: Vec<usize> = Vec::new();
        let painted = &mut self.painted;
        painted.clear();
        // What a row's element of cell hosts is clipped to, by band.
        let band_clips = [
            bands.band(PinnedEdge::Top),
            rows_area,
            bands.band(PinnedEdge::Bottom),
        ];
        let mut hosts = HostPlan::new(ColumnZone::ALL.map(|zone| {
            let frame = geometry.zone(zone);
            (bounds.origin.x + frame.x, frame.width)
        }));
        hosts.reserve(self.stats.cell_hosts);
        // The focused control registers its handler of Space anew; the
        // image source keeps this frame's images.
        self.widgets.begin_frame();
        self.images.begin_frame();
        let grid = cx.weak_entity();
        // The links of this frame replace the last frame's, in the same buffer.
        let mut link_rects = std::mem::take(&mut self.links.rects);
        link_rects.clear();
        let mut planner = RowPlanner {
            delegate: &self.delegate,
            app: cx,
            text_cache: &mut self.text_cache,
            planned: &planned,
            zone_ranges: &zone_ranges,
            row_extents,
            palette,
            change_paint: &self.change_paint,
            change_palette,
            lead,
            arrows: Arrows::new(),
            policy,
            tick,
            row_height,
            focus_visible,
            buffer: String::new(),
            glyphs: Vec::with_capacity(4_096),
            any_hot: false,
            cells: 0,
            icons: 0,
            hovered: None,
            a11y,
            a11y_focus,
            band_clips,
            hosts,
            edit: &self.edit,
            edited_row_painted: false,
            pending: &self.pending_edits,
            #[cfg(any(test, feature = "test-support"))]
            test_host: self.test_cell_host.clone(),
            grid,
            widgets: &self.widgets,
            images: &self.images,
            images_painted: 0,
            host_text: String::new(),
            links: link_rects,
            captions: CaptionPlanner {
                palette: caption_palette,
                texts: &self.caption_texts,
                aggregates: self.view.aggregates(),
                chevrons: Chevrons::new(),
                chevron_size: chevron_icon,
                body_x: bounds.origin.x,
                row_right,
                visible_columns: visible_columns.len(),
                caption: String::new(),
                tag: String::new(),
                value: String::new(),
                painted: 0,
            },
        };

        // Walk the shown lines from the first visible one and fill the
        // viewport with live rows. Rows removed after the projection was
        // built are skipped, so they leave the screen on the next frame
        // without waiting for the background compaction.
        let mut tombstones = 0;
        let mut line = 0;
        let mut next_display_ix = self.scroll_row;
        while line < needed_lines && next_display_ix < projection.len() {
            let display_ix = next_display_ix;
            next_display_ix += 1;
            let y = rows_area.origin.y + row_height * line as f32 - self.scroll_offset;
            let (id, display_row) = match projection.line(display_ix) {
                Some(display_row @ DisplayRow::Data { row, .. }) => (row, display_row),
                Some(DisplayRow::GroupCaption { group, depth }) => {
                    let Some(group) = projection.groups().get(group) else {
                        continue;
                    };
                    // A caption ends the selection fill's runs above it.
                    for (runs, region) in fills.iter_mut().zip(&mut regions) {
                        runs.end_row(y, &mut region.fills, palette.selected);
                    }
                    let path = group.path();
                    let title = titles
                        .iter()
                        .find(|(id, _)| Some(id) == path.level_id(depth))
                        .map_or_else(
                            || path.level_id(depth).map_or("", ColumnId::as_str),
                            |(_, title)| title,
                        );
                    planner.plan_caption(
                        &PlannedCaption {
                            group,
                            depth,
                            y,
                            display_ix,
                            title,
                            expanded: expansion
                                .is_group_expanded(path, levels.max(usize::from(depth) + 1)),
                            focused: focus_group.as_ref() == Some(path),
                            hover_x: hover_point.and_then(|(at, position)| {
                                (at == BodyLine::Data(line)).then_some(position.x)
                            }),
                            // What changed.
                            stamps: projection.group_stamps(group.id()),
                            stamp: projection.caption_stamp(group.id()),
                        },
                        &mut regions,
                        &mut lane,
                        window,
                    );
                    painted.push(PaintedLine::Caption(path.clone()));
                    caption_lines.push(line);
                    line += 1;
                    continue;
                }
                Some(DisplayRow::GroupFooter { group, depth }) => {
                    let Some(group) = projection.groups().get(group) else {
                        continue;
                    };
                    // A footer, too, ends the selection fill's runs above it.
                    for (runs, region) in fills.iter_mut().zip(&mut regions) {
                        runs.end_row(y, &mut region.fills, palette.selected);
                    }
                    let path = group.path();
                    planner.plan_summary(
                        &PlannedSummary {
                            line: AggregateLine::Footer(group),
                            aggregates: group.aggregates(),
                            stamps: projection.group_stamps(group.id()),
                            y,
                            background: Some(palette.pinned),
                            weight: None,
                            label: None,
                            hovered_column: hover.and_then(|(at, column)| {
                                (at == BodyLine::Data(line)).then_some(column)
                            }),
                            row: PaintedRow {
                                pinned: None,
                                position: display_ix,
                                key_hash: footer_hash(path),
                                y,
                                height: row_height,
                                selected: false,
                            },
                            outline: RowOutline {
                                content: RowContent::Footer,
                                level: Some(depth.saturating_add(1)),
                                expanded: None,
                            },
                        },
                        &mut regions,
                        window,
                    );
                    painted.push(PaintedLine::Footer(path.clone()));
                    line += 1;
                    continue;
                }
                None => break,
            };
            let Some(row) = snapshot.row(id) else {
                // A removed row keeps its line while it fades out
                // (`state/fade.rs`), else it is skipped.
                let fading = FadingLine {
                    id,
                    y,
                    display_ix,
                    line: display_row,
                    background: ((self.scroll_row + line) % 2 == 1).then_some(palette.stripe),
                };
                if planner.plan_fading_line(&fading, &snapshot, &mut regions, &mut fills, window) {
                    painted.push(PaintedLine::Ghost(id));
                    line += 1;
                } else {
                    tombstones += 1;
                }
                continue;
            };
            let mut selected = painter.columns(row.key(), display_ix);
            if dragging
                && !selected.is_empty()
                && !selection_policy.is_selectable(&SelectableLine::Row {
                    row: &**row,
                    branch: projection.branch(id),
                })
            {
                selected = ColumnSet::EMPTY;
            }
            // Every visible cell selected: the fill spans the row, as the
            // row background does.
            let whole_row = !selected.is_empty() && {
                let mut with_visible = selected;
                with_visible.union_with(visible_columns);
                with_visible == selected
            };
            // A line of a grouped view or a tree has its level; a tree row
            // with children its state.
            let outline = if structured {
                let depth = display_row.depth();
                RowOutline {
                    content: RowContent::Data,
                    level: Some(depth.saturating_add(1)),
                    expanded: projection
                        .branch(id)
                        .map(|_| expansion.is_row_expanded(row.key(), depth)),
                }
            } else {
                RowOutline::default()
            };
            planner.plan_row(
                &PlannedRow {
                    row,
                    y,
                    changes: RowChanges::Stamps(snapshot.stamps(id)),
                    display_ix,
                    line: Some(display_row),
                    outline,
                    pinned: None,
                    background: ((self.scroll_row + line) % 2 == 1).then_some(palette.stripe),
                    foreground: palette.foreground,
                    selected,
                    whole_row,
                    focused_column: focus
                        .and_then(|(focused, column)| (focused == id).then_some(column)),
                    hovered_column: hover
                        .and_then(|(at, column)| (at == BodyLine::Data(line)).then_some(column)),
                },
                &mut regions,
                Some(&mut fills),
                window,
            );
            painted.push(PaintedLine::Row(id));
            line += 1;
        }
        let rows_end = rows_area.origin.y + row_height * line as f32 - self.scroll_offset;
        for (runs, region) in fills.iter_mut().zip(&mut regions) {
            runs.finish(rows_end, &mut region.fills, palette.selected);
        }

        // The pinned rows: a band background per zone, no stripes, no
        // selection or focus; highlight, styles and tooltips as data rows.
        // The totals row is the band's outermost line.
        let total_label = self.summary_texts.total();
        for edge in EDGES {
            let area = bands.band(edge);
            self.painted_pinned.begin(edge);
            if area.size.height <= px(0.) {
                continue;
            }
            let band = &mut band_regions[band_slot(edge)];
            for region in band.iter_mut() {
                region.quads.push((region.mask(), palette.pinned));
            }
            for (band_line, y) in bands.visible_lines(edge) {
                let hovered_column =
                    hover.and_then(|(at, column)| (at == band_line).then_some(column));
                let ix = match band_line {
                    BodyLine::Pinned(_, ix) => ix,
                    BodyLine::Totals(_) => {
                        planner.plan_summary(
                            &PlannedSummary {
                                line: AggregateLine::Totals(edge),
                                aggregates: projection.totals(),
                                stamps: projection.totals_stamps(),
                                y,
                                background: None,
                                weight: Some(TOTALS_WEIGHT),
                                label: Some(total_label),
                                hovered_column,
                                row: PaintedRow {
                                    pinned: Some(edge),
                                    position: 0,
                                    key_hash: totals_hash(edge),
                                    y,
                                    height: row_height,
                                    selected: false,
                                },
                                outline: RowOutline {
                                    content: RowContent::Totals,
                                    ..RowOutline::default()
                                },
                            },
                            band,
                            window,
                        );
                        continue;
                    }
                    BodyLine::Data(_) => continue,
                };
                let Some(row) = snapshot.pinned_rows(edge).get(ix) else {
                    continue;
                };
                planner.plan_row(
                    &PlannedRow {
                        row,
                        y,
                        changes: RowChanges::Stamps(snapshot.pinned_stamps(edge, ix)),
                        display_ix: ix,
                        line: None,
                        outline: RowOutline::default(),
                        pinned: Some(edge),
                        background: None,
                        foreground: palette.pinned_foreground,
                        selected: ColumnSet::EMPTY,
                        whole_row: false,
                        focused_column: None,
                        hovered_column,
                    },
                    band,
                    None,
                    window,
                );
                self.painted_pinned.push(edge, ix, row.clone());
            }
        }
        let edited_row_painted = planner.edited_row_painted;
        let RowPlanner {
            glyphs,
            any_hot,
            cells,
            icons,
            images_painted,
            hovered,
            a11y,
            links,
            mut hosts,
            captions: CaptionPlanner {
                painted: captions, ..
            },
            ..
        } = planner;
        self.links.rects = links;
        // A row moved under a resting pointer: the cursor follows this frame.
        self.links.hovered = self
            .tooltip
            .position
            .is_some_and(|position| self.links.is_over(position));
        // An open editor whose row is not painted keeps its host.
        if self.is_editing() {
            self.plan_hidden_editor_host(&mut hosts, edited_row_painted, window, cx);
        }

        // One quad per vertical grid line over each run of painted data
        // rows, so none crosses a caption, and over each band.
        let line_top = |line: usize| {
            (rows_area.origin.y + row_height * line as f32 - self.scroll_offset)
                .max(rows_area.origin.y)
                .min(rows_area.bottom())
        };
        let column_lines = ColumnLines {
            columns: &self.columns,
            geometry: &geometry,
            body_x: bounds.origin.x,
            has_right_zone,
            color: palette.row_border,
        };
        // The captions' spans, top to bottom: the zone dividers leave them
        // out too.
        let caption_spans: Vec<(Pixels, Pixels)> = caption_lines
            .iter()
            .map(|&caption| (line_top(caption), line_top(caption + 1)))
            .collect();
        let mut run_start = 0;
        for end in caption_lines.iter().copied().chain([line]) {
            let (top, bottom) = (line_top(run_start), line_top(end));
            if bottom > top {
                column_lines.push(&mut regions, top, bottom - top);
            }
            run_start = end + 1;
        }
        for edge in EDGES {
            let area = bands.band(edge);
            if area.size.height > px(0.) {
                column_lines.push(
                    &mut band_regions[band_slot(edge)],
                    area.origin.y,
                    area.size.height,
                );
            }
        }

        let mut overlay = Vec::new();
        let [left, _, right] = &geometry.zones;
        if left.width > px(0.) {
            push_divider(
                &mut overlay,
                bounds.origin.x + left.width - px(1.),
                bounds,
                &caption_spans,
                palette.divider,
            );
        }
        if right.width > px(0.) {
            push_divider(
                &mut overlay,
                bounds.origin.x + right.x,
                bounds,
                &caption_spans,
                palette.divider,
            );
        }
        // A band owns its edge toward the data rows: a 1 px divider inside
        // it, across the body.
        let top = bands.band(PinnedEdge::Top);
        if top.size.height > px(0.) {
            overlay.push((
                Bounds::new(
                    point(bounds.origin.x, top.bottom() - px(1.)),
                    size(bounds.size.width, px(1.)),
                ),
                palette.divider,
            ));
        }
        let bottom = bands.band(PinnedEdge::Bottom);
        if bottom.size.height > px(0.) {
            overlay.push((
                Bounds::new(
                    point(bounds.origin.x, bottom.origin.y),
                    size(bounds.size.width, px(1.)),
                ),
                palette.divider,
            ));
        }
        // The drop gap of a header drag runs on through the body.
        if let Some(x) = drop_gap_x {
            overlay.push((
                Bounds::new(
                    point(bounds.origin.x + x - px(1.), bounds.origin.y),
                    size(px(2.), bounds.size.height),
                ),
                drop_gap_color,
            ));
        }

        // A drag over the rows, placed again on the lines just painted if
        // they scrolled under it, and its indicator.
        self.retarget_drop();
        let drop_indicator = self.drop_indicator(rows_area, drop_fill, drop_border);

        if any_hot {
            self.schedule_fade(policy.next_tick(now_ms), now_ms, cx);
        }
        self.stats.frames += 1;
        self.stats.visible_cells = cells;
        self.stats.icons = icons;
        self.stats.images = images_painted;
        self.stats.tombstones = tombstones;
        // Fading rows painted.
        self.stats.fading_lines = self
            .painted
            .iter()
            .filter(|line| matches!(line, PaintedLine::Ghost(_)))
            .count();
        self.stats.painted_captions = captions;
        // The body times the hosts' layout and prepaint.
        self.stats.cell_hosts = hosts.len();
        if hosts.is_empty() {
            self.stats.last_hosts_prepaint = Duration::ZERO;
        }
        self.stats.last_prepaint = profile_elapsed(started);
        #[cfg(any(test, feature = "test-support"))]
        {
            self.focus_visible = focus_visible;
        }
        self.publish_scroll();
        let hover = self.finish_hover(hovered);
        let a11y = a11y.map(|input| self.finish_a11y(input, window));
        self.stats.a11y_rows = a11y.as_ref().map_or(0, |plan| plan.rows().len());
        self.stats.a11y_cells = a11y.as_ref().map_or(0, |plan| plan.cell_count());
        if a11y.is_none() {
            self.stats.last_a11y_plan = Duration::ZERO;
            #[cfg(any(test, feature = "test-support"))]
            {
                self.last_a11y = None;
            }
        }

        // The scrolling region first in each band, so frozen columns paint
        // over it.
        // The caption lane last of the data rows, over every zone.
        let mut planned_regions = Vec::with_capacity(10);
        for (band, [left, scrolling, right]) in
            [regions].into_iter().chain(band_regions).enumerate()
        {
            planned_regions.extend([scrolling, left, right]);
            if band == 0 {
                planned_regions.push(std::mem::replace(&mut lane, Region::new(Bounds::default())));
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            self.painted_icons.clear();
            self.painted_icons
                .extend(planned_regions.iter().flat_map(|region| {
                    region
                        .icons
                        .iter()
                        .map(|icon| (icon.bounds, icon.path.clone(), icon.color))
                }));
            self.painted_pills.clear();
            self.painted_pills
                .extend(planned_regions.iter().flat_map(|region| {
                    region
                        .pills
                        .iter()
                        .map(|(bounds, color, _)| (*bounds, *color))
                }));
            self.painted_images.clear();
            self.painted_images
                .extend(planned_regions.iter().flat_map(|region| {
                    region.images.iter().map(|image| {
                        (
                            image.bounds,
                            image.image_bounds,
                            image.corner_radius,
                            image.data.clone(),
                        )
                    })
                }));
        }
        FramePlan {
            regions: planned_regions,
            overlay,
            ellipsis,
            line_height: row_height,
            glyphs,
            hover,
            a11y,
            hosts,
            drop_indicator,
        }
    }

    /// The icons the body painted in the last frame: their bounds in window
    /// coordinates, asset paths and colours. Icons of icon values,
    /// disclosure chevrons and trend arrows alike; not the icons of
    /// elements in cells. For tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn painted_icons(&self) -> &[(Bounds<Pixels>, SharedString, Hsla)] {
        &self.painted_icons
    }

    /// The tags of group captions the body painted in the last frame:
    /// their bounds in window coordinates and colours. For tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn painted_caption_tags(&self) -> &[(Bounds<Pixels>, Hsla)] {
        &self.painted_pills
    }

    /// The images the body painted in the last frame:
    /// each one's room (the cell without its padding, which clips it), its
    /// bounds as fitted, its corner radius and its data, in window
    /// coordinates. For tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn painted_images(&self) -> &[PaintedImage] {
        &self.painted_images
    }

    /// Plans the accessibility tree from what the walk gathered.
    fn finish_a11y(&mut self, input: A11yInput, window: &Window) -> Rc<A11yPlan> {
        let started = profile_start();
        let plan = plan_a11y(input);
        // GPUI builds the nodes from the plan in prepaint, only while the
        // window's accessibility is active; when a test forces the plan,
        // the frame builds them itself, so they are run and timed.
        #[cfg(any(test, feature = "test-support"))]
        if !window.is_a11y_active() {
            plan.build_nodes();
        }
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = window;
        self.stats.last_a11y_plan = profile_elapsed(started);
        let plan = Rc::new(plan);
        #[cfg(any(test, feature = "test-support"))]
        {
            self.last_a11y = Some(plan.clone());
        }
        plan
    }

    /// The row of the last frame's accessibility plan (planned while a
    /// screen reader listens, or [`Self::set_a11y_forced`]) whose element
    /// id is `id`.
    #[cfg(any(test, feature = "test-support"))]
    fn last_a11y_row(
        &self,
        id: &gpui_kit::ElementId,
    ) -> Option<(&A11yPlan, &super::a11y::A11yRow)> {
        let plan = self.last_a11y.as_deref()?;
        let row = plan.rows().iter().find(|row| {
            *id == gpui_kit::ElementId::NamedInteger(
                SharedString::new_static(row.id_name()),
                row.key_hash(),
            )
        })?;
        Some((plan, row))
    }

    /// The accessible name of the row with element id `id` in the last
    /// frame's plan, such as "Total" for the totals row; `None` for a row
    /// without one or not planned.
    #[cfg(any(test, feature = "test-support"))]
    pub fn a11y_row_label(&self, id: &gpui_kit::ElementId) -> Option<String> {
        let (plan, row) = self.last_a11y_row(id)?;
        let mut node = gpui_kit::accesskit::Node::new(gpui_kit::accesskit::Role::Row);
        plan.write_row(row, &mut node);
        node.label().map(str::to_owned)
    }

    /// The synthetic cells of the row with element id `id` in the last
    /// frame's plan, left to right: each one's label and description. The
    /// focused cell is not among them; empty for a row not planned.
    #[cfg(any(test, feature = "test-support"))]
    pub fn a11y_row_cells(&self, id: &gpui_kit::ElementId) -> Vec<(String, Option<String>)> {
        let Some((plan, row)) = self.last_a11y_row(id) else {
            return Vec::new();
        };
        plan.cells(row)
            .iter()
            .map(|cell| {
                let node = plan.cell_node(row, cell);
                (
                    node.label().unwrap_or_default().to_owned(),
                    node.description().map(str::to_owned),
                )
            })
            .collect()
    }
}

/// One row to plan: a data row, a pinned row or a fading row.
pub(super) struct PlannedRow<'r, R: GridRow> {
    pub(super) row: &'r Arc<R>,
    /// The top edge in window coordinates.
    pub(super) y: Pixels,
    /// The stamps of a live row's cells, or the removal of a fading row.
    pub(super) changes: RowChanges<'r>,
    /// The display position; for a pinned row, its position in its band.
    pub(super) display_ix: usize,
    /// The shown line of a data row; `None` for a pinned row.
    pub(super) line: Option<DisplayRow>,
    /// What the row is in a grouped view or a tree, for accessibility.
    pub(super) outline: RowOutline,
    pub(super) pinned: Option<PinnedEdge>,
    pub(super) background: Option<Hsla>,
    /// The text colour unless a style sets one.
    pub(super) foreground: Hsla,
    pub(super) selected: ColumnSet,
    /// Every visible cell is selected: one fill across the row.
    pub(super) whole_row: bool,
    pub(super) focused_column: Option<ColumnIx>,
    pub(super) hovered_column: Option<ColumnIx>,
}

impl<R: GridRow> PlannedRow<'_, R> {
    /// Returns true for a row of the store; false for a removed row that
    /// fades out, whose elements in cells take no press, focus or edit.
    pub(super) fn is_live(&self) -> bool {
        matches!(self.changes, RowChanges::Stamps(_))
    }

    /// The hash of the row's identity in element ids: its key, or for a
    /// pinned row its band and key (a band's keys are its own).
    pub(super) fn key_hash(&self) -> u64 {
        match self.pinned {
            None => key_hash(self.row.key()),
            Some(edge) => pinned_key_hash(edge, self.row.key()),
        }
    }
}

/// The paths of the change arrows, made once per frame.
pub(super) struct Arrows {
    up: SharedString,
    down: SharedString,
    /// The error mark of a refused edit.
    error: SharedString,
}

/// The text of one cell to place in a region: a data cell's or an
/// aggregate cell's.
pub(super) struct CellText<'t> {
    pub(super) text: &'t str,
    /// The cell's left edge and width in window coordinates.
    pub(super) x: Pixels,
    pub(super) width: Pixels,
    pub(super) y: Pixels,
    /// Aligned to the right edge (numbers).
    pub(super) right: bool,
    pub(super) lead: Option<CellLead>,
    pub(super) trend: Option<ChangeDirection>,
    pub(super) weight: Option<FontWeight>,
    pub(super) color: Hsla,
}

/// What placing a cell's text needs of the frame.
pub(super) struct CellTextContext<'c> {
    pub(super) text_cache: &'c mut TextCaches,
    pub(super) glyphs: &'c mut GlyphArena,
    pub(super) arrows: &'c Arrows,
    pub(super) row_height: Pixels,
}

/// Places a cell's text in `region`, with the icon at its start and its
/// trend arrow: an indent and an arrow take room from the text, which keeps
/// its place. Returns whether the text is truncated, and the icons placed.
#[inline]
pub(super) fn place_cell_text(
    cell: CellText<'_>,
    frame: &mut CellTextContext<'_>,
    region: &mut Region,
    window: &mut Window,
) -> (bool, usize) {
    place_marked_cell_text(cell, None, frame, region, window)
}

/// [`place_cell_text`] with an error mark of a refused edit: an icon of
/// `error`'s colour in the trend arrow's lane, which it takes from the
/// arrow (an error matters more than a direction).
#[inline]
pub(super) fn place_marked_cell_text(
    cell: CellText<'_>,
    error: Option<Hsla>,
    frame: &mut CellTextContext<'_>,
    region: &mut Region,
    window: &mut Window,
) -> (bool, usize) {
    let CellText {
        text,
        x,
        width,
        y,
        right,
        lead,
        trend,
        weight,
        color,
    } = cell;
    let row_height = frame.row_height;
    let indent = lead.as_ref().map_or(px(0.), |lead| lead.indent);
    let trend_lane = if trend.is_some() || error.is_some() {
        TREND_LANE
    } else {
        px(0.)
    };
    let mut truncated = false;
    let mut icons = 0;
    if !text.is_empty() {
        let cache = frame.text_cache.for_weight(weight);
        let layout = cache.layout(text, window, frame.glyphs);
        let ellipsis = weight.map(|_| cache.ellipsis(window));
        let available = (width - CELL_PADDING * 2. - indent - trend_lane).max(px(0.));
        truncated = layout.width() > available;
        let text_x = if right {
            x + width - CELL_PADDING - layout.width().min(available)
        } else {
            x + CELL_PADDING + indent
        };
        region.texts.push(TextItem {
            origin: point(text_x, y),
            layout,
            max_width: available,
            color,
            ellipsis,
        });
    }
    if let Some(icon) = lead.and_then(|lead| lead.icon) {
        let lane_x = x + CELL_PADDING + indent - LEAD_ICON_LANE;
        region.icons.push(IconItem {
            bounds: Bounds::new(
                point(
                    lane_x + (LEAD_ICON_LANE - icon.size) / 2.,
                    y + (row_height - icon.size) / 2.,
                ),
                size(icon.size, icon.size),
            ),
            path: icon.path,
            color: icon.color,
        });
        icons += 1;
    }
    let lane_icon = match (error, trend) {
        (Some(error), _) => Some((frame.arrows.error_path(), error)),
        (None, Some(direction)) => Some((frame.arrows.path(direction), color)),
        (None, None) => None,
    };
    if let Some((path, color)) = lane_icon {
        let lane_x = if right {
            x + CELL_PADDING + indent
        } else {
            x + width - CELL_PADDING - TREND_LANE
        };
        region.icons.push(IconItem {
            bounds: Bounds::new(
                point(
                    lane_x + (TREND_LANE - TREND_ICON) / 2.,
                    y + (row_height - TREND_ICON) / 2.,
                ),
                size(TREND_ICON, TREND_ICON),
            ),
            path,
            color,
        });
        icons += 1;
    }
    (truncated, icons)
}

/// The side of an icon in a cell, `CELL_ICON`: 14 px in rows up to 30 px
/// (`xsmall`, `small`), 16 px in taller ones (`medium`, `large`), as
/// gpui-kit's `Icon` at `small` and `medium`.
pub(super) fn cell_icon_size(row_height: Pixels) -> Pixels {
    if row_height <= px(30.) {
        px(14.)
    } else {
        px(16.)
    }
}

/// The icon of a [`CellValue::Icon`] to place in its cell.
pub(super) struct CellIcon {
    pub(super) path: SharedString,
    /// The cell's left edge after a tree row's indent, and the width left,
    /// in window coordinates.
    pub(super) x: Pixels,
    pub(super) width: Pixels,
    pub(super) y: Pixels,
    pub(super) align: ColumnAlign,
    pub(super) color: Hsla,
}

/// Places an icon value's icon in `region`, after the glyphs: centred in
/// its row, at the start or the end of the cell's padded room by the
/// column's alignment, in its middle for `Auto` (a column of statuses reads
/// as a column). An icon wider than the room is centred in the cell; one
/// wider than the cell is left out. Returns the icons placed.
pub(super) fn place_cell_icon(icon: CellIcon, row_height: Pixels, region: &mut Region) -> usize {
    let side = cell_icon_size(row_height);
    let (start, end) = (icon.x + CELL_PADDING, icon.x + icon.width - CELL_PADDING);
    let x = if end - start >= side {
        match icon.align {
            ColumnAlign::Start => start,
            ColumnAlign::End => end - side,
            ColumnAlign::Auto => start + (end - start - side) / 2.,
        }
    } else if icon.width >= side {
        icon.x + (icon.width - side) / 2.
    } else {
        return 0;
    };
    region.icons.push(IconItem {
        bounds: Bounds::new(
            point(x, icon.y + (row_height - side) / 2.),
            size(side, side),
        ),
        path: icon.path,
        color: icon.color,
    });
    1
}

/// The side of the square an image gets in a row of `row_height`: the row
/// without the cell padding above and below; the width autofit gives an
/// image column.
pub(super) fn cell_image_size(row_height: Pixels) -> Pixels {
    (row_height - CELL_PADDING * 2.).max(px(0.))
}

/// The image of a [`CellValue::Image`] to place in its cell.
pub(super) struct CellPicture {
    pub(super) data: Arc<RenderImage>,
    /// The cell's left edge after a tree row's indent, and the width left,
    /// in window coordinates.
    pub(super) x: Pixels,
    pub(super) width: Pixels,
    pub(super) y: Pixels,
    pub(super) align: ColumnAlign,
    pub(super) fit: ImageFit,
    pub(super) corner_radius: Pixels,
}

/// Places an image value's image in `region`, after the icons: in the
/// cell's room (the cell without its padding on every side), fitted by the
/// column's fit and clipped to the room. An image narrower than the room
/// goes to the start or the end of it by the column's alignment, in its
/// middle for `Auto`. Returns the images placed: none without room or for
/// an image of no size.
pub(super) fn place_cell_image(
    image: CellPicture,
    row_height: Pixels,
    region: &mut Region,
) -> usize {
    let room = Bounds::new(
        point(image.x + CELL_PADDING, image.y + CELL_PADDING),
        size(
            image.width - CELL_PADDING * 2.,
            row_height - CELL_PADDING * 2.,
        ),
    );
    let image_size = image.data.size(0);
    if room.size.width <= px(0.)
        || room.size.height <= px(0.)
        || image_size.width.0 <= 0
        || image_size.height.0 <= 0
    {
        return 0;
    }
    let mut fitted = image.fit.object_fit().get_bounds(room, image_size);
    if fitted.size.width < room.size.width {
        fitted.origin.x = match image.align {
            ColumnAlign::Start => room.origin.x,
            ColumnAlign::End => room.right() - fitted.size.width,
            ColumnAlign::Auto => fitted.origin.x,
        };
    }
    region.images.push(ImageItem {
        bounds: room,
        image_bounds: fitted,
        corner_radius: image.corner_radius,
        data: image.data,
    });
    1
}

/// What [`LiveGridState::painted_images`] gives of an image: its room, its
/// fitted bounds, its corner radius and its data.
#[cfg(any(test, feature = "test-support"))]
pub(super) type PaintedImage = (Bounds<Pixels>, Bounds<Pixels>, Pixels, Arc<RenderImage>);

impl Arrows {
    pub(super) fn new() -> Self {
        Self {
            up: IconName::ArrowUp.path(),
            down: IconName::ArrowDown.path(),
            error: IconName::CircleAlert.path(),
        }
    }

    pub(super) fn error_path(&self) -> SharedString {
        self.error.clone()
    }

    pub(super) fn path(&self, direction: ChangeDirection) -> SharedString {
        match direction {
            ChangeDirection::Up => self.up.clone(),
            ChangeDirection::Down => self.down.clone(),
        }
    }
}

/// What every row of a frame is planned with: the columns, the palettes,
/// the highlight tick and the delegate's hooks; and what the rows planned
/// so far add up to.
pub(super) struct RowPlanner<'a, D: LiveGridDelegate> {
    pub(super) delegate: &'a D,
    pub(super) app: &'a mut App,
    pub(super) text_cache: &'a mut TextCaches,
    pub(super) planned: &'a [PlannedColumn<ColumnOf<D>>],
    pub(super) zone_ranges: &'a [Range<usize>; 3],
    pub(super) row_extents: [Pixels; 3],
    pub(super) palette: Palette,
    /// Paints a hot cell's default style.
    pub(super) change_paint: &'a ChangePaint,
    pub(super) change_palette: ChangePalette,
    /// The starts of the cells of shown lines, if any line has one.
    pub(super) lead: Option<LeadProvider<'a>>,
    pub(super) arrows: Arrows,
    pub(super) policy: HighlightPolicy,
    pub(super) tick: u32,
    pub(super) row_height: Pixels,
    pub(super) focus_visible: bool,
    pub(super) buffer: String,
    pub(super) glyphs: GlyphArena,
    /// A planned cell is highlighted: the fade timer is needed.
    pub(super) any_hot: bool,
    pub(super) cells: usize,
    pub(super) icons: usize,
    pub(super) hovered: Option<HoveredCell>,
    /// What the accessibility tree needs of the rows planned so far, while
    /// it is built.
    pub(super) a11y: Option<A11yInput>,
    /// The grid holds the focus and the tree is built: the focused cell is
    /// planned as the active descendant.
    pub(super) a11y_focus: bool,
    // Real elements in cells.
    /// What a row's element is clipped to: the top band, the data rows,
    /// the bottom band.
    pub(super) band_clips: [Bounds<Pixels>; 3],
    /// The cell hosts planned so far, row by row.
    pub(super) hosts: HostPlan,
    /// Where the texts of links were painted, for the pointer and clicks.
    pub(super) links: Vec<Bounds<Pixels>>,
    /// The open edit.
    pub(super) edit: &'a EditSlot<KeyOf<D>, ColumnOf<D>>,
    /// The edited row was painted this frame.
    pub(super) edited_row_painted: bool,
    /// The commits that wait: their cells show the value committed.
    pub(super) pending: &'a PendingEdits<KeyOf<D>, ColumnOf<D>>,
    /// A test's host for every cell of a column.
    #[cfg(any(test, feature = "test-support"))]
    pub(super) test_host: Option<TestCellHost>,
    /// The grid, for the listeners of cell hosts.
    pub(super) grid: WeakEntity<LiveGridState<D>>,
    /// Where the focused control's handler of Space goes.
    pub(super) widgets: &'a WidgetSlot<KeyOf<D>, ColumnOf<D>>,
    /// The text of a hosted cell, for its element.
    pub(super) host_text: String,
    /// Where the images of image cells come from, and how many were painted.
    pub(super) images: &'a CellImages,
    pub(super) images_painted: usize,
    /// What group captions are planned with (`state/groups.rs`).
    pub(super) captions: CaptionPlanner<'a, D::Row>,
}

/// The element id name of a fading row's element of cell hosts: a row
/// written again under the same key while its old version fades out does
/// not share element state with it.
const FADING_ROW_ID_NAME: &str = "live-grid-fading-row";

/// A cell that may get a real element: what the steps that plan
/// cell hosts read. Cells of live rows get one; a fading row's `Element`
/// and `Control` cells get one without presses.
pub(super) struct HostCell<'c, R: GridRow> {
    pub(super) row: &'c PlannedRow<'c, R>,
    pub(super) column: R::Column,
    pub(super) view: ViewColumnIx,
    pub(super) zone: ColumnZone,
    pub(super) align: ColumnAlign,
    /// What the column paints.
    pub(super) content: CellContent,
    /// The column title.
    pub(super) title: &'c SharedString,
    /// How the column shows its numbers.
    pub(super) number_format: NumberFormat,
    /// The cell in window coordinates.
    pub(super) bounds: Bounds<Pixels>,
    /// A tree row's indent and chevron lane at the cell's start: a host
    /// begins after it.
    pub(super) indent: Pixels,
    /// The value the cell shows: the store's, or a waiting commit's.
    pub(super) value: CellValue<'c>,
    pub(super) change: Option<CellChange>,
    pub(super) selected: bool,
    pub(super) focused: bool,
    /// What the cell's commits mark it with.
    pub(super) edit_state: Option<&'c EditState>,
}

impl<R: GridRow> HostCell<'_, R> {
    /// Where the cell's host goes: the cell after a tree row's indent and
    /// chevron lane, in window coordinates.
    pub(super) fn host_bounds(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(self.bounds.origin.x + self.indent, self.bounds.origin.y),
            size(
                (self.bounds.size.width - self.indent).max(px(0.)),
                self.bounds.size.height,
            ),
        )
    }
}

/// Who paints a cell of a live row: the body (its text), or a host over it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CellHosting {
    /// The body: text or an icon.
    Body,
    /// An open editor's field or a column's element: the host is the
    /// cell's accessible node.
    Real,
    /// A test's host ([`LiveGridState::set_test_cell_host`]): the cell
    /// stays synthetic for accessibility.
    Test,
}

/// What [`LiveGridState::set_test_cell_host`] gives: the column whose cells
/// get a host, and what makes the host's element.
#[cfg(any(test, feature = "test-support"))]
pub(super) type TestCellHost = (
    ColumnIx,
    Rc<dyn Fn(gpui_kit::Stateful<gpui_kit::Div>) -> gpui_kit::Stateful<gpui_kit::Div>>,
);

/// What group captions of a frame are planned with, and how many were.
pub(super) struct CaptionPlanner<'a, R: GridRow> {
    pub(super) palette: CaptionPalette,
    pub(super) texts: &'a CaptionTexts,
    /// The view's aggregates, for their names.
    pub(super) aggregates: &'a [(R::Column, Aggregate<R>)],
    pub(super) chevrons: Chevrons,
    pub(super) chevron_size: Pixels,
    /// The body's left edge in window coordinates.
    pub(super) body_x: Pixels,
    /// The right edge of the painted rows in window coordinates.
    pub(super) row_right: Pixels,
    /// The number of visible columns.
    pub(super) visible_columns: usize,
    /// The caption text, written by the delegate's hook.
    pub(super) caption: String,
    /// The text of the caption's tag, written by the delegate's hook.
    pub(super) tag: String,
    /// The text of the group's key value.
    pub(super) value: String,
    /// Captions planned so far.
    pub(super) painted: usize,
}

/// The start of a tree row's cell in the tree column: the indent of its
/// depth, and a chevron if the row has children.
struct TreeLeads<'a, R: GridRow> {
    column: ColumnIx,
    projection: &'a Projection,
    snapshot: &'a StoreSnapshot<R>,
    expansion: &'a Expansion<R::Key>,
    chevrons: Chevrons,
    color: Hsla,
    size: Pixels,
}

impl<R: GridRow> TreeLeads<'_, R> {
    /// The start of a cell of a shown line: in the tree column, the indent
    /// of the row's depth and the chevron lane, with the chevron by the
    /// view's expansion if the row has children; a leaf keeps the lane
    /// empty, so its text lines up with its siblings' (Design Guides:
    /// "Preserve the spine through optional content").
    fn lead(&self, line: &DisplayRow, column: ColumnIx) -> Option<CellLead> {
        if column != self.column {
            return None;
        }
        let DisplayRow::Data { row, depth } = *line else {
            return None;
        };
        let icon = self.projection.branch(row).and_then(|_| {
            let key = self.snapshot.row(row)?.key();
            Some(LeadIcon {
                path: self
                    .chevrons
                    .path(self.expansion.is_row_expanded(key, depth)),
                color: self.color,
                size: self.size,
            })
        });
        Some(CellLead {
            indent: INDENT * f32::from(depth) + LEAD_ICON_LANE,
            icon,
        })
    }
}

/// A zone divider over the body, at window x `x`, with the spans of group
/// captions left out.
fn push_divider(
    overlay: &mut Vec<(Bounds<Pixels>, Hsla)>,
    x: Pixels,
    body: Bounds<Pixels>,
    gaps: &[(Pixels, Pixels)],
    color: Hsla,
) {
    let mut top = body.origin.y;
    for &(gap_top, gap_bottom) in gaps.iter().chain([&(body.bottom(), body.bottom())]) {
        if gap_top > top {
            overlay.push((
                Bounds::new(point(x, top), size(px(1.), gap_top - top)),
                color,
            ));
        }
        top = top.max(gap_bottom);
    }
}

impl<D: LiveGridDelegate> RowPlanner<'_, D> {
    /// Plans one row across the three zone regions of its band: the row
    /// background, the selection fill (data rows only), every cell's
    /// highlight, style, text and focus outline, the row line, and the
    /// hovered cell's tooltip; while the accessibility tree is built, the
    /// row, its cells' texts and the focused cell for it.
    pub(super) fn plan_row(
        &mut self,
        row: &PlannedRow<'_, D::Row>,
        regions: &mut [Region; 3],
        mut fills: Option<&mut [FillRuns; 3]>,
        window: &mut Window,
    ) {
        let (y, row_height, palette) = (row.y, self.row_height, self.palette);
        let a11y_key = self.a11y.as_mut().map(|a11y| {
            let key_hash = row.key_hash();
            a11y.push_outlined_row(
                PaintedRow {
                    pinned: row.pinned,
                    position: row.display_ix,
                    key_hash,
                    y,
                    height: row_height,
                    selected: !row.selected.is_empty(),
                },
                row.outline,
            );
            key_hash
        });
        // The cells of a live row may get real elements (cell hosts); the
        // open editor's cell gets its field, and a cell whose commit waits
        // shows the value committed. One check per row when none of them
        // is there.
        let live = row.is_live();
        let first_host = self.hosts.len();
        let edit_column = if live && row.pinned.is_none() {
            self.edit.column_in(row.row.key()).map(GridColumns::ix)
        } else {
            None
        };
        self.edited_row_painted |= edit_column.is_some();
        let pending = self.pending;
        let overlaid = live && row.pinned.is_none() && !pending.is_empty();
        let (planned_columns, zone_ranges) = (self.planned, self.zone_ranges);
        for (slot, region) in regions.iter_mut().enumerate() {
            let row_x = region.mask().origin.x;
            let row_width = self.row_extents[slot];
            if let Some(background) = row.background {
                region.quads.push((
                    Bounds::new(point(row_x, y), size(row_width, row_height)),
                    background,
                ));
            }
            if row.whole_row
                && let Some(fills) = fills.as_deref_mut()
            {
                fills[slot].push(row_x, row_width);
            }
            for column_slot in zone_ranges[slot].clone() {
                let planned = &planned_columns[column_slot];
                let column = planned.column;
                let (x, width) = (planned.x, planned.width);
                // A cell whose commit waits shows its value; a refused one,
                // its mark.
                let marks = if overlaid {
                    pending.marks(row.row.key(), column)
                } else {
                    None
                };
                let value = match marks.and_then(|marks| marks.value) {
                    Some(value) => value.as_cell(),
                    None => row.row.value(column),
                };
                let change = row.changes.cell(column.ix(), &self.policy, self.tick);
                self.any_hot |= change.is_some();

                let selected = row.selected.contains(column.ix());
                let focused = row.focused_column == Some(column.ix());
                let mut style = CellStyle::default();
                if let Some(change) = &change {
                    self.change_paint
                        .paint(change, &self.change_palette, &mut style);
                }
                // The edit's look, after the change paint and before the
                // delegate, which may paint over it.
                if let Some(marks) = marks {
                    if marks.pending {
                        style.set_foreground(palette.muted);
                    }
                    style.set_error_mark(marks.failed);
                }
                let edit_state = marks.and_then(|marks| marks.state);
                self.delegate.cell_style(
                    &CellStyleContext {
                        row: &**row.row,
                        column,
                        value,
                        change,
                        display_ix: row.display_ix,
                        pinned: row.pinned,
                        selected,
                        focused,
                        edit_state,
                    },
                    &mut style,
                    &*self.app,
                );
                let cell_bounds = Bounds::new(point(x, y), size(width, row_height));
                if let Some(background) = style.background {
                    region.quads.push((cell_bounds, background));
                }
                if selected
                    && !row.whole_row
                    && let Some(fills) = fills.as_deref_mut()
                {
                    fills[slot].push(x, width);
                }
                if focused && self.focus_visible {
                    region
                        .outline
                        .extend(focus_outline(cell_bounds).map(|bounds| (bounds, palette.ring)));
                }

                // An indent and a trend arrow take room from the text; the
                // text keeps its place.
                let lead = match (self.lead, &row.line) {
                    (Some(lead), Some(line)) => lead(line, column.ix()),
                    _ => None,
                };
                let indent = lead.as_ref().map_or(px(0.), |lead| lead.indent);
                // A hosted cell shows its element, not its text. A
                // fading row keeps the elements of its `Element` and
                // `Control` cells, without their presses.
                let edited = edit_column == Some(column.ix());
                let hosting = if planned.content != CellContent::Text
                    || (live && (edited || self.has_test_host(column.ix())))
                {
                    let cell = HostCell {
                        row,
                        column,
                        view: planned.view,
                        zone: ColumnZone::ALL[slot],
                        align: planned.align,
                        content: planned.content,
                        title: &planned.title,
                        number_format: planned.number_format,
                        bounds: cell_bounds,
                        indent,
                        value,
                        change,
                        selected,
                        focused,
                        edit_state,
                    };
                    if (edited && self.plan_editor_host(&cell, window))
                        || (planned.content != CellContent::Text
                            && self.plan_cell_host(&cell, window))
                    {
                        CellHosting::Real
                    } else if live && self.plan_test_host(&cell) {
                        CellHosting::Test
                    } else {
                        CellHosting::Body
                    }
                } else {
                    CellHosting::Body
                };
                let hosted = hosting != CellHosting::Body;
                let right = match planned.align {
                    ColumnAlign::End => true,
                    ColumnAlign::Start => false,
                    ColumnAlign::Auto => value.is_number(),
                };
                let text = display_text_in(value, planned.number_format, &mut self.buffer);
                // An icon value paints its icon instead of
                // its text, which the tooltip shows (truncated).
                let icon = match value {
                    CellValue::Icon(icon) if !hosted => Some(icon),
                    _ => None,
                };
                // An image value paints its image, likewise.
                let image = match value {
                    CellValue::Image(image) if !hosted => Some(image),
                    _ => None,
                };
                let pictured = icon.is_some() || image.is_some();
                // A link: the theme's link colour unless a style sets one,
                // underlined.
                let link = matches!(value, CellValue::Link(_)) && !hosted;
                let color =
                    style
                        .foreground
                        .unwrap_or(if link { palette.link } else { row.foreground });
                // A hosted cell keeps its start's icon (a tree row's chevron)
                // and paints no text or trend arrow; nor does an icon or an
                // image.
                let error = (style.error_mark && !hosted).then_some(palette.danger);
                let (truncated, icons) = place_marked_cell_text(
                    CellText {
                        text: if hosted || pictured { "" } else { text },
                        x,
                        width,
                        y,
                        right,
                        lead,
                        trend: style.trend.filter(|_| !hosted && !pictured),
                        weight: style.font_weight,
                        color,
                    },
                    error,
                    &mut CellTextContext {
                        text_cache: &mut *self.text_cache,
                        glyphs: &mut self.glyphs,
                        arrows: &self.arrows,
                        row_height,
                    },
                    region,
                    window,
                );
                let truncated = truncated || pictured;
                self.icons += icons;
                if link && !text.is_empty() {
                    let layout = self.text_cache.for_weight(style.font_weight).layout(
                        text,
                        window,
                        &mut self.glyphs,
                    );
                    let (ascent, descent) = layout.ascent_descent();
                    let trend_lane = if style.trend.is_some() || error.is_some() {
                        TREND_LANE
                    } else {
                        px(0.)
                    };
                    let available = (width - CELL_PADDING * 2. - indent - trend_lane).max(px(0.));
                    let shown = layout.width().min(available);
                    let text_x = if right {
                        x + width - CELL_PADDING - shown
                    } else {
                        x + CELL_PADDING + indent
                    };
                    let baseline = y + (row_height - ascent - descent) / 2. + ascent;
                    // As gpui-kit's `Link`: the line at half strength, full
                    // under the pointer.
                    let hovered = row.hovered_column == Some(column.ix());
                    let line = if hovered {
                        color
                    } else {
                        color.opacity(color.a * 0.5)
                    };
                    region.lines.push((
                        Bounds::new(
                            point(text_x, (baseline + descent * 0.5).round()),
                            size(shown, px(1.)),
                        ),
                        line,
                    ));
                    self.links
                        .push(Bounds::new(point(text_x, y), size(shown, row_height)));
                }
                if let Some(icon) = icon {
                    self.icons += place_cell_icon(
                        CellIcon {
                            path: self.text_cache.icon_path(icon.path()),
                            x: x + indent,
                            width: width - indent,
                            y,
                            align: planned.align,
                            color,
                        },
                        row_height,
                        region,
                    );
                }
                // Loading or missing, the cell stays empty: no placeholder.
                if let Some(image) = image
                    && let CellImage::Ready(data) = self.images.image(image.id(), window, self.app)
                {
                    self.images_painted += place_cell_image(
                        CellPicture {
                            data,
                            x: x + indent,
                            width: width - indent,
                            y,
                            align: planned.align,
                            fit: planned.image_fit,
                            corner_radius: planned.image_radius,
                        },
                        row_height,
                        region,
                    );
                }
                if let Some(a11y) = &mut self.a11y {
                    // A host is the cell's node; a control's element
                    // names itself.
                    let label = if hosting == CellHosting::Real
                        && !edited
                        && planned.content == CellContent::Control
                    {
                        ""
                    } else {
                        text
                    };
                    a11y.push_cell(column_slot, label, selected);
                    if hosting == CellHosting::Real {
                        a11y.set_real_cell();
                    }
                    // "rising" or "falling".
                    if let Some(trend) = style.trend {
                        a11y.set_trend(trend);
                    }
                    if link {
                        a11y.set_edit_note(&self.captions.texts.a11y().link);
                    }
                    // "saving", "not saved" and why.
                    match edit_state {
                        Some(EditState::Pending) => {
                            a11y.set_edit_note(&t!("live_grid.a11y.saving"))
                        }
                        Some(EditState::Failed(error)) => a11y.set_edit_note(&format!(
                            "{}. {}",
                            t!("live_grid.a11y.not_saved"),
                            error.message()
                        )),
                        _ => {}
                    }
                }
                self.cells += 1;
                let hovered = row.hovered_column == Some(column.ix());
                let a11y_focused = focused && self.a11y_focus;
                if !(hovered || a11y_focused) {
                    continue;
                }
                // One call serves both the hovered and the focused cell.
                let tooltip = self.delegate.tooltip(
                    &CellTooltipContext {
                        row: &**row.row,
                        column,
                        value,
                        text,
                        truncated,
                        edit_state,
                    },
                    &*self.app,
                );
                if a11y_focused && let (Some(a11y), Some(key_hash)) = (&mut self.a11y, a11y_key) {
                    // A text tooltip describes the focused cell after its
                    // column, unless it only repeats the cell's full text,
                    // the label already (the default tooltip of a truncated
                    // cell).
                    let description = tooltip
                        .as_ref()
                        .and_then(CellTooltip::as_text)
                        .map(|tooltip| tooltip.as_ref())
                        .filter(|tooltip| *tooltip != text);
                    a11y.set_focus(key_hash, column.ix(), description);
                }
                if hovered {
                    // A pinned row's key is of its band's own key space: the
                    // tooltip slot of a band row hashes the band with it.
                    self.hovered = Some(HoveredCell {
                        key_hash: a11y_key.unwrap_or_else(|| row.key_hash()),
                        column: column.ix(),
                        id: column.id(),
                        bounds: cell_bounds.intersect(&region.mask()),
                        tooltip,
                    });
                }
            }
            if let Some(fills) = fills.as_deref_mut() {
                fills[slot].end_row(y, &mut region.fills, palette.selected);
            }
            region.lines.push((
                Bounds::new(
                    point(row_x, y + row_height - px(1.)),
                    size(row_width, px(1.)),
                ),
                palette.row_border,
            ));
        }
        if self.hosts.len() > first_host {
            let clip = self.band_clips[match row.pinned {
                Some(PinnedEdge::Top) => 0,
                None => 1,
                Some(PinnedEdge::Bottom) => 2,
            }];
            self.hosts.end_row(
                first_host,
                if live {
                    data_row_id_name(row.pinned)
                } else {
                    FADING_ROW_ID_NAME
                },
                a11y_key.unwrap_or_else(|| row.key_hash()),
                Bounds::new(point(clip.origin.x, y), size(clip.size.width, row_height)),
                clip,
            );
        }
    }

    /// Returns true if a test gives the cells of `column` a host.
    #[inline]
    fn has_test_host(&self, column: ColumnIx) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.test_host
                .as_ref()
                .is_some_and(|(hosted, _)| *hosted == column)
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            let _ = column;
            false
        }
    }

    /// Plans a test's host for `cell` ([`LiveGridState::set_test_cell_host`]):
    /// a widget host over the cell after its start. Returns true if planned.
    fn plan_test_host(&mut self, cell: &HostCell<'_, D::Row>) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        if let Some((column, build)) = &self.test_host
            && *column == cell.column.ix()
        {
            let bounds = cell.host_bounds();
            let element = build(CellHost::host_div(HostLayer::Widget, cell.column.id()));
            self.hosts.push(CellHost {
                layer: HostLayer::Widget,
                column: *column,
                zone: cell.zone,
                bounds,
                element,
            });
            return true;
        }
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = cell;
        false
    }
}

/// The vertical grid lines of one frame: one quad per line, in the regions
/// of a band. The zones' dividers own their outer edges, so the column there
/// draws none (Design Guides, "Hairlines belong on the boundary owner"); the
/// right zone is resized from the left, so its columns' lines are on the
/// left.
struct ColumnLines<'a, C> {
    columns: &'a ColumnLayout<C>,
    geometry: &'a ZoneGeometry,
    /// The body's left edge in window coordinates.
    body_x: Pixels,
    has_right_zone: bool,
    color: Hsla,
}

impl<C: GridColumns> ColumnLines<'_, C> {
    /// Adds the lines from `top`, `height` high, to the regions of a band.
    fn push(&self, regions: &mut [Region; 3], top: Pixels, height: Pixels) {
        for zone in ColumnZone::ALL {
            let frame = self.geometry.zone(zone);
            let columns = self.columns.zone_columns(zone);
            let region = &mut regions[zone.slot()];
            for i in frame.columns.clone() {
                let left = frame.column_x(columns, i);
                let right = left + columns.width(i);
                let x = match zone {
                    ColumnZone::FrozenLeft if i + 1 == columns.len() => continue,
                    ColumnZone::Scrolling if self.has_right_zone && right >= frame.right() => {
                        continue;
                    }
                    ColumnZone::FrozenRight if i == 0 => continue,
                    ColumnZone::FrozenRight => left,
                    _ => right - px(1.),
                };
                region.lines.push((
                    Bounds::new(point(self.body_x + x, top), size(px(1.), height)),
                    self.color,
                ));
            }
        }
    }
}

/// The focus outline of a cell: a 2 px frame inside its bounds, four quads.
pub(super) fn focus_outline(cell: Bounds<Pixels>) -> [Bounds<Pixels>; 4] {
    const WIDTH: Pixels = px(2.);
    let (origin, extent) = (cell.origin, cell.size);
    let side = (extent.height - WIDTH * 2.).max(px(0.));
    [
        Bounds::new(origin, size(extent.width, WIDTH)),
        Bounds::new(
            point(origin.x, origin.y + extent.height - WIDTH),
            size(extent.width, WIDTH),
        ),
        Bounds::new(point(origin.x, origin.y + WIDTH), size(WIDTH, side)),
        Bounds::new(
            point(origin.x + extent.width - WIDTH, origin.y + WIDTH),
            size(WIDTH, side),
        ),
    ]
}

/// The selection fill of one region: the runs of selected cells of each row
/// (or the whole row), merged with the same runs of the rows above, so a
/// block of selected rows or cells is one quad, not a quad per cell.
#[derive(Default)]
pub(super) struct FillRuns {
    /// Runs still growing downwards: left edge, width, top.
    open: Vec<(Pixels, Pixels, Pixels)>,
    /// Runs of the current row: left edge and width.
    row: Vec<(Pixels, Pixels)>,
}

impl FillRuns {
    /// Adds a selected cell (or row extent) of the current row, joining it
    /// to the run on its left.
    fn push(&mut self, x: Pixels, width: Pixels) {
        match self.row.last_mut() {
            Some((left, run)) if (*left + *run - x).abs() < px(0.5) => *run = x + width - *left,
            _ => self.row.push((x, width)),
        }
    }

    /// Ends the row at `y`: open runs it does not continue are closed into
    /// `fills`, and its new runs open.
    fn end_row(&mut self, y: Pixels, fills: &mut Vec<(Bounds<Pixels>, Hsla)>, color: Hsla) {
        let row = &mut self.row;
        self.open.retain(|&(x, width, top)| {
            if let Some(position) = row.iter().position(|run| *run == (x, width)) {
                row.swap_remove(position);
                true
            } else {
                fills.push((Bounds::new(point(x, top), size(width, y - top)), color));
                false
            }
        });
        self.open
            .extend(row.drain(..).map(|(x, width)| (x, width, y)));
    }

    /// Closes every open run at `y`, the bottom of the last row.
    fn finish(&mut self, y: Pixels, fills: &mut Vec<(Bounds<Pixels>, Hsla)>, color: Hsla) {
        for (x, width, top) in self.open.drain(..) {
            fills.push((Bounds::new(point(x, top), size(width, y - top)), color));
        }
    }
}

/// A column painted in this frame.
pub(super) struct PlannedColumn<C> {
    pub(super) column: C,
    /// Its place among the visible columns.
    pub(super) view: ViewColumnIx,
    pub(super) align: ColumnAlign,
    /// What its cells paint.
    pub(super) content: CellContent,
    /// Its title: what a screen reader reads as a hosted cell's description.
    pub(super) title: SharedString,
    /// How its images fit their cells, and their corners.
    pub(super) image_fit: ImageFit,
    pub(super) image_radius: Pixels,
    /// How it shows its numbers.
    pub(super) number_format: NumberFormat,
    /// The left edge in window coordinates.
    pub(super) x: Pixels,
    pub(super) width: Pixels,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features(list: &[(&str, u32)]) -> FontFeatures {
        FontFeatures(Arc::new(
            list.iter()
                .map(|(tag, value)| ((*tag).to_owned(), *value))
                .collect(),
        ))
    }

    #[test]
    fn test_place_cell_icon() {
        let icon = |x: f32, width: f32, align: ColumnAlign| CellIcon {
            path: SharedString::new_static("icons/check.svg"),
            x: px(x),
            width: px(width),
            y: px(100.),
            align,
            color: Hsla::default(),
        };
        let place = |icon: CellIcon, row_height: f32| {
            let mut region = Region::new(Bounds::default());
            let placed = place_cell_icon(icon, px(row_height), &mut region);
            (placed, region.icons.first().map(|icon| icon.bounds))
        };
        let bounds = |x: f32, y: f32, side: f32| {
            Some(Bounds::new(point(px(x), px(y)), size(px(side), px(side))))
        };
        // 14 px up to 30 px rows, 16 px in taller ones, centred in the row.
        assert_eq!(
            place(icon(0., 80., ColumnAlign::Auto), 26.),
            (1, bounds(33., 106., 14.))
        );
        assert_eq!(
            place(icon(0., 80., ColumnAlign::Auto), 32.),
            (1, bounds(32., 108., 16.))
        );
        assert_eq!(
            place(icon(0., 80., ColumnAlign::Start), 40.),
            (1, bounds(6., 112., 16.))
        );
        assert_eq!(
            place(icon(0., 80., ColumnAlign::End), 30.),
            (1, bounds(60., 108., 14.))
        );
        // No room for the padding: centred in the cell; no room at all: none.
        assert_eq!(
            place(icon(10., 20., ColumnAlign::Start), 26.),
            (1, bounds(13., 106., 14.))
        );
        assert_eq!(place(icon(10., 12., ColumnAlign::Auto), 26.), (0, None));
    }

    #[test]
    fn test_tabular_figures_keep_inherited_features() {
        assert_eq!(
            with_tabular_figures(features(&[("zero", 1), ("ss01", 1)])),
            features(&[("zero", 1), ("ss01", 1), ("tnum", 1)]),
        );
        assert_eq!(
            with_tabular_figures(FontFeatures::default()),
            features(&[("tnum", 1)])
        );
        // The application's own `tnum` wins, even when it turns it off.
        assert_eq!(
            with_tabular_figures(features(&[("tnum", 0), ("zero", 1)])),
            features(&[("tnum", 0), ("zero", 1)]),
        );
    }
}
