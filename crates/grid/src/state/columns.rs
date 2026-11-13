//! The column schema and the column layout: one display order split in
//! three zones (frozen left, scrolling, frozen right), widths, visibility,
//! header bands, and the per-zone prefix sums that hit tests and horizontal
//! culling use.
//!
//! Everything that outlives a frame is keyed by [`ColumnIx`]; positions
//! ([`ViewColumnIx`], places in a zone) are computed, never stored.

use std::{ops::Range, sync::Arc};

use gpui_kit::component::IconNamed;
use gpui_kit::{App, Context, Pixels, Point, SharedString, Window, px};

use super::{
    CELL_PADDING, ColumnOf, LiveGridDelegate, LiveGridState, SelectionOrigin,
    disclosure::INDENT,
    events::{LayoutOrigin, LayoutPart, LayoutParts},
    frame::{LEAD_ICON_LANE, cell_icon_size, cell_image_size},
    header::{HEADER_SORT_LANE, header_icon_lane, header_label_style},
    images::ImageFit,
    pinned::{BodyLine, EDGES},
    style::AggregateLine,
    summary::{SummaryLine, SummaryMeasure, TOTALS_WEIGHT},
};
use crate::{
    actions::{
        AutofitColumn, AutofitColumns, FreezeColumnLeft, FreezeColumnRight, HideColumn,
        MoveColumnLeft, MoveColumnRight, ShowAllColumns, UnfreezeColumn,
    },
    engine::DisplayRow,
    model::{CellValue, ColumnIx, ColumnSet, GridColumns, GridRow, NumberFormat, display_text_in},
    policy::{AutofitContext, AutofitPolicy, ColumnMoveContext, ColumnMovePolicy, ColumnOrder},
};

/// The smallest width a column can have, whatever its own minimum.
pub const MIN_COLUMN_WIDTH: Pixels = px(10.);

/// Horizontal alignment of a column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnAlign {
    /// Numbers right, everything else left.
    #[default]
    Auto,
    /// Left in left-to-right layouts.
    Start,
    /// Right in left-to-right layouts.
    End,
}

/// Where a column is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColumnZone {
    /// Frozen at the left edge; ignores horizontal scrolling.
    FrozenLeft,
    /// Scrolls horizontally.
    #[default]
    Scrolling,
    /// Frozen at the right edge; ignores horizontal scrolling.
    FrozenRight,
}

impl ColumnZone {
    /// Every zone, left to right.
    pub(crate) const ALL: [Self; 3] = [Self::FrozenLeft, Self::Scrolling, Self::FrozenRight];

    /// The zone's place, left to right: an index into per-zone arrays.
    pub(crate) const fn slot(self) -> usize {
        self as usize
    }
}

/// The on-screen position of a visible column, left to right across all
/// zones. Transient: never stored and never persisted; the column itself (or
/// its [`ColumnIx`]) is the identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewColumnIx(u16);

impl ViewColumnIx {
    /// Creates a view position.
    pub const fn new(ix: u16) -> Self {
        Self(ix)
    }

    /// Returns the position as `usize`.
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// What a column paints in its cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CellContent {
    /// The value, painted by the body: text, an icon for
    /// [`CellValue::Icon`](crate::CellValue::Icon), or an image for
    /// [`CellValue::Image`](crate::CellValue::Image).
    #[default]
    Text,
    /// A real element from `LiveGridDelegate::render_cell` for each visible
    /// cell (progress, tag, sparkline). The cell stays a grid cell for the
    /// keyboard; presses reach the element and, unless it stops them, the
    /// cell.
    Element,
    /// An interactive element (checkbox, button): as `Element`, and Space
    /// on the focused cell activates it through the handler it registers
    /// (`CellElementContext::on_activate`). The keyboard focus stays with
    /// the grid: give the element `tab_stop(false)`.
    Control,
}

impl CellContent {
    /// The value of the root's [`CELL_CONTENT_KEY`] context attribute.
    ///
    /// [`CELL_CONTENT_KEY`]: crate::actions::CELL_CONTENT_KEY
    pub(crate) const fn key_value(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Element => "element",
            Self::Control => "control",
        }
    }
}

/// What a column's header cell shows. Wherever the header shows no text,
/// the title still names the column: its tooltip, its accessible name, the
/// column chooser, the menus and an export's header.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HeaderLabel {
    /// The title.
    #[default]
    Title,
    /// An icon (its asset path) before the title.
    IconAndTitle(SharedString),
    /// The icon (its asset path) alone.
    Icon(SharedString),
}

/// A column of the grid schema.
#[derive(Clone, Debug)]
pub struct GridColumn<C> {
    pub(super) column: C,
    pub(super) title: SharedString,
    pub(super) width: Pixels,
    pub(super) align: ColumnAlign,
    pub(super) min_width: Pixels,
    pub(super) max_width: Pixels,
    pub(super) zone: ColumnZone,
    pub(super) visible: bool,
    pub(super) resizable: bool,
    pub(super) movable: bool,
    pub(super) sortable: bool,
    pub(super) groupable: bool,
    // What the cells paint and what the header shows.
    pub(super) content: CellContent,
    pub(super) header_label: HeaderLabel,
    pub(super) hideable: bool,
    /// Never shown, offered or exported by default: a helper column.
    pub(super) internal: bool,
    /// How the column shows its numbers.
    pub(super) number_format: NumberFormat,
    // How an image fits its cell, and its corners.
    pub(super) image_fit: ImageFit,
    pub(super) image_corner_radius: Pixels,
}

impl<C: GridColumns> GridColumn<C> {
    /// A column with a title, 100 px wide.
    pub fn new(column: C, title: impl Into<SharedString>) -> Self {
        Self {
            column,
            title: title.into(),
            width: px(100.),
            align: ColumnAlign::Auto,
            min_width: MIN_COLUMN_WIDTH,
            max_width: Pixels::MAX,
            zone: ColumnZone::Scrolling,
            visible: true,
            resizable: true,
            movable: true,
            sortable: true,
            groupable: true,
            content: CellContent::Text,
            header_label: HeaderLabel::Title,
            hideable: true,
            internal: false,
            number_format: NumberFormat::new(),
            image_fit: ImageFit::Contain,
            image_corner_radius: px(0.),
        }
    }

    /// Sets the initial width, kept within the column's limits.
    pub fn with_width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    /// Sets the alignment.
    pub fn with_align(mut self, align: ColumnAlign) -> Self {
        self.align = align;
        self
    }

    /// Sets the smallest width; raised to [`MIN_COLUMN_WIDTH`].
    pub fn with_min_width(mut self, width: Pixels) -> Self {
        self.min_width = width.max(MIN_COLUMN_WIDTH);
        self
    }

    /// Sets the largest width (default: no limit).
    pub fn with_max_width(mut self, width: Pixels) -> Self {
        self.max_width = width;
        self
    }

    /// Sets the zone (default [`ColumnZone::Scrolling`]).
    pub fn with_zone(mut self, zone: ColumnZone) -> Self {
        self.zone = zone;
        self
    }

    /// Shows or hides the column initially (default shown).
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Lets the user resize the column with the mouse (default true).
    pub fn with_resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    /// Lets the user drag the column's header to reorder it (default true).
    pub fn with_movable(mut self, movable: bool) -> Self {
        self.movable = movable;
        self
    }

    /// Lets a click on the column's header sort by it (default true).
    /// Sorting through `LiveGridState::set_sort` is always allowed.
    pub fn with_sortable(mut self, sortable: bool) -> Self {
        self.sortable = sortable;
        self
    }

    /// Lets the user group by the column (default true): drag its header
    /// into the group panel ([`crate::LiveGrid::group_panel`]) or use
    /// "Group by this column". Grouping through
    /// `LiveGridState::set_group_by` is always allowed.
    pub fn with_groupable(mut self, groupable: bool) -> Self {
        self.groupable = groupable;
        self
    }

    /// Sets what the cells paint: the value as text (the default), or an
    /// element of `LiveGridDelegate::render_cell`.
    pub fn with_content(mut self, content: CellContent) -> Self {
        self.content = content;
        self
    }

    /// Shows `icon` in the header before the title.
    pub fn with_icon(mut self, icon: impl IconNamed) -> Self {
        self.header_label = HeaderLabel::IconAndTitle(icon.path());
        self
    }

    /// Shows only `icon` in the header. The title stays the column's name:
    /// its tooltip, its accessible name, and its label in the column
    /// chooser, the menus and an export's header.
    pub fn with_icon_only(mut self, icon: impl IconNamed) -> Self {
        self.header_label = HeaderLabel::Icon(icon.path());
        self
    }

    /// Sets whether the user may hide the column: "Hide column",
    /// `HideColumn` and the column chooser. Default `true`.
    /// `LiveGridState::set_column_visible` and a restored layout hide
    /// any column.
    pub fn with_hideable(mut self, hideable: bool) -> Self {
        self.hideable = hideable;
        self
    }

    /// Makes the column internal (default `false`): a helper the grid
    /// sorts, filters, groups and aggregates by, but never shows. It is
    /// hidden whatever `with_visible`, a restored layout or
    /// `LiveGridState::set_column_visible` say, and it is not in the column
    /// chooser, "Show all columns", a copy or an export of the visible
    /// columns. An export that names it (`ExportRequest::with_columns`)
    /// writes it.
    pub fn with_internal(mut self, internal: bool) -> Self {
        self.internal = internal;
        self
    }

    /// Sets how the column shows its numbers (default
    /// [`NumberFormat::new`], two decimals): its cells, its aggregates, a
    /// copy and an export's text and number hint.
    pub fn with_number_format(mut self, format: NumberFormat) -> Self {
        self.number_format = format;
        self
    }

    /// Sets how an image (`CellValue::Image`) fits the room of its cell.
    /// Default [`ImageFit::Contain`].
    pub fn with_image_fit(mut self, fit: ImageFit) -> Self {
        self.image_fit = fit;
        self
    }

    /// Rounds the corners of the images in the cells by `radius`: half the
    /// image's height draws a round avatar. Default 0.
    pub fn with_image_corner_radius(mut self, radius: Pixels) -> Self {
        self.image_corner_radius = radius;
        self
    }

    /// The column.
    pub fn column(&self) -> C {
        self.column
    }

    /// The title.
    pub fn title(&self) -> &SharedString {
        &self.title
    }

    /// The initial width.
    pub fn width(&self) -> Pixels {
        self.width
    }

    /// The smallest width.
    pub fn min_width(&self) -> Pixels {
        self.min_width
    }

    /// The largest width.
    pub fn max_width(&self) -> Pixels {
        self.max_width
    }

    /// The alignment.
    pub fn align(&self) -> ColumnAlign {
        self.align
    }

    /// The initial zone.
    pub fn zone(&self) -> ColumnZone {
        self.zone
    }

    /// Returns true if the column is shown initially.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Returns true if the user can resize the column.
    pub fn is_resizable(&self) -> bool {
        self.resizable
    }

    /// Returns true if the user can drag the column's header.
    pub fn is_movable(&self) -> bool {
        self.movable
    }

    /// Returns true if a click on the column's header sorts by it.
    pub fn is_sortable(&self) -> bool {
        self.sortable
    }

    /// Returns true if the user can group by the column.
    pub fn is_groupable(&self) -> bool {
        self.groupable
    }

    /// What the cells paint.
    pub fn content(&self) -> CellContent {
        self.content
    }

    /// What the header shows: the title, an icon and the title, or the
    /// icon alone.
    pub fn header_label(&self) -> &HeaderLabel {
        &self.header_label
    }

    /// The header's icon (its asset path), if it shows one.
    pub fn icon(&self) -> Option<&SharedString> {
        match &self.header_label {
            HeaderLabel::Title => None,
            HeaderLabel::IconAndTitle(icon) | HeaderLabel::Icon(icon) => Some(icon),
        }
    }

    /// Returns true if the user may hide the column.
    pub fn is_hideable(&self) -> bool {
        self.hideable
    }

    /// How the column shows its numbers.
    pub fn number_format(&self) -> NumberFormat {
        self.number_format
    }

    /// Returns true if the column is internal: never shown.
    pub fn is_internal(&self) -> bool {
        self.internal
    }

    /// How an image fits the room of its cell.
    pub fn image_fit(&self) -> ImageFit {
        self.image_fit
    }

    /// The corner radius of the images in the cells.
    pub fn image_corner_radius(&self) -> Pixels {
        self.image_corner_radius
    }

    /// `width` within `max(MIN_COLUMN_WIDTH, min_width) ..= max_width`; NaN,
    /// infinite, zero and negative widths give the minimum.
    pub(crate) fn clamp_width(&self, width: Pixels) -> Pixels {
        let min = self.min_width.max(MIN_COLUMN_WIDTH);
        if !f32::from(width).is_finite() || width <= px(0.) {
            return min;
        }
        width.min(self.max_width).max(min)
    }
}

/// A titled header band over columns and nested bands (a multi-level
/// header).
#[derive(Clone, Debug)]
pub struct ColumnBand<C> {
    id: &'static str,
    title: SharedString,
    children: Vec<BandChild<C>>,
}

#[derive(Clone, Debug)]
enum BandChild<C> {
    Column(GridColumn<C>),
    Band(ColumnBand<C>),
}

impl<C: GridColumns> ColumnBand<C> {
    /// A band; `id` names it for element identity and must be unique within
    /// the schema.
    pub fn new(id: &'static str, title: impl Into<SharedString>) -> Self {
        Self {
            id,
            title: title.into(),
            children: Vec::new(),
        }
    }

    /// Appends a column under the band.
    pub fn column(mut self, column: GridColumn<C>) -> Self {
        self.children.push(BandChild::Column(column));
        self
    }

    /// Appends a nested band.
    pub fn band(mut self, band: ColumnBand<C>) -> Self {
        self.children.push(BandChild::Band(band));
        self
    }

    /// The id.
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// The title.
    pub fn title(&self) -> &SharedString {
        &self.title
    }
}

/// The columns shown by a grid, in their initial display order, with their
/// header bands.
#[derive(Clone, Debug)]
pub struct GridSchema<C> {
    /// Declaration order: depth first through bands.
    columns: Vec<GridColumn<C>>,
    /// The bands of `columns[i]`, outermost first.
    chains: Vec<Box<[&'static str]>>,
    /// Every band's id and title.
    bands: Vec<(&'static str, SharedString)>,
    frozen: usize,
}

impl<C: GridColumns> Default for GridSchema<C> {
    fn default() -> Self {
        Self {
            columns: Vec::new(),
            chains: Vec::new(),
            bands: Vec::new(),
            frozen: 0,
        }
    }
}

impl<C: GridColumns> GridSchema<C> {
    /// An empty schema.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a column.
    ///
    /// # Panics
    ///
    /// If the column, or another column with the same [`GridColumns::id`],
    /// is already in the schema.
    pub fn column(mut self, column: GridColumn<C>) -> Self {
        self.push_column(column, &[]);
        self
    }

    /// Appends a band and its columns, depth first.
    ///
    /// # Panics
    ///
    /// If a band id or a column repeats one already in the schema.
    pub fn band(mut self, band: ColumnBand<C>) -> Self {
        self.push_band(band, &mut Vec::new());
        self
    }

    /// Freezes the first `count` declared columns on the left, over their
    /// own [`GridColumn::with_zone`].
    pub fn with_frozen(mut self, count: usize) -> Self {
        self.frozen = count;
        self
    }

    fn push_column(&mut self, column: GridColumn<C>, chain: &[&'static str]) {
        let ix = column.column.ix();
        assert!(ix.get() < 128, "a grid supports at most 128 columns");
        assert!(
            !self
                .columns
                .iter()
                .any(|c| c.column == column.column || c.column.id() == column.column.id()),
            "column {:?} (id {:?}) is already in the schema",
            column.column,
            column.column.id(),
        );
        self.columns.push(column);
        self.chains.push(chain.into());
    }

    fn push_band(&mut self, band: ColumnBand<C>, chain: &mut Vec<&'static str>) {
        assert!(
            !self.bands.iter().any(|(id, _)| *id == band.id),
            "band {:?} is already in the schema",
            band.id,
        );
        self.bands.push((band.id, band.title));
        chain.push(band.id);
        for child in band.children {
            match child {
                BandChild::Column(column) => self.push_column(column, chain),
                BandChild::Band(band) => self.push_band(band, chain),
            }
        }
        chain.pop();
    }
}

/// The visible columns of one zone, left to right, with prefix sums.
#[derive(Clone, Debug, Default)]
pub(crate) struct ZoneColumns {
    pub(crate) columns: Vec<ColumnIx>,
    /// `offsets[i]` is the left edge of `columns[i]` from the zone's left
    /// edge; `offsets[n]` is the zone's content width.
    pub(crate) offsets: Vec<Pixels>,
}

impl ZoneColumns {
    pub(crate) fn len(&self) -> usize {
        self.columns.len()
    }

    /// The zone's content width: the sum of its visible columns.
    pub(crate) fn content_width(&self) -> Pixels {
        self.offsets.last().copied().unwrap_or_default()
    }

    /// The width of `columns[i]`.
    pub(crate) fn width(&self, i: usize) -> Pixels {
        self.offsets[i + 1] - self.offsets[i]
    }

    /// Columns intersecting `left..right` of the zone content: a binary
    /// search over the prefix sums.
    pub(crate) fn range(&self, left: Pixels, right: Pixels) -> Range<usize> {
        let n = self.len();
        if n == 0 || right <= left {
            return 0..0;
        }
        let start = self.offsets[1..].partition_point(|&end| end <= left);
        let end = self.offsets[..n].partition_point(|&start| start < right);
        start..end.max(start)
    }

    /// The column containing `x` of the zone content.
    pub(crate) fn index_at(&self, x: Pixels) -> Option<usize> {
        let ix = self.offsets.partition_point(|&o| o <= x);
        (ix >= 1 && ix <= self.len()).then(|| ix - 1)
    }
}

/// A run of consecutive visible columns of one band in one zone: one band
/// cell of the header.
#[derive(Clone, Debug)]
pub(crate) struct BandRun {
    pub(crate) band: &'static str,
    pub(crate) title: SharedString,
    pub(crate) zone: ColumnZone,
    /// The header row of the band cell (0 is the top row).
    pub(crate) level: usize,
    /// The columns under the run: a range of the zone's [`ZoneColumns`].
    pub(crate) columns: Range<usize>,
    /// Every column under the run can be dragged.
    pub(crate) movable: bool,
}

/// One zone as laid out in a frame: its geometry.
#[derive(Clone, Debug)]
pub(crate) struct ZoneFrame {
    pub(crate) zone: ColumnZone,
    /// The zone's left edge from the body's left edge.
    pub(crate) x: Pixels,
    /// The zone's on-screen width.
    pub(crate) width: Pixels,
    /// The horizontal scroll of the zone's content (zero for frozen zones).
    pub(crate) scroll: Pixels,
    /// The zone's visible columns that intersect it: a range of its
    /// [`ZoneColumns`].
    pub(crate) columns: Range<usize>,
}

impl ZoneFrame {
    /// The left edge of `zone.columns[i]` from the body's left edge.
    pub(crate) fn column_x(&self, zone: &ZoneColumns, i: usize) -> Pixels {
        self.x + zone.offsets[i] - self.scroll
    }

    /// The zone's right edge from the body's left edge.
    pub(crate) fn right(&self) -> Pixels {
        self.x + self.width
    }
}

/// The three zones as laid out in a frame, left to right.
#[derive(Clone, Debug)]
pub(crate) struct ZoneGeometry {
    pub(crate) zones: [ZoneFrame; 3],
}

impl ZoneGeometry {
    pub(crate) fn zone(&self, zone: ColumnZone) -> &ZoneFrame {
        &self.zones[zone.slot()]
    }

    /// The zone under `x` (from the body's left edge). The right zone does
    /// not scroll, so it is found without the scroll offset.
    pub(crate) fn zone_at(&self, x: Pixels) -> Option<ColumnZone> {
        let [left, scrolling, right] = &self.zones;
        if right.width > px(0.) && x >= right.x && x < right.right() {
            Some(ColumnZone::FrozenRight)
        } else if left.width > px(0.) && x >= left.x && x < left.right() {
            Some(ColumnZone::FrozenLeft)
        } else if scrolling.width > px(0.) && x >= scrolling.x && x < scrolling.right() {
            Some(ColumnZone::Scrolling)
        } else {
            None
        }
    }
}

/// How a visible-column boundary resolves for a block being moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Boundary {
    /// Insert the block at this gap of the zone order.
    Gap(usize),
    /// The block is already there.
    Here,
    /// The move policy refuses every gap at this boundary.
    Refused,
}

/// A block of one zone being moved: its members and where they are.
pub(crate) struct ZoneMove<'a> {
    context: ColumnMoveContext<'a>,
    members: Vec<ColumnIx>,
    /// From the first to the last member, in the zone order.
    span: Range<usize>,
    contiguous: bool,
    /// The zone-order position of every visible column of the zone.
    visible_at: Vec<usize>,
}

impl ZoneMove<'_> {
    /// The policy's verdict on putting the block at visible boundary `k`
    /// (before the zone's `k`-th visible column, or after the last one).
    /// Gaps across hidden columns are tried nearest to the visible column
    /// first: the block lands right before it, or right after the last one.
    pub(crate) fn resolve(&self, k: usize, policy: &dyn ColumnMovePolicy) -> Boundary {
        let n = self.visible_at.len();
        let check = |gap: usize| {
            if self.contiguous && self.span.start <= gap && gap <= self.span.end {
                Some(Boundary::Here)
            } else if policy.is_allowed(&self.context, self.span.clone(), gap) {
                Some(Boundary::Gap(gap))
            } else {
                None
            }
        };
        let found = if k < n {
            let low = if k > 0 { self.visible_at[k - 1] + 1 } else { 0 };
            (low..=self.visible_at[k]).rev().find_map(check)
        } else if k == n {
            let low = n.checked_sub(1).map_or(0, |last| self.visible_at[last] + 1);
            (low..=self.context.columns().len()).find_map(check)
        } else {
            None
        };
        found.unwrap_or(Boundary::Refused)
    }
}

/// The column layout of a grid: the schema plus the user's order, widths,
/// visibility and zones, and what is derived from them. Cloned for a dry run
/// of a move (the column chooser's drop targets).
#[derive(Clone)]
pub(crate) struct ColumnLayout<C> {
    /// Schema columns by `ColumnIx`; `None` for columns of `C` not in it.
    schema: Vec<Option<GridColumn<C>>>,
    /// Schema columns in declaration order, with the zone each starts in.
    declared: Vec<(ColumnIx, ColumnZone)>,
    /// The bands of each column, outermost first, by `ColumnIx`.
    chains: Vec<Box<[&'static str]>>,
    bands: Vec<(&'static str, SharedString)>,
    /// The deepest band nesting: the header has `depth + 1` rows.
    depth: usize,
    /// Every schema column in display order, sorted by zone.
    order: Vec<ColumnIx>,
    /// `order[ends[z - 1]..ends[z]]` is zone `z`.
    ends: [usize; 3],
    widths: Vec<Pixels>,
    zones: Vec<ColumnZone>,
    visible: ColumnSet,
    /// The internal columns: never visible.
    internal: ColumnSet,
    // Derived by `settle`.
    zone_columns: [ZoneColumns; 3],
    /// The first `ViewColumnIx` of each zone.
    zone_starts: [usize; 3],
    view_ix: Vec<u16>,
    runs: Vec<BandRun>,
}

const NO_VIEW_IX: u16 = u16::MAX;

impl<C: GridColumns> ColumnLayout<C> {
    pub(crate) fn new(schema: GridSchema<C>) -> Self {
        let count = C::all().len();
        let mut columns: Vec<Option<GridColumn<C>>> = (0..count).map(|_| None).collect();
        let mut chains: Vec<Box<[&'static str]>> = vec![Box::default(); count];
        let mut widths = vec![px(0.); count];
        let mut zones = vec![ColumnZone::Scrolling; count];
        let mut visible = ColumnSet::EMPTY;
        let mut internal = ColumnSet::EMPTY;
        let mut declared = Vec::with_capacity(schema.columns.len());
        let depth = schema.chains.iter().map(|chain| chain.len()).max();
        for (position, (column, chain)) in schema.columns.into_iter().zip(schema.chains).enumerate()
        {
            let ix = column.column.ix();
            let zone = if position < schema.frozen {
                ColumnZone::FrozenLeft
            } else {
                column.zone
            };
            widths[ix.get()] = column.clamp_width(column.width);
            zones[ix.get()] = zone;
            if column.internal {
                internal.insert(ix);
            } else if column.visible {
                visible.insert(ix);
            }
            chains[ix.get()] = chain;
            declared.push((ix, zone));
            columns[ix.get()] = Some(column);
        }
        let mut layout = Self {
            schema: columns,
            declared,
            chains,
            bands: schema.bands,
            depth: depth.unwrap_or(0),
            order: Vec::new(),
            ends: [0; 3],
            widths,
            zones,
            visible,
            internal,
            zone_columns: Default::default(),
            zone_starts: [0; 3],
            view_ix: vec![NO_VIEW_IX; count],
            runs: Vec::new(),
        };
        layout.reset_order();
        layout
    }

    /// The order and zones of the schema.
    fn reset_order(&mut self) {
        self.order.clear();
        for &(ix, zone) in &self.declared {
            self.order.push(ix);
            self.zones[ix.get()] = zone;
        }
        self.settle();
        // At least one column is always shown.
        if self.visible.is_empty()
            && let Some(&first) = self.order.iter().find(|ix| !self.internal.contains(**ix))
        {
            self.visible.insert(first);
            self.settle();
        }
    }

    /// Resets the order and zones to the schema, normalized by `policy`.
    pub(crate) fn reset(&mut self, policy: &dyn ColumnMovePolicy) {
        self.reset_order();
        self.normalize(policy);
    }

    /// Restores `policy`'s invariants; a restored layout calls it too.
    pub(crate) fn normalize(&mut self, policy: &dyn ColumnMovePolicy) {
        policy.normalize(&mut ColumnOrder::new(
            &mut self.order,
            &mut self.zones,
            &self.chains,
        ));
        self.settle();
    }

    /// Sorts the order by zone and derives the zone columns, prefix sums,
    /// view positions and band runs.
    fn settle(&mut self) {
        let zones = &self.zones;
        self.order.sort_by_key(|ix| zones[ix.get()].slot());
        for zone in ColumnZone::ALL {
            self.ends[zone.slot()] = self
                .order
                .partition_point(|ix| zones[ix.get()].slot() <= zone.slot());
        }
        self.view_ix.fill(NO_VIEW_IX);
        let mut view = 0;
        for zone in ColumnZone::ALL {
            let range = self.zone_range(zone);
            let columns = &mut self.zone_columns[zone.slot()];
            columns.columns.clear();
            columns.offsets.clear();
            self.zone_starts[zone.slot()] = view;
            let mut x = px(0.);
            columns.offsets.push(x);
            for &ix in &self.order[range] {
                if self.visible.contains(ix) {
                    columns.columns.push(ix);
                    x += self.widths[ix.get()];
                    columns.offsets.push(x);
                    self.view_ix[ix.get()] = view as u16;
                    view += 1;
                }
            }
        }
        self.runs.clear();
        for zone in ColumnZone::ALL {
            let columns = &self.zone_columns[zone.slot()];
            for level in 0..self.depth {
                let mut i = 0;
                while i < columns.len() {
                    let Some(&band) = self.chains[columns.columns[i].get()].get(level) else {
                        i += 1;
                        continue;
                    };
                    let start = i;
                    while i < columns.len()
                        && self.chains[columns.columns[i].get()].get(level) == Some(&band)
                    {
                        i += 1;
                    }
                    let title = self
                        .bands
                        .iter()
                        .find(|(id, _)| *id == band)
                        .map(|(_, title)| title.clone())
                        .unwrap_or_default();
                    let movable = columns.columns[start..i]
                        .iter()
                        .all(|ix| self.column(*ix).movable);
                    self.runs.push(BandRun {
                        band,
                        title,
                        zone,
                        level,
                        columns: start..i,
                        movable,
                    });
                }
            }
        }
    }

    fn zone_range(&self, zone: ColumnZone) -> Range<usize> {
        let start = match zone.slot() {
            0 => 0,
            slot => self.ends[slot - 1],
        };
        start..self.ends[zone.slot()]
    }

    /// The schema column `ix`, if it is in the schema.
    pub(crate) fn get(&self, ix: ColumnIx) -> Option<&GridColumn<C>> {
        self.schema.get(ix.get()).and_then(Option::as_ref)
    }

    /// The schema column `ix`. Panics if it is not in the schema.
    pub(crate) fn column(&self, ix: ColumnIx) -> &GridColumn<C> {
        self.get(ix).expect("the column is in the schema")
    }

    /// Every schema column in display order, hidden ones included.
    pub(crate) fn order(&self) -> &[ColumnIx] {
        &self.order
    }

    /// The display order of one zone, hidden columns included.
    pub(crate) fn zone_order(&self, zone: ColumnZone) -> &[ColumnIx] {
        &self.order[self.zone_range(zone)]
    }

    pub(crate) fn width(&self, ix: ColumnIx) -> Pixels {
        self.widths.get(ix.get()).copied().unwrap_or_default()
    }

    pub(crate) fn is_visible(&self, ix: ColumnIx) -> bool {
        self.visible.contains(ix)
    }

    pub(crate) fn zone_of(&self, ix: ColumnIx) -> ColumnZone {
        self.zones.get(ix.get()).copied().unwrap_or_default()
    }

    /// The visible columns (the quick filter's default columns).
    pub(crate) fn visible_set(&self) -> ColumnSet {
        self.visible
    }

    /// How the column `ix` shows its numbers; the default for a column not
    /// in the schema.
    #[inline]
    pub(crate) fn number_format(&self, ix: ColumnIx) -> NumberFormat {
        self.schema
            .get(ix.get())
            .and_then(Option::as_ref)
            .map_or_else(NumberFormat::new, |column| column.number_format)
    }

    /// The number formats of every column, by `ColumnIx`: what an export
    /// takes to its thread.
    pub(crate) fn number_formats(&self) -> Arc<[NumberFormat]> {
        (0..self.schema.len())
            .map(|ix| self.number_format(ColumnIx::new(ix as u16)))
            .collect()
    }

    /// Returns true if `ix` is an internal column, never shown.
    pub(crate) fn is_internal(&self, ix: ColumnIx) -> bool {
        self.internal.contains(ix)
    }

    /// Returns true if a column the user may show is hidden: what "Show
    /// all columns" would show.
    pub(crate) fn any_hidden(&self) -> bool {
        self.order
            .iter()
            .any(|ix| !self.visible.contains(*ix) && !self.internal.contains(*ix))
    }

    /// Visible columns in display order, left zone to right zone.
    pub(crate) fn visible(&self) -> impl Iterator<Item = ColumnIx> + '_ {
        self.zone_columns
            .iter()
            .flat_map(|zone| zone.columns.iter().copied())
    }

    pub(crate) fn view_column_ix(&self, ix: ColumnIx) -> Option<ViewColumnIx> {
        let view = *self.view_ix.get(ix.get())?;
        (view != NO_VIEW_IX).then_some(ViewColumnIx(view))
    }

    pub(crate) fn column_at(&self, view: ViewColumnIx) -> Option<ColumnIx> {
        let mut view = view.get();
        for zone in &self.zone_columns {
            if view < zone.len() {
                return Some(zone.columns[view]);
            }
            view -= zone.len();
        }
        None
    }

    /// The zone of a visible column and its index in the zone's visible
    /// columns.
    pub(crate) fn zone_position(&self, ix: ColumnIx) -> Option<(ColumnZone, usize)> {
        let view = self.view_column_ix(ix)?.get();
        let zone = self.zone_of(ix);
        Some((zone, view - self.zone_starts[zone.slot()]))
    }

    pub(crate) fn zone_columns(&self, zone: ColumnZone) -> &ZoneColumns {
        &self.zone_columns[zone.slot()]
    }

    /// Header band cells, zone by zone.
    pub(crate) fn runs(&self) -> &[BandRun] {
        &self.runs
    }

    /// The deepest band nesting: the header has `depth + 1` rows.
    pub(crate) fn depth(&self) -> usize {
        self.depth
    }

    /// The bands of `ix`, outermost first.
    pub(crate) fn bands_of(&self, ix: ColumnIx) -> &[&'static str] {
        self.chains.get(ix.get()).map_or(&[], |chain| chain)
    }

    /// The title of the band `id`, shown or not (an export's band titles,
    /// the column chooser).
    pub(crate) fn band_title(&self, id: &str) -> Option<&SharedString> {
        self.bands
            .iter()
            .find(|(band, _)| *band == id)
            .map(|(_, title)| title)
    }

    /// The total width of the visible columns.
    pub(crate) fn total_width(&self) -> Pixels {
        self.zone_columns
            .iter()
            .map(ZoneColumns::content_width)
            .fold(px(0.), |total, width| total + width)
    }

    /// The zones laid out in a body `viewport_width` wide: the left zone at
    /// the left edge, the right zone at the right edge, the scrolling zone
    /// between them. Frozen zones wider than the body are cut by its edge;
    /// the left one first gets `min(left, viewport)`.
    pub(crate) fn geometry(&self, viewport_width: Pixels, scroll_x: Pixels) -> ZoneGeometry {
        let viewport = viewport_width.max(px(0.));
        let [left, scrolling, right] = &self.zone_columns;
        let left_width = left.content_width().min(viewport);
        let right_width = right.content_width().min(viewport - left_width);
        let scroll_width = (viewport - left_width - right_width).max(px(0.));
        ZoneGeometry {
            zones: [
                ZoneFrame {
                    zone: ColumnZone::FrozenLeft,
                    x: px(0.),
                    width: left_width,
                    scroll: px(0.),
                    columns: left.range(px(0.), left_width),
                },
                ZoneFrame {
                    zone: ColumnZone::Scrolling,
                    x: left_width,
                    width: scroll_width,
                    scroll: scroll_x,
                    columns: scrolling.range(scroll_x, scroll_x + scroll_width),
                },
                ZoneFrame {
                    zone: ColumnZone::FrozenRight,
                    x: viewport - right_width,
                    width: right_width,
                    scroll: px(0.),
                    columns: right.range(px(0.), right_width),
                },
            ],
        }
    }

    /// The column at `x` from the body's left edge: right zone without the
    /// scroll offset, then left zone, then the scrolling zone with it.
    pub(crate) fn column_at_x(
        &self,
        x: Pixels,
        viewport_width: Pixels,
        scroll_x: Pixels,
    ) -> Option<ColumnIx> {
        let geometry = self.geometry(viewport_width, scroll_x);
        let zone = geometry.zone_at(x)?;
        let frame = geometry.zone(zone);
        let columns = self.zone_columns(zone);
        columns
            .index_at(x - frame.x + frame.scroll)
            .map(|i| columns.columns[i])
    }

    pub(crate) fn move_context(&self, zone: ColumnZone) -> ColumnMoveContext<'_> {
        ColumnMoveContext::new(zone, self.zone_order(zone), self.visible, &self.chains)
    }

    /// A move of `members` (columns of `zone`, in zone order).
    pub(crate) fn zone_move(&self, zone: ColumnZone, members: Vec<ColumnIx>) -> ZoneMove<'_> {
        let order = self.zone_order(zone);
        let positions: Vec<usize> = members
            .iter()
            .filter_map(|member| order.iter().position(|ix| ix == member))
            .collect();
        let span = match (positions.iter().min(), positions.iter().max()) {
            (Some(&first), Some(&last)) => first..last + 1,
            _ => 0..0,
        };
        let visible_at = order
            .iter()
            .enumerate()
            .filter(|(_, ix)| self.visible.contains(**ix))
            .map(|(position, _)| position)
            .collect();
        ZoneMove {
            context: self.move_context(zone),
            contiguous: span.len() == members.len(),
            members,
            span,
            visible_at,
        }
    }

    /// The columns of `band` in `zone` that lie in one unbroken run of the
    /// zone order with `first`, hidden ones included: what a drag of the
    /// band's header cell moves.
    pub(crate) fn band_segment(
        &self,
        zone: ColumnZone,
        band: &'static str,
        first: ColumnIx,
    ) -> Vec<ColumnIx> {
        let order = self.zone_order(zone);
        let is_member = |ix: &ColumnIx| self.bands_of(*ix).contains(&band);
        let Some(position) = order.iter().position(|ix| *ix == first) else {
            return Vec::new();
        };
        if !is_member(&first) {
            return Vec::new();
        }
        let start = order[..position]
            .iter()
            .rposition(|ix| !is_member(ix))
            .map_or(0, |p| p + 1);
        let end = order[position..]
            .iter()
            .position(|ix| !is_member(ix))
            .map_or(order.len(), |p| position + p);
        order[start..end].to_vec()
    }

    /// Every column of `band` in `zone`, in order.
    pub(crate) fn band_members(&self, zone: ColumnZone, band: &'static str) -> Vec<ColumnIx> {
        self.zone_order(zone)
            .iter()
            .copied()
            .filter(|ix| self.bands_of(*ix).contains(&band))
            .collect()
    }

    /// Sets a width within the column's limits. Returns true if it changed.
    pub(crate) fn set_width(&mut self, ix: ColumnIx, width: Pixels) -> bool {
        let Some(column) = self.get(ix) else {
            return false;
        };
        let width = column.clamp_width(width);
        if self.widths[ix.get()] == width {
            return false;
        }
        self.widths[ix.get()] = width;
        self.settle();
        true
    }

    /// Shows or hides a column at its place. `None` if refused: the last
    /// visible column cannot be hidden, an internal one cannot be shown.
    /// Otherwise whether it changed.
    pub(crate) fn set_visible(&mut self, ix: ColumnIx, visible: bool) -> Option<bool> {
        self.get(ix)?;
        if visible && self.internal.contains(ix) {
            return None;
        }
        if self.visible.contains(ix) == visible {
            return Some(false);
        }
        if visible {
            self.visible.insert(ix);
        } else {
            if self.visible.len() == 1 {
                return None;
            }
            let visible = self.visible;
            self.visible = column_set(
                self.order
                    .iter()
                    .copied()
                    .filter(|c| *c != ix && visible.contains(*c)),
            );
        }
        self.settle();
        Some(true)
    }

    /// Shows every column but the internal ones. Returns the columns that
    /// were hidden.
    pub(crate) fn show_all(&mut self) -> ColumnSet {
        let mut shown = ColumnSet::EMPTY;
        for &ix in &self.order {
            if !self.visible.contains(ix) && !self.internal.contains(ix) {
                shown.insert(ix);
                self.visible.insert(ix);
            }
        }
        if !shown.is_empty() {
            self.settle();
        }
        shown
    }

    /// Moves `column` (with the columns `policy` moves along) to `zone`, at
    /// the edge of the new zone nearest to its old place: the end of a zone
    /// to the left, the start of a zone to the right. Returns the columns
    /// that changed zone.
    pub(crate) fn set_zone(
        &mut self,
        ix: ColumnIx,
        zone: ColumnZone,
        policy: &dyn ColumnMovePolicy,
    ) -> Vec<ColumnIx> {
        if self.get(ix).is_none() {
            return Vec::new();
        }
        let current = self.zone_of(ix);
        if current == zone {
            return Vec::new();
        }
        let order = self.zone_order(current);
        let position = order
            .iter()
            .position(|c| *c == ix)
            .expect("a schema column is in the order");
        let block = policy.zone_block(&self.move_context(current), ix);
        let block = if block.start <= position && position < block.end && block.end <= order.len() {
            block
        } else {
            position..position + 1
        };
        let members = order[block].to_vec();
        self.order.retain(|c| !members.contains(c));
        for member in &members {
            self.zones[member.get()] = zone;
        }
        let zones = &self.zones;
        let at = if zone.slot() < current.slot() {
            self.order
                .partition_point(|c| zones[c.get()].slot() <= zone.slot())
        } else {
            self.order
                .partition_point(|c| zones[c.get()].slot() < zone.slot())
        };
        self.order.splice(at..at, members.iter().copied());
        self.settle();
        members
    }

    /// Moves `members` (columns of `zone`, in zone order) to `gap` of the
    /// zone order, counted before they are removed. Returns true if the
    /// order changed.
    pub(crate) fn move_members(
        &mut self,
        zone: ColumnZone,
        members: &[ColumnIx],
        gap: usize,
    ) -> bool {
        let range = self.zone_range(zone);
        let order = &self.order[range.clone()];
        let gap = gap.min(order.len());
        let before = order[..gap].iter().filter(|c| members.contains(c)).count();
        let mut moved: Vec<ColumnIx> = order
            .iter()
            .copied()
            .filter(|c| !members.contains(c))
            .collect();
        let at = gap - before;
        moved.splice(at..at, members.iter().copied());
        if moved == order {
            return false;
        }
        self.order.splice(range, moved);
        self.settle();
        true
    }

    /// Moves `members` before `before` (`None`: to the end of the zone) if
    /// `policy` allows it. `None` if refused, else whether it changed.
    pub(crate) fn move_before(
        &mut self,
        zone: ColumnZone,
        members: Vec<ColumnIx>,
        before: Option<ColumnIx>,
        policy: &dyn ColumnMovePolicy,
    ) -> Option<bool> {
        if members.is_empty() || members.iter().any(|ix| self.zone_of(*ix) != zone) {
            return None;
        }
        let order = self.zone_order(zone);
        let gap = match before {
            Some(before) => order.iter().position(|c| *c == before)?,
            None => order.len(),
        };
        let shift = self.zone_move(zone, members);
        if shift.contiguous && shift.span.start <= gap && gap <= shift.span.end {
            return Some(false);
        }
        if !policy.is_allowed(&shift.context, shift.span.clone(), gap) {
            return None;
        }
        let members = shift.members;
        Some(self.move_members(zone, &members, gap))
    }
}

/// What a layout change changed, and what to scroll into view after it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LayoutDelta {
    pub(super) parts: LayoutParts,
    pub(super) columns: ColumnSet,
    /// Moved columns to scroll into view (order and zone changes).
    pub(super) reveal: ColumnSet,
}

impl LayoutDelta {
    pub(super) fn new(part: LayoutPart, columns: impl IntoIterator<Item = ColumnIx>) -> Self {
        let columns = column_set(columns);
        Self {
            parts: part.into(),
            columns,
            reveal: ColumnSet::EMPTY,
        }
    }

    pub(super) fn revealing(mut self) -> Self {
        self.reveal = self.columns;
        self
    }

    pub(super) fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub(super) fn union_with(&mut self, other: Self) {
        self.parts.union_with(other.parts);
        self.columns.union_with(other.columns);
        self.reveal.union_with(other.reveal);
    }
}

fn column_set(columns: impl IntoIterator<Item = ColumnIx>) -> ColumnSet {
    columns.into_iter().fold(ColumnSet::EMPTY, |mut set, ix| {
        set.insert(ix);
        set
    })
}

pub(super) fn column_of<C: GridColumns>(ix: ColumnIx) -> C {
    C::all()[ix.get()]
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets how columns and bands move (default [`BandedMove`]; see also
    /// [`FreeMove`]). The schema's order is normalized by the new policy.
    ///
    /// [`BandedMove`]: crate::BandedMove
    /// [`FreeMove`]: crate::FreeMove
    pub fn with_column_move_policy(mut self, policy: impl ColumnMovePolicy) -> Self {
        self.column_move = Box::new(policy);
        self.columns.reset(&*self.column_move);
        // A layout given to `with_layout` is resolved by the new policy.
        self.apply_initial_layout()
    }

    /// Sets how autofit measures a column (default [`VisibleRowsAutofit`]).
    ///
    /// [`VisibleRowsAutofit`]: crate::VisibleRowsAutofit
    pub fn with_autofit_policy(mut self, policy: impl AutofitPolicy) -> Self {
        self.autofit = Box::new(policy);
        self
    }

    /// Every schema column in display order (left, scrolling, right zone),
    /// hidden ones included at their place.
    pub fn columns(&self) -> impl Iterator<Item = ColumnOf<D>> + '_ {
        self.columns.order().iter().map(|ix| column_of(*ix))
    }

    /// Visible columns in display order; the n-th has `ViewColumnIx::new(n)`.
    pub fn visible_columns(&self) -> impl Iterator<Item = ColumnOf<D>> + '_ {
        self.columns.visible().map(column_of)
    }

    /// The width of `column` (zero if it is not in the schema).
    pub fn column_width(&self, column: ColumnOf<D>) -> Pixels {
        self.columns.width(column.ix())
    }

    /// Returns true if `column` is shown.
    pub fn is_column_visible(&self, column: ColumnOf<D>) -> bool {
        self.columns.is_visible(column.ix())
    }

    /// The zone of `column`.
    pub fn column_zone(&self, column: ColumnOf<D>) -> ColumnZone {
        self.columns.zone_of(column.ix())
    }

    /// The on-screen position of `column`; `None` if it is hidden.
    pub fn view_column_ix(&self, column: ColumnOf<D>) -> Option<ViewColumnIx> {
        self.columns.view_column_ix(column.ix())
    }

    /// The visible column at an on-screen position.
    pub fn column_at(&self, ix: ViewColumnIx) -> Option<ColumnOf<D>> {
        self.columns.column_at(ix).map(column_of)
    }

    /// The column that column commands (the column Actions) act on: the last
    /// header cell or band pressed, or the column the focus moved to,
    /// whichever came last. `None` if there is none or it is hidden.
    pub fn current_column(&self) -> Option<ColumnOf<D>> {
        self.current_column
            .filter(|ix| self.columns.is_visible(*ix))
            .map(column_of)
    }

    /// Writes the current column. The last writer wins: a header press, the
    /// focused cell, the header menu.
    pub(super) fn set_current_column(&mut self, column: ColumnIx) {
        self.current_column = Some(column);
    }

    /// Sets the width of `column`, kept within its limits. Emits one
    /// `LayoutChange` if it changed.
    pub fn set_column_width(&mut self, column: ColumnOf<D>, width: Pixels, cx: &mut Context<Self>) {
        self.change_layout(LayoutOrigin::Programmatic, cx, |layout, _| {
            if layout.set_width(column.ix(), width) {
                LayoutDelta::new(LayoutPart::Width, [column.ix()])
            } else {
                LayoutDelta::default()
            }
        });
    }

    /// Shows or hides `column`; a shown column returns to its place.
    /// Returns false if refused: the last visible column cannot be hidden.
    pub fn set_column_visible(
        &mut self,
        column: ColumnOf<D>,
        visible: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        self.set_column_visible_from(column.ix(), visible, LayoutOrigin::Programmatic, cx)
    }

    fn set_column_visible_from(
        &mut self,
        ix: ColumnIx,
        visible: bool,
        origin: LayoutOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut refused = false;
        self.change_layout(origin, cx, |layout, _| {
            match layout.set_visible(ix, visible) {
                Some(true) => LayoutDelta::new(LayoutPart::Visibility, [ix]),
                Some(false) => LayoutDelta::default(),
                None => {
                    refused = true;
                    LayoutDelta::default()
                }
            }
        });
        !refused
    }

    /// Shows every column, with one `LayoutChange`.
    pub fn show_all_columns(&mut self, cx: &mut Context<Self>) {
        self.show_all_columns_from(LayoutOrigin::Programmatic, cx);
    }

    pub(super) fn show_all_columns_from(&mut self, origin: LayoutOrigin, cx: &mut Context<Self>) {
        self.change_layout(origin, cx, |layout, _| {
            let shown = layout.show_all();
            if shown.is_empty() {
                LayoutDelta::default()
            } else {
                LayoutDelta {
                    parts: LayoutPart::Visibility.into(),
                    columns: shown,
                    reveal: ColumnSet::EMPTY,
                }
            }
        });
    }

    /// Moves `column` to `zone`, at the new zone's edge nearest to its old
    /// place. The [`ColumnMovePolicy`] decides what moves along (with
    /// `BandedMove`, its whole top-level band).
    pub fn set_column_zone(
        &mut self,
        column: ColumnOf<D>,
        zone: ColumnZone,
        cx: &mut Context<Self>,
    ) {
        self.set_column_zone_from(column.ix(), zone, LayoutOrigin::Programmatic, cx);
    }

    fn set_column_zone_from(
        &mut self,
        ix: ColumnIx,
        zone: ColumnZone,
        origin: LayoutOrigin,
        cx: &mut Context<Self>,
    ) {
        self.change_layout(origin, cx, |layout, policy| {
            let moved = layout.set_zone(ix, zone, policy);
            if moved.is_empty() {
                return LayoutDelta::default();
            }
            let mut delta = LayoutDelta::new(LayoutPart::Zone, moved);
            delta.parts = delta.parts.with(LayoutPart::Order);
            delta.revealing()
        });
    }

    /// Moves `column` before `before` (`None`: to the end of its zone).
    /// Returns false if the zones differ or the move policy refuses.
    pub fn move_column(
        &mut self,
        column: ColumnOf<D>,
        before: Option<ColumnOf<D>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let ix = column.ix();
        if self.columns.get(ix).is_none() {
            return false;
        }
        let zone = self.columns.zone_of(ix);
        self.move_before(zone, vec![ix], before.map(|c| c.ix()), cx)
    }

    /// Moves every column of `band` in its zone before `before` (`None`: to
    /// the end of the zone). Returns false if the band is unknown, the zones
    /// differ or the move policy refuses.
    pub fn move_band(
        &mut self,
        band: &'static str,
        before: Option<ColumnOf<D>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(&first) = self
            .columns
            .order()
            .iter()
            .find(|ix| self.columns.bands_of(**ix).contains(&band))
        else {
            return false;
        };
        let zone = self.columns.zone_of(first);
        let members = self.columns.band_members(zone, band);
        self.move_before(zone, members, before.map(|c| c.ix()), cx)
    }

    fn move_before(
        &mut self,
        zone: ColumnZone,
        members: Vec<ColumnIx>,
        before: Option<ColumnIx>,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut refused = false;
        self.change_layout(LayoutOrigin::Programmatic, cx, |layout, policy| {
            let moved = members.clone();
            match layout.move_before(zone, members, before, policy) {
                Some(true) => LayoutDelta::new(LayoutPart::Order, moved).revealing(),
                Some(false) => LayoutDelta::default(),
                None => {
                    refused = true;
                    LayoutDelta::default()
                }
            }
        });
        !refused
    }

    /// Fits the width of `column` to its header and the rows painted in the
    /// next painted frame; the `LayoutChange` comes with that frame.
    pub fn autofit_column(&mut self, column: ColumnOf<D>, cx: &mut Context<Self>) {
        self.request_autofit(column_set([column.ix()]), LayoutOrigin::Programmatic, cx);
    }

    /// Fits every visible column the same way, with one `LayoutChange`.
    pub fn autofit_columns(&mut self, cx: &mut Context<Self>) {
        self.request_autofit(self.columns.visible_set(), LayoutOrigin::Programmatic, cx);
    }

    pub(super) fn request_autofit(
        &mut self,
        columns: ColumnSet,
        origin: LayoutOrigin,
        cx: &mut Context<Self>,
    ) {
        let pending = self
            .pending_autofit
            .get_or_insert((ColumnSet::EMPTY, origin));
        pending.0.union_with(columns);
        pending.1 = origin;
        cx.notify();
    }

    /// Measures and applies a requested autofit at the start of a frame
    /// plan, over the lines this frame paints: data rows, pinned rows, and
    /// the aggregates of captions, footers and the totals row, whose
    /// `aggregate_style` runs here for their weight. Returns the change, to be
    /// emitted after the frame. While the first rows of a data set are still
    /// being projected, the request waits for the frame that paints them.
    pub(super) fn apply_pending_autofit(
        &mut self,
        needed_lines: usize,
        window: &Window,
        cx: &App,
    ) -> Option<(LayoutDelta, LayoutOrigin)> {
        let loading = self.projection.is_empty()
            && !self.snapshot.is_empty()
            && (self.rebuild_task.is_some() || self.pending != super::PendingWork::None);
        if loading {
            return None;
        }
        let (requested, origin) = self.pending_autofit.take()?;
        let projection = self.projection.clone();
        let snapshot = self.snapshot.clone();
        let start = self.scroll_row.min(projection.len());
        let mut end = start;
        let mut lines = 0;
        // Group captions and footers take a line each.
        let mut summaries = Vec::new();
        while lines < needed_lines && end < projection.len() {
            match projection.line(end) {
                Some(DisplayRow::Data { row, .. }) => {
                    lines += usize::from(snapshot.row(row).is_some());
                }
                Some(
                    line @ (DisplayRow::GroupCaption { group, .. }
                    | DisplayRow::GroupFooter { group, .. }),
                ) => {
                    lines += 1;
                    if let Some(group) = projection.groups().get(group) {
                        summaries.push(SummaryLine {
                            line: if matches!(line, DisplayRow::GroupFooter { .. }) {
                                AggregateLine::Footer(group)
                            } else {
                                AggregateLine::Caption(group)
                            },
                            aggregates: group.aggregates(),
                            weight: None,
                            label: None,
                        });
                    }
                }
                None => {}
            }
            end += 1;
        }

        // The pinned rows this frame paints, and the totals row with its
        // label in the first visible column, if it shows.
        let bands = self.body_bands();
        let mut pinned: Vec<Arc<D::Row>> = Vec::new();
        for edge in EDGES {
            for (line, _) in bands.visible_lines(edge) {
                match line {
                    BodyLine::Pinned(_, ix) => {
                        pinned.extend(snapshot.pinned_rows(edge).get(ix).cloned());
                    }
                    BodyLine::Totals(_) => summaries.push(SummaryLine {
                        line: AggregateLine::Totals(edge),
                        aggregates: projection.totals(),
                        weight: Some(TOTALS_WEIGHT),
                        label: self
                            .columns
                            .visible()
                            .next()
                            .map(|first| (self.summary_texts.total(), first)),
                    }),
                    BodyLine::Data(_) => {}
                }
            }
        }

        let (font, font_size) = header_label_style(window);
        let mut widths = Vec::new();
        let mut buffer = String::new();
        let mut arena = Vec::new();
        // An icon or an image is measured as itself, not its text; a tree
        // row's indent and chevron lane are part of its cell.
        let icon_width = cell_icon_size(self.row_height);
        let image_width = cell_image_size(self.row_height);
        let tree_column = projection.is_tree().then(|| self.tree_column()).flatten();
        let targets: Vec<ColumnIx> = self
            .columns
            .visible()
            .filter(|ix| requested.contains(*ix))
            .collect();
        for ix in targets {
            let column: ColumnOf<D> = column_of(ix);
            let schema_column = self.columns.column(ix);
            let title = schema_column.title.clone();
            // A sortable column keeps room for the sort indicator, so a
            // later sort does not truncate a fitted title.
            let lane = if schema_column.sortable {
                HEADER_SORT_LANE
            } else {
                px(0.)
            };
            // The header's icon, and its gap before the title; a header that
            // shows the icon alone fits the icon.
            let icon = header_icon_lane(&schema_column.header_label, self.row_height);
            let title_width = match schema_column.header_label {
                HeaderLabel::Icon(_) => px(0.),
                HeaderLabel::Title | HeaderLabel::IconAndTitle(_) => self
                    .header
                    .labels
                    .width(ix, &title, &font, font_size, window),
            };
            let header = icon + title_width + lane;
            let summary_width = SummaryMeasure {
                delegate: &self.delegate,
                app: cx,
                text_cache: &mut self.text_cache,
                buffer: &mut buffer,
                arena: &mut arena,
                format: self.columns.number_format(ix),
            }
            .width(&summaries, column, window);
            let text_cache = &mut self.text_cache;
            let format = self.columns.number_format(ix);
            let mut measure_text = |row: &D::Row| {
                let value = row.value(column);
                match value {
                    CellValue::Icon(_) => return icon_width,
                    CellValue::Image(_) => return image_width,
                    _ => {}
                }
                let text = display_text_in(value, format, &mut buffer);
                if text.is_empty() {
                    return px(0.);
                }
                arena.clear();
                text_cache
                    .regular()
                    .layout(text, window, &mut arena)
                    .width()
            };
            let pinned_width = pinned
                .iter()
                .map(|row| measure_text(row))
                .fold(px(0.), Pixels::max);
            let mut measure = |display_ix: usize| {
                let DisplayRow::Data { row, depth } = projection.line(display_ix)? else {
                    return None;
                };
                let row = snapshot.row(row)?;
                let indent = if tree_column == Some(ix) {
                    INDENT * f32::from(depth) + LEAD_ICON_LANE
                } else {
                    px(0.)
                };
                Some(measure_text(row) + indent)
            };
            let mut context =
                AutofitContext::new(header, start..end, projection.len(), &mut measure)
                    .with_pinned_width(pinned_width)
                    .with_summary_width(summary_width);
            let content = self.autofit.content_width(&mut context);
            widths.push((ix, content.ceil() + CELL_PADDING * 2. + px(1.)));
        }
        let delta = self.relayout(|layout, _| {
            let mut delta = LayoutDelta::default();
            for (ix, width) in widths {
                if layout.set_width(ix, width) {
                    delta.union_with(LayoutDelta::new(LayoutPart::Width, [ix]));
                }
            }
            delta
        });
        (!delta.is_empty()).then_some((delta, origin))
    }

    /// Applies a layout change with the scroll anchor kept, then emits one
    /// `LayoutChange` and repaints. Returns false if nothing changed.
    pub(super) fn change_layout(
        &mut self,
        origin: LayoutOrigin,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ColumnLayout<ColumnOf<D>>, &dyn ColumnMovePolicy) -> LayoutDelta,
    ) -> bool {
        let visible = self.columns.visible_set();
        let delta = self.relayout(change);
        if delta.is_empty() {
            return false;
        }
        self.emit_layout_change(delta, origin, cx);
        // After the event: a focus moved off a hidden column follows
        // its `LayoutChange`.
        if self.columns.visible_set() != visible {
            self.visible_columns_changed(cx);
        }
        cx.notify();
        true
    }

    /// Applies a layout change keeping the first visible scrolling column at
    /// its place on screen, then scrolls moved columns into view. Does not
    /// notify: frame planning calls it.
    pub(super) fn relayout(
        &mut self,
        change: impl FnOnce(&mut ColumnLayout<ColumnOf<D>>, &dyn ColumnMovePolicy) -> LayoutDelta,
    ) -> LayoutDelta {
        let anchor = self.scroll_anchor();
        let delta = change(&mut self.columns, &*self.column_move);
        if delta.is_empty() {
            return delta;
        }
        self.restore_scroll_anchor(anchor);
        if !delta.reveal.is_empty() {
            self.reveal_columns(delta.reveal);
        }
        self.clamp_scroll();
        delta
    }

    /// Emits a `LayoutChange`. A focus on a column it hid then moves to the
    /// next visible column, so `FocusChange` follows with the matching origin.
    pub(super) fn emit_layout_change(
        &mut self,
        delta: LayoutDelta,
        origin: LayoutOrigin,
        cx: &mut Context<Self>,
    ) {
        cx.emit(super::LiveGridEvent::LayoutChange {
            parts: delta.parts,
            columns: delta.columns,
            origin,
        });
        let origin = match origin {
            LayoutOrigin::User => SelectionOrigin::User,
            LayoutOrigin::Programmatic => SelectionOrigin::Programmatic,
        };
        self.move_focus_off_hidden(origin, cx);
    }

    /// Called when the set of visible columns changed, after its
    /// `LayoutChange`: the quick filter of the default columns is rebuilt,
    /// and a focus left on a hidden column moves off it. A restored layout
    /// does the same in its own order, with one rebuild for its sort and
    /// its columns (`restore_layout`).
    pub(super) fn visible_columns_changed(&mut self, cx: &mut Context<Self>) {
        self.move_focus_off_hidden(SelectionOrigin::Programmatic, cx);
        self.quick_filter_columns_changed(cx);
    }

    /// The column under a window position, for hit tests of the body.
    pub(crate) fn column_at_position(&self, position: Point<Pixels>) -> Option<ColumnIx> {
        let x = position.x - self.body_bounds.origin.x;
        self.columns
            .column_at_x(x, self.body_bounds.size.width, self.scroll_x)
    }

    /// Moves the current column one visible place left (`-1`) or right
    /// (`1`) within its zone, if the move policy allows it.
    fn move_current_column(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(ix) = self
            .current_column
            .filter(|ix| self.columns.is_visible(*ix))
        else {
            return;
        };
        let Some((zone, i)) = self.columns.zone_position(ix) else {
            return;
        };
        let boundary = if step < 0 {
            match i.checked_sub(1) {
                Some(k) => k,
                None => return,
            }
        } else {
            i + 2
        };
        let Boundary::Gap(gap) = self
            .columns
            .zone_move(zone, vec![ix])
            .resolve(boundary, &*self.column_move)
        else {
            return;
        };
        self.change_layout(LayoutOrigin::User, cx, |layout, _| {
            if layout.move_members(zone, &[ix], gap) {
                LayoutDelta::new(LayoutPart::Order, [ix]).revealing()
            } else {
                LayoutDelta::default()
            }
        });
    }

    fn current_visible_column(&self) -> Option<ColumnIx> {
        self.current_column
            .filter(|ix| self.columns.is_visible(*ix))
    }

    pub(super) fn on_autofit_columns(
        &mut self,
        _: &AutofitColumns,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_autofit(self.columns.visible_set(), LayoutOrigin::User, cx);
    }

    pub(super) fn on_autofit_column(
        &mut self,
        _: &AutofitColumn,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.current_visible_column() {
            self.request_autofit(column_set([ix]), LayoutOrigin::User, cx);
        }
    }

    pub(super) fn on_hide_column(
        &mut self,
        _: &HideColumn,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The user does not hide a column that is not hideable;
        // `set_column_visible` does.
        if let Some(ix) = self.current_visible_column()
            && self.columns.column(ix).hideable
        {
            self.set_column_visible_from(ix, false, LayoutOrigin::User, cx);
        }
    }

    pub(super) fn on_show_all_columns(
        &mut self,
        _: &ShowAllColumns,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_all_columns_from(LayoutOrigin::User, cx);
    }

    pub(super) fn on_freeze_column_left(
        &mut self,
        _: &FreezeColumnLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.current_visible_column() {
            self.set_column_zone_from(ix, ColumnZone::FrozenLeft, LayoutOrigin::User, cx);
        }
    }

    pub(super) fn on_freeze_column_right(
        &mut self,
        _: &FreezeColumnRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.current_visible_column() {
            self.set_column_zone_from(ix, ColumnZone::FrozenRight, LayoutOrigin::User, cx);
        }
    }

    pub(super) fn on_unfreeze_column(
        &mut self,
        _: &UnfreezeColumn,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.current_visible_column() {
            self.set_column_zone_from(ix, ColumnZone::Scrolling, LayoutOrigin::User, cx);
        }
    }

    pub(super) fn on_move_column_left(
        &mut self,
        _: &MoveColumnLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_current_column(-1, cx);
    }

    pub(super) fn on_move_column_right(
        &mut self,
        _: &MoveColumnRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_current_column(1, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui_kit::px;

    use super::{
        ColumnAlign, ColumnBand, ColumnLayout, ColumnZone, GridColumn, GridSchema,
        MIN_COLUMN_WIDTH, ViewColumnIx,
    };
    use crate::{
        highlight::HighlightPolicy,
        model::{ColumnIx, GridColumns},
        policy::{BandedMove, ColumnMovePolicy, FreeMove},
        state::GridTuning,
        store::StoreOptions,
    };

    /// Up to 12 columns `c0`..`c11`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct TestColumn(u16);

    static ALL: [TestColumn; 12] = {
        let mut all = [TestColumn(0); 12];
        let mut ix = 0;
        while ix < 12 {
            all[ix] = TestColumn(ix as u16);
            ix += 1;
        }
        all
    };

    const IDS: [&str; 12] = [
        "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11",
    ];
    const BANDS: [&str; 12] = [
        "b0", "b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8", "b9", "b10", "b11",
    ];

    impl GridColumns for TestColumn {
        fn all() -> &'static [Self] {
            &ALL
        }

        fn id(self) -> &'static str {
            IDS[self.0 as usize]
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self.0)
        }
    }

    fn ix(i: u16) -> ColumnIx {
        ColumnIx::new(i)
    }

    fn column(i: u16) -> GridColumn<TestColumn> {
        GridColumn::new(TestColumn(i), IDS[i as usize])
    }

    fn zone_order(layout: &ColumnLayout<TestColumn>, zone: ColumnZone) -> Vec<u16> {
        layout
            .zone_order(zone)
            .iter()
            .map(|c| c.get() as u16)
            .collect()
    }

    fn visible(layout: &ColumnLayout<TestColumn>) -> Vec<u16> {
        layout.visible().map(|c| c.get() as u16).collect()
    }

    #[test]
    fn test_grid_schema_builder() {
        let bid = GridColumn::new(TestColumn(1), "Bid")
            .with_width(px(80.))
            .with_align(ColumnAlign::End)
            .with_min_width(px(40.))
            .with_max_width(px(300.))
            .with_zone(ColumnZone::FrozenRight)
            .with_visible(false)
            .with_resizable(false)
            .with_movable(false)
            .with_sortable(false)
            .with_groupable(false);
        assert_eq!(bid.column(), TestColumn(1));
        assert_eq!(bid.title().as_ref(), "Bid");
        assert_eq!(bid.width(), px(80.));
        assert_eq!(bid.align(), ColumnAlign::End);
        assert_eq!(bid.min_width(), px(40.));
        assert_eq!(bid.max_width(), px(300.));
        assert_eq!(bid.zone(), ColumnZone::FrozenRight);
        assert!(!bid.is_visible());
        assert!(!bid.is_resizable());
        assert!(!bid.is_movable());
        assert!(!bid.is_sortable());
        assert!(!bid.is_groupable());
        // Defaults: 100 px wide, numbers right and text left, scrolling,
        // shown, resizable, movable, sortable, groupable, no upper width
        // limit.
        let plain = column(2);
        assert_eq!(plain.width(), px(100.));
        assert_eq!(plain.align(), ColumnAlign::Auto);
        assert_eq!(plain.min_width(), MIN_COLUMN_WIDTH);
        assert_eq!(plain.zone(), ColumnZone::Scrolling);
        assert!(plain.is_visible() && plain.is_resizable() && plain.is_movable());
        assert!(plain.is_sortable() && plain.is_groupable());

        let band = ColumnBand::new("quote", "Quote")
            .column(column(2))
            .band(ColumnBand::new("depth", "Depth").column(column(3)));
        assert_eq!(band.id(), "quote");
        assert_eq!(band.title().as_ref(), "Quote");

        // `with_frozen` freezes the first declared columns over `with_zone`.
        let layout = ColumnLayout::new(
            GridSchema::new()
                .with_frozen(2)
                .column(column(0).with_zone(ColumnZone::FrozenRight))
                .band(band)
                .column(column(4).with_zone(ColumnZone::FrozenRight)),
        );
        assert_eq!(zone_order(&layout, ColumnZone::FrozenLeft), [0, 2]);
        assert_eq!(zone_order(&layout, ColumnZone::Scrolling), [3]);
        assert_eq!(zone_order(&layout, ColumnZone::FrozenRight), [4]);
        assert_eq!(layout.bands_of(ix(3)), ["quote", "depth"]);
        assert_eq!(layout.depth(), 2);

        let tuning = GridTuning::default();
        assert_eq!(tuning.min_rebuild_interval, Duration::from_millis(100));
        assert_eq!(tuning.pull_interval, Duration::from_millis(8));
        let tuning = tuning
            .with_min_rebuild_interval(Duration::from_millis(250))
            .with_pull_interval(Duration::from_millis(16));
        assert_eq!(tuning.min_rebuild_interval, Duration::from_millis(250));
        assert_eq!(tuning.pull_interval, Duration::from_millis(16));

        let options = StoreOptions::default();
        assert_eq!(options.highlight(), HighlightPolicy::default());
        // The store publishes no more often than a grid pulls.
        assert_eq!(options.publish_interval(), Duration::from_millis(8));
        assert_eq!(
            options.publish_interval(),
            GridTuning::default().pull_interval
        );
        let highlight = HighlightPolicy::new(Duration::from_secs(2), 4);
        let options = options
            .with_highlight(highlight)
            .with_publish_interval(Duration::from_millis(10));
        assert_eq!(options.highlight(), highlight);
        assert_eq!(options.publish_interval(), Duration::from_millis(10));
    }

    #[test]
    #[should_panic(expected = "already in the schema")]
    fn test_schema_rejects_repeated_column() {
        let _ = GridSchema::new()
            .column(column(1))
            .band(ColumnBand::new("b", "B").column(column(1)));
    }

    #[test]
    #[should_panic(expected = "band \"b\" is already in the schema")]
    fn test_schema_rejects_repeated_band() {
        let _ = GridSchema::new()
            .band(ColumnBand::new("b", "B").column(column(1)))
            .band(ColumnBand::new("b", "B").column(column(2)));
    }

    /// c0 50 px frozen left; c1 100, c2 80, c3 120 scrolling; c4 60 and
    /// c5 40 frozen right.
    fn zoned() -> ColumnLayout<TestColumn> {
        ColumnLayout::new(
            GridSchema::new()
                .column(
                    column(0)
                        .with_width(px(50.))
                        .with_zone(ColumnZone::FrozenLeft),
                )
                .column(column(1))
                .column(column(2).with_width(px(80.)))
                .column(column(3).with_width(px(120.)))
                .column(
                    column(4)
                        .with_width(px(60.))
                        .with_zone(ColumnZone::FrozenRight),
                )
                .column(
                    column(5)
                        .with_width(px(40.))
                        .with_zone(ColumnZone::FrozenRight),
                ),
        )
    }

    #[test]
    fn test_column_layout_zones_and_offsets() {
        let layout = zoned();
        let offsets = |zone| layout.zone_columns(zone).offsets.clone();
        assert_eq!(offsets(ColumnZone::FrozenLeft), [px(0.), px(50.)]);
        assert_eq!(
            offsets(ColumnZone::Scrolling),
            [px(0.), px(100.), px(180.), px(300.)]
        );
        assert_eq!(
            offsets(ColumnZone::FrozenRight),
            [px(0.), px(60.), px(100.)]
        );
        assert_eq!(layout.total_width(), px(450.));

        // A 300 px body: the right zone is pinned to the right edge and the
        // scrolling zone, 150 px, lies between.
        let geometry = layout.geometry(px(300.), px(0.));
        let [left, scrolling, right] = &geometry.zones;
        assert_eq!((left.x, left.width), (px(0.), px(50.)));
        assert_eq!((scrolling.x, scrolling.width), (px(50.), px(150.)));
        assert_eq!((right.x, right.width), (px(200.), px(100.)));
        assert_eq!(scrolling.columns, 0..2);
        assert_eq!(right.columns, 0..2);
        // c1 ends exactly at the scroll offset: it is out.
        assert_eq!(layout.geometry(px(300.), px(100.)).zones[1].columns, 1..3);
        assert_eq!(layout.geometry(px(300.), px(160.)).zones[1].columns, 1..3);

        // Hit tests: the right zone does not depend on the scroll offset.
        for scroll_x in [px(0.), px(100.)] {
            assert_eq!(layout.column_at_x(px(10.), px(300.), scroll_x), Some(ix(0)));
            assert_eq!(
                layout.column_at_x(px(210.), px(300.), scroll_x),
                Some(ix(4))
            );
            assert_eq!(
                layout.column_at_x(px(270.), px(300.), scroll_x),
                Some(ix(5))
            );
            assert_eq!(layout.column_at_x(px(300.), px(300.), scroll_x), None);
            assert_eq!(layout.column_at_x(px(-1.), px(300.), scroll_x), None);
        }
        assert_eq!(layout.column_at_x(px(60.), px(300.), px(0.)), Some(ix(1)));
        // 60 px on screen is 110 px into the scrolling content: c2.
        assert_eq!(layout.column_at_x(px(60.), px(300.), px(100.)), Some(ix(2)));
        assert_eq!(layout.column_at_x(px(199.), px(300.), px(0.)), Some(ix(2)));

        // Frozen zones wider than the body: the left one first, the right
        // one gets the rest, the scrolling zone none; the edge cuts them.
        let geometry = layout.geometry(px(120.), px(0.));
        let [left, scrolling, right] = &geometry.zones;
        assert_eq!((left.x, left.width), (px(0.), px(50.)));
        assert_eq!((scrolling.width, scrolling.columns.clone()), (px(0.), 0..0));
        assert_eq!((right.x, right.width), (px(50.), px(70.)));
        assert_eq!(layout.column_at_x(px(60.), px(120.), px(0.)), Some(ix(4)));
        assert_eq!(layout.column_at_x(px(115.), px(120.), px(0.)), Some(ix(5)));
        let geometry = layout.geometry(px(30.), px(0.));
        assert_eq!(geometry.zones[0].width, px(30.));
        assert_eq!(geometry.zones[2].width, px(0.));
        assert_eq!(layout.column_at_x(px(29.), px(30.), px(0.)), Some(ix(0)));

        // No frozen columns: everything scrolls, from the left edge.
        let layout = ColumnLayout::new(
            GridSchema::new()
                .column(column(0).with_width(px(50.)))
                .column(column(1)),
        );
        let geometry = layout.geometry(px(120.), px(60.));
        assert_eq!(
            (geometry.zones[1].x, geometry.zones[1].width),
            (px(0.), px(120.))
        );
        assert_eq!(geometry.zones[1].columns, 1..2);
        assert_eq!(layout.column_at_x(px(10.), px(120.), px(60.)), Some(ix(1)));

        let empty = ColumnLayout::new(GridSchema::<TestColumn>::new().with_frozen(2));
        assert_eq!(empty.total_width(), px(0.));
        assert_eq!(empty.geometry(px(200.), px(0.)).zones[1].columns, 0..0);
        assert_eq!(empty.column_at_x(px(0.), px(200.), px(0.)), None);
    }

    #[test]
    fn test_zone_change_places_column_at_nearest_edge() {
        // c0 c1 | c2 c3 c4 | c5 c6
        let layout = || {
            ColumnLayout::new(
                GridSchema::new()
                    .with_frozen(2)
                    .column(column(0))
                    .column(column(1))
                    .column(column(2))
                    .column(column(3))
                    .column(column(4))
                    .column(column(5).with_zone(ColumnZone::FrozenRight))
                    .column(column(6).with_zone(ColumnZone::FrozenRight)),
            )
        };
        let all = |layout: &ColumnLayout<TestColumn>| {
            ColumnZone::ALL.map(|zone| zone_order(layout, zone))
        };
        use ColumnZone::{FrozenLeft as L, FrozenRight as R, Scrolling as S};
        for (column, zone, expected) in [
            // Scrolling to left: the end of the left zone, next to the scrolling one.
            (3, L, [vec![0, 1, 3], vec![2, 4], vec![5, 6]]),
            // Scrolling to right: the start of the right zone.
            (3, R, [vec![0, 1], vec![2, 4], vec![3, 5, 6]]),
            // Left to scrolling: the start of the scrolling zone.
            (0, S, [vec![1], vec![0, 2, 3, 4], vec![5, 6]]),
            // Right to scrolling: the end of the scrolling zone.
            (6, S, [vec![0, 1], vec![2, 3, 4, 6], vec![5]]),
            // Left to right: the start of the right zone.
            (0, R, [vec![1], vec![2, 3, 4], vec![0, 5, 6]]),
            // Right to left: the end of the left zone.
            (6, L, [vec![0, 1, 6], vec![2, 3, 4], vec![5]]),
        ] {
            let mut layout = layout();
            assert_eq!(layout.set_zone(ix(column), zone, &FreeMove), [ix(column)]);
            assert_eq!(all(&layout), expected, "c{column} to {zone:?}");
            assert_eq!(layout.zone_of(ix(column)), zone);
            // The same zone again changes nothing.
            assert!(layout.set_zone(ix(column), zone, &FreeMove).is_empty());
        }
    }

    #[test]
    fn test_column_width_limits() {
        let narrow = column(0).with_min_width(px(5.));
        assert_eq!(
            narrow.min_width(),
            MIN_COLUMN_WIDTH,
            "raised to the minimum"
        );
        let limited = column(1)
            .with_width(px(2.))
            .with_min_width(px(30.))
            .with_max_width(px(200.));
        for (width, expected) in [
            (px(50.), px(50.)),
            (px(1000.), px(200.)),
            (px(12.), px(30.)),
            (px(0.), px(30.)),
            (px(-5.), px(30.)),
            (px(f32::NAN), px(30.)),
            (px(f32::INFINITY), px(30.)),
        ] {
            assert_eq!(limited.clamp_width(width), expected, "{width:?}");
        }
        assert_eq!(column(2).clamp_width(px(3.)), MIN_COLUMN_WIDTH);

        let mut layout = ColumnLayout::new(GridSchema::new().column(limited).column(column(2)));
        assert_eq!(
            layout.width(ix(1)),
            px(30.),
            "the schema width is clamped too"
        );
        assert!(layout.set_width(ix(1), px(120.)));
        assert!(!layout.set_width(ix(1), px(120.)), "unchanged");
        assert!(layout.set_width(ix(2), px(f32::NAN)));
        assert_eq!(layout.width(ix(2)), MIN_COLUMN_WIDTH);
        assert!(!layout.set_width(ix(9), px(50.)), "not in the schema");
        assert_eq!(
            layout.zone_columns(ColumnZone::Scrolling).offsets[2],
            px(130.)
        );
    }

    #[test]
    fn test_hide_show_keeps_position() {
        let mut layout =
            ColumnLayout::new((0..4).fold(GridSchema::new(), |schema, i| schema.column(column(i))));
        assert_eq!(layout.set_visible(ix(1), false), Some(true));
        assert_eq!(layout.set_visible(ix(1), false), Some(false));
        assert_eq!(visible(&layout), [0, 2, 3]);
        assert_eq!(zone_order(&layout, ColumnZone::Scrolling), [0, 1, 2, 3]);
        // Moves around a hidden column leave it at its place.
        assert_eq!(
            layout.move_before(ColumnZone::Scrolling, vec![ix(3)], Some(ix(0)), &FreeMove),
            Some(true)
        );
        assert_eq!(zone_order(&layout, ColumnZone::Scrolling), [3, 0, 1, 2]);
        assert_eq!(layout.set_visible(ix(1), true), Some(true));
        assert_eq!(visible(&layout), [3, 0, 1, 2]);

        // The last visible column cannot be hidden.
        for i in [0, 1, 2] {
            assert_eq!(layout.set_visible(ix(i), false), Some(true));
        }
        assert_eq!(layout.set_visible(ix(3), false), None);
        assert_eq!(visible(&layout), [3]);
        let shown = layout.show_all();
        assert_eq!(shown.len(), 3);
        assert!(shown.contains(ix(0)) && !shown.contains(ix(3)));
        assert_eq!(visible(&layout), [3, 0, 1, 2]);
        assert!(layout.show_all().is_empty());
        assert_eq!(layout.set_visible(ix(9), false), None, "not in the schema");

        // A schema with every column hidden shows the first.
        let layout = ColumnLayout::new(
            GridSchema::new()
                .column(column(5).with_visible(false))
                .column(column(6).with_visible(false)),
        );
        assert_eq!(visible(&layout), [5]);
    }

    #[test]
    fn test_view_column_ix_round_trip() {
        let mut layout = zoned();
        layout.set_visible(ix(2), false);
        layout.set_visible(ix(5), false);
        // Left zone first, then scrolling, then right; hidden ones skipped.
        assert_eq!(visible(&layout), [0, 1, 3, 4]);
        for (n, column) in layout.visible().collect::<Vec<_>>().into_iter().enumerate() {
            let view = layout.view_column_ix(column).expect("visible");
            assert_eq!(view, ViewColumnIx::new(n as u16));
            assert_eq!(view.get(), n);
            assert_eq!(layout.column_at(view), Some(column));
        }
        assert_eq!(layout.view_column_ix(ix(2)), None);
        assert_eq!(layout.view_column_ix(ix(5)), None);
        assert_eq!(layout.view_column_ix(ix(11)), None, "not in the schema");
        assert_eq!(layout.column_at(ViewColumnIx::new(4)), None);
        assert_eq!(
            layout.zone_position(ix(3)),
            Some((ColumnZone::Scrolling, 1))
        );
        assert_eq!(
            layout.zone_position(ix(4)),
            Some((ColumnZone::FrozenRight, 0))
        );
    }

    /// c0, band b0 [c1, band b1 [c2, c3], c4], c5.
    fn banded() -> GridSchema<TestColumn> {
        GridSchema::new()
            .column(column(0))
            .band(
                ColumnBand::new("b0", "Outer")
                    .column(column(1))
                    .band(
                        ColumnBand::new("b1", "Inner")
                            .column(column(2))
                            .column(column(3)),
                    )
                    .column(column(4)),
            )
            .column(column(5))
    }

    #[test]
    fn test_banded_move_keeps_bands_together() {
        let mut layout = ColumnLayout::new(banded());
        layout.normalize(&BandedMove);
        let scrolling =
            |layout: &ColumnLayout<TestColumn>| zone_order(layout, ColumnZone::Scrolling);
        let zone = ColumnZone::Scrolling;
        // Out of the band: refused; within it: done.
        assert_eq!(
            layout.move_before(zone, vec![ix(2)], Some(ix(0)), &BandedMove),
            None
        );
        assert_eq!(
            layout.move_before(zone, vec![ix(3)], Some(ix(2)), &BandedMove),
            Some(true)
        );
        assert_eq!(scrolling(&layout), [0, 1, 3, 2, 4, 5]);
        // A band moves whole.
        let members = layout.band_members(zone, "b0");
        assert_eq!(members, [1, 3, 2, 4].map(ix));
        assert_eq!(
            layout.move_before(zone, members, None, &BandedMove),
            Some(true)
        );
        assert_eq!(scrolling(&layout), [0, 5, 1, 3, 2, 4]);
        // Its zone changes for the whole top-level band.
        assert_eq!(
            layout.set_zone(ix(2), ColumnZone::FrozenLeft, &BandedMove),
            [1, 3, 2, 4].map(ix)
        );
        assert_eq!(zone_order(&layout, ColumnZone::FrozenLeft), [1, 3, 2, 4]);
        // Before itself: nothing to do.
        assert_eq!(
            layout.move_before(
                ColumnZone::FrozenLeft,
                vec![ix(1)],
                Some(ix(1)),
                &BandedMove
            ),
            Some(false)
        );
        // `normalize` gathers a band that a free order split.
        let mut layout = ColumnLayout::new(banded());
        layout.set_zone(ix(4), ColumnZone::FrozenRight, &FreeMove);
        layout.move_before(zone, vec![ix(0)], Some(ix(3)), &FreeMove);
        assert_eq!(scrolling(&layout), [1, 2, 0, 3, 5]);
        layout.normalize(&BandedMove);
        assert_eq!(scrolling(&layout), [1, 2, 3, 4, 0, 5]);
        assert!(zone_order(&layout, ColumnZone::FrozenRight).is_empty());
        // A valid order stays as it is.
        let before = layout.order().to_vec();
        layout.normalize(&BandedMove);
        assert_eq!(layout.order(), before);
    }

    #[test]
    fn test_free_move_splits_band_runs() {
        let mut layout = ColumnLayout::new(banded());
        let zone = ColumnZone::Scrolling;
        assert_eq!(
            layout.move_before(zone, vec![ix(1)], None, &BandedMove),
            None
        );
        assert_eq!(
            layout.move_before(zone, vec![ix(1)], None, &FreeMove),
            Some(true)
        );
        assert_eq!(zone_order(&layout, zone), [0, 2, 3, 4, 5, 1]);
        let outer: Vec<_> = layout
            .runs()
            .iter()
            .filter(|run| run.band == "b0")
            .map(|run| (run.columns.clone(), run.title.clone()))
            .collect();
        assert_eq!(
            outer,
            [(1..4, "Outer".into()), (5..6, "Outer".into())],
            "two runs with one title"
        );
        // A band cell drags its own run only.
        assert_eq!(layout.band_segment(zone, "b0", ix(2)), [2, 3, 4].map(ix));
        assert_eq!(layout.band_segment(zone, "b0", ix(1)), [ix(1)]);
        assert!(layout.band_segment(zone, "b1", ix(1)).is_empty());
    }

    #[test]
    fn test_band_runs_and_header_rows() {
        let mut layout = ColumnLayout::new(banded());
        // Two band levels: the header has three rows.
        assert_eq!(layout.depth(), 2);
        // A column cell starts under its deepest band.
        assert_eq!(layout.bands_of(ix(0)).len(), 0);
        assert_eq!(layout.bands_of(ix(1)).len(), 1);
        assert_eq!(layout.bands_of(ix(2)).len(), 2);
        let runs = |layout: &ColumnLayout<TestColumn>| {
            layout
                .runs()
                .iter()
                .map(|run| (run.band, run.level, run.columns.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(runs(&layout), [("b0", 0, 1..5), ("b1", 1, 2..4)]);
        // A band with no visible column draws nothing; hidden columns shrink
        // the runs over them.
        layout.set_visible(ix(2), false);
        layout.set_visible(ix(3), false);
        assert_eq!(runs(&layout), [("b0", 0, 1..3)]);
        // Runs follow the zones.
        layout.set_zone(ix(1), ColumnZone::FrozenRight, &BandedMove);
        assert_eq!(runs(&layout), [("b0", 0, 0..2)]);
        assert_eq!(layout.runs()[0].zone, ColumnZone::FrozenRight);

        let flat = ColumnLayout::new(GridSchema::new().column(column(0)));
        assert_eq!(flat.depth(), 0);
        assert!(flat.runs().is_empty());
    }

    /// A random schema of up to 12 columns in bands up to two deep, random
    /// zones, widths and visibility.
    fn random_schema(rng: &mut fastrand::Rng) -> (GridSchema<TestColumn>, usize) {
        fn items(
            rng: &mut fastrand::Rng,
            level: usize,
            next_column: &mut u16,
            next_band: &mut usize,
            limit: u16,
        ) -> Vec<Result<GridColumn<TestColumn>, ColumnBand<TestColumn>>> {
            let mut out = Vec::new();
            for _ in 0..rng.usize(1..=4) {
                if *next_column >= limit {
                    break;
                }
                if level < 2 && *next_band < BANDS.len() && rng.u8(..3) == 0 {
                    let id = BANDS[*next_band];
                    *next_band += 1;
                    let children = items(rng, level + 1, next_column, next_band, limit);
                    let band = children
                        .into_iter()
                        .fold(ColumnBand::new(id, id), |band, child| match child {
                            Ok(column) => band.column(column),
                            Err(nested) => band.band(nested),
                        });
                    out.push(Err(band));
                } else {
                    let i = *next_column;
                    *next_column += 1;
                    let zone = match rng.u8(..6) {
                        0 => ColumnZone::FrozenLeft,
                        1 => ColumnZone::FrozenRight,
                        _ => ColumnZone::Scrolling,
                    };
                    out.push(Ok(column(i)
                        .with_width(px(rng.u16(10..200) as f32))
                        .with_zone(zone)
                        .with_visible(rng.u8(..5) != 0)));
                }
            }
            out
        }
        let limit = rng.u16(1..=12);
        let (mut next_column, mut next_band) = (0, 0);
        let mut schema = GridSchema::new();
        while next_column < limit {
            for item in items(rng, 0, &mut next_column, &mut next_band, limit) {
                schema = match item {
                    Ok(column) => schema.column(column),
                    Err(band) => schema.band(band),
                };
            }
        }
        (schema, next_band)
    }

    fn check_invariants(layout: &ColumnLayout<TestColumn>, banded: bool, bands: usize) {
        let order = layout.order();
        let mut seen = order.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), order.len(), "no column twice: {order:?}");
        assert!(
            order.iter().all(|c| layout.get(*c).is_some()),
            "only schema columns"
        );
        assert!(
            order
                .windows(2)
                .all(|w| layout.zone_of(w[0]).slot() <= layout.zone_of(w[1]).slot()),
            "split by zone: {order:?}"
        );
        assert!(!layout.visible_set().is_empty(), "one column is visible");
        if banded {
            for band in &BANDS[..bands] {
                let positions: Vec<usize> = order
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| layout.bands_of(**c).contains(band))
                    .map(|(p, _)| p)
                    .collect();
                if let (Some(first), Some(last)) = (positions.first(), positions.last()) {
                    assert_eq!(last - first + 1, positions.len(), "{band} is contiguous");
                    let zone = layout.zone_of(order[*first]);
                    assert!(positions.iter().all(|p| layout.zone_of(order[*p]) == zone));
                }
            }
        }
        for zone in ColumnZone::ALL {
            let columns = layout.zone_columns(zone);
            let mut x = px(0.);
            for (i, column) in columns.columns.iter().enumerate() {
                assert_eq!(columns.offsets[i], x);
                x += layout.width(*column);
            }
            assert_eq!(columns.content_width(), x);
        }
        let visible: Vec<ColumnIx> = layout.visible().collect();
        assert_eq!(visible.len(), layout.visible_set().len());
        for (n, column) in visible.iter().enumerate() {
            assert_eq!(
                layout.view_column_ix(*column),
                Some(ViewColumnIx::new(n as u16))
            );
            assert_eq!(layout.column_at(ViewColumnIx::new(n as u16)), Some(*column));
        }
        for column in order.iter().filter(|c| !layout.is_visible(**c)) {
            assert_eq!(layout.view_column_ix(*column), None);
        }
    }

    #[test]
    fn test_column_ops_match_reference_model() {
        for seed in 0..60 {
            for banded in [true, false] {
                let mut rng = fastrand::Rng::with_seed(seed);
                let (schema, bands) = random_schema(&mut rng);
                let mut layout = ColumnLayout::new(schema);
                let policy: &dyn ColumnMovePolicy = if banded { &BandedMove } else { &FreeMove };
                layout.normalize(policy);
                // The naive model of a free order: a `Vec` of (column, zone).
                let mut model: Vec<(ColumnIx, ColumnZone)> = layout
                    .order()
                    .iter()
                    .map(|c| (*c, layout.zone_of(*c)))
                    .collect();
                check_invariants(&layout, banded, bands);
                let columns: Vec<ColumnIx> = layout.order().to_vec();
                for step in 0..40 {
                    let column = columns[rng.usize(..columns.len())];
                    match rng.u8(..5) {
                        0 => {
                            let zone = layout.zone_of(column);
                            let before = rng
                                .bool()
                                .then(|| columns[rng.usize(..columns.len())])
                                .filter(|before| layout.zone_of(*before) == zone);
                            let moved = layout.move_before(zone, vec![column], before, policy);
                            if !banded {
                                assert!(moved.is_some(), "free moves are never refused");
                                let at = model.iter().position(|(c, _)| *c == column).unwrap();
                                let entry = model.remove(at);
                                let insert = match before {
                                    Some(before) if before != column => {
                                        model.iter().position(|(c, _)| *c == before).unwrap()
                                    }
                                    Some(_) => at,
                                    None => {
                                        model.iter().rposition(|(_, z)| *z == zone).map_or_else(
                                            || {
                                                model
                                                    .iter()
                                                    .filter(|(_, z)| z.slot() < zone.slot())
                                                    .count()
                                            },
                                            |p| p + 1,
                                        )
                                    }
                                };
                                model.insert(insert, entry);
                            }
                        }
                        1 => {
                            let zone = ColumnZone::ALL[rng.usize(..3)];
                            let moved = layout.set_zone(column, zone, policy);
                            if !banded && !moved.is_empty() {
                                let at = model.iter().position(|(c, _)| *c == column).unwrap();
                                let (_, from) = model.remove(at);
                                let insert = if zone.slot() < from.slot() {
                                    model
                                        .iter()
                                        .filter(|(_, z)| z.slot() <= zone.slot())
                                        .count()
                                } else {
                                    model.iter().filter(|(_, z)| z.slot() < zone.slot()).count()
                                };
                                model.insert(insert, (column, zone));
                            }
                        }
                        2 => {
                            let shown = layout.is_visible(column);
                            layout.set_visible(column, !shown);
                        }
                        3 => {
                            layout.set_width(column, px(rng.u16(0..300) as f32));
                        }
                        _ => {
                            if let Some(&band) = layout.bands_of(column).first() {
                                let zone = layout.zone_of(column);
                                let members = layout.band_members(zone, band);
                                let before = columns[rng.usize(..columns.len())];
                                let before = (layout.zone_of(before) == zone).then_some(before);
                                let changed = layout.move_before(zone, members, before, policy);
                                let top_level = before.is_none_or(|before| {
                                    layout.bands_of(before).is_empty()
                                        || layout.bands_of(before).first() == Some(&band)
                                });
                                if banded && top_level {
                                    assert!(changed.is_some(), "a whole band moves among items");
                                } else if !banded && changed == Some(true) {
                                    model = layout
                                        .order()
                                        .iter()
                                        .map(|c| (*c, layout.zone_of(*c)))
                                        .collect();
                                }
                            }
                        }
                    }
                    check_invariants(&layout, banded, bands);
                    if !banded {
                        let expected: Vec<ColumnIx> = model.iter().map(|(c, _)| *c).collect();
                        assert_eq!(layout.order(), expected, "seed {seed} step {step}");
                    }
                }
            }
        }
    }
}
