//! The accessibility plan of the grid body. The body paints without an
//! element per cell, so the grid builds its own tree: one `Row` per painted
//! row with synthetic `GridCell` leaves, and the focused cell as a real
//! element that claims the active descendant.
//!
//! The plan is a pure function of plain frame data and needs no window:
//! GPUI's test window keeps accessibility off, so the tree is tested here.
//! The frame fills an [`A11yInput`] in its cell walk, [`plan_a11y`] turns it
//! into an [`A11yPlan`], and the body builds row elements and nodes from it.
//!
//! Rows and columns are numbered from one, as `gpui_base::table` and ARIA
//! number them ([`RowNumbering`], [`column_index`]). No node is a live
//! region: the stream is not announced; labels carry the current values,
//! and AccessKit reports the labels that changed.
//!
//! A grouped view or a tree is a `TreeGrid` ([`root_role`]). Its rows stay
//! direct children of the root, as macOS builds table rows from them; the
//! hierarchy is each row's `level` and `expanded` (ARIA allows a treegrid
//! with flat rows). The grid makes some rows itself ([`RowContent`]): group
//! captions, group footers and totals. AccessKit's macOS adapter reads
//! neither `level` nor `expanded`, so the expansion state is in the text
//! too: in the description of a caption's cell and of the tree column's
//! cell of a tree row with children.

use std::fmt::Write as _;
use std::ops::Range;

use gpui_kit::accesskit::{Action, Node, Rect, Role};
use gpui_kit::{Bounds, Pixels, SharedString, point, size};

use super::{ColumnZone, ViewColumnIx};
use crate::{highlight::ChangeDirection, model::ColumnIx, store::PinnedEdge};

/// The row index of the header row.
pub(crate) const HEADER_ROW_INDEX: usize = 1;

/// The element id name of the focused cell: its id is
/// `NamedChild(Name(FOCUS_CELL_ID), column id)` in the id scope of its row,
/// unique for the pair of row key and column.
pub(crate) const FOCUS_CELL_ID: &str = "live-grid-focus";

/// The child name of a focused caption's element: its id is
/// `NamedChild(Name(FOCUS_CELL_ID), CAPTION_FOCUS_ID)` in the id scope of
/// the caption row ([`A11yFocus::column_id`] returns it).
pub(crate) const CAPTION_FOCUS_ID: &str = "group";

/// Element id names of row elements, by id slot ([`id_slot`]). Pinned keys
/// are a space of their own, so each band names its rows; rows the grid
/// makes name theirs by what they are, and hash a `GroupPath` or a band
/// instead of a key.
const ROW_IDS: [&str; 6] = [
    "live-grid-pinned-top",
    "live-grid-row",
    "live-grid-pinned-bottom",
    "live-grid-group-row",
    "live-grid-group-footer-row",
    "live-grid-totals-row",
];

/// Between the parts of a description: the column title, the expansion
/// state, the trend and the tooltip text.
const DESCRIPTION_SEPARATOR: &str = ". ";

/// Between an aggregate's name and its column title: "Sum, Volume".
const AGGREGATE_SEPARATOR: &str = ", ";

/// Between an aggregate's description and its value, where a caption's
/// description carries it: "Sum, Volume: 1 234".
const VALUE_SEPARATOR: &str = ": ";

/// The synthetic node key ([`A11yCell::column`]) of a caption cell: no
/// column has this index, as a grid has at most 128.
const CAPTION_COLUMN: ColumnIx = ColumnIx::new(u16::MAX);

/// The slot of a caption cell, which belongs to no painted column.
const CAPTION_SLOT: u16 = u16::MAX;

/// The accessibility column index of a visible column.
pub(crate) fn column_index(view: ViewColumnIx) -> usize {
    view.get() + 1
}

/// The role of the grid's root node: `TreeGrid` for a projection built by
/// grouping or as a tree (`Projection::is_grouped() || is_tree()`), `Grid`
/// otherwise. Set whether or not the plan is built: it is a property of the
/// root element.
pub(crate) fn root_role(grouped_or_tree: bool) -> Role {
    if grouped_or_tree {
        Role::TreeGrid
    } else {
        Role::Grid
    }
}

/// How the tree numbers rows: the header, the top band, the display lines
/// by position, the bottom band. The totals row is the outermost row of its
/// band: first in the top band, last in the bottom one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowNumbering {
    top: usize,
    data: usize,
    bottom: usize,
    totals: Option<PinnedEdge>,
}

impl RowNumbering {
    /// `top` and `bottom` application rows in the bands around the `data`
    /// lines of the shown projection (`Projection::len`: in a grouped view
    /// captions, data rows and footers). Lines removed after the projection
    /// was built count: they keep their place until compaction.
    pub(crate) fn new(top: usize, data: usize, bottom: usize) -> Self {
        Self {
            top,
            data,
            bottom,
            totals: None,
        }
    }

    /// The totals row in the band at `edge`; `None` for no totals.
    /// It is counted apart from the application rows given to [`Self::new`].
    pub(crate) fn with_totals(mut self, edge: Option<PinnedEdge>) -> Self {
        self.totals = edge;
        self
    }

    /// The row count of the root node: the header and every band row.
    pub(crate) fn row_count(self) -> usize {
        HEADER_ROW_INDEX + self.top + self.data + self.bottom + usize::from(self.totals.is_some())
    }

    /// The row index of the row at `position` of its band: an application
    /// row's place among its band's application rows, or the display
    /// position of a line (`pinned` is `None`).
    pub(crate) fn row_index(self, pinned: Option<PinnedEdge>, position: usize) -> usize {
        let first = HEADER_ROW_INDEX + 1 + usize::from(self.totals == Some(PinnedEdge::Top));
        match pinned {
            Some(PinnedEdge::Top) => first + position,
            None => first + self.top + position,
            Some(PinnedEdge::Bottom) => first + self.top + self.data + position,
        }
    }

    /// The row index of the totals row, if there is one.
    pub(crate) fn totals_row_index(self) -> Option<usize> {
        let first = HEADER_ROW_INDEX + 1;
        self.totals.map(|edge| match edge {
            PinnedEdge::Top => first,
            PinnedEdge::Bottom => first + self.top + self.data + self.bottom,
        })
    }
}

/// What one item of the selection is: rows carry `selected` when the
/// selection is made of rows, cells when it is made of cells. The selection
/// mode decides it; pinned rows never carry the flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionUnit {
    Row,
    Cell,
}

/// What a painted row holds: an application row, or a row the grid makes
/// itself. Only data rows of the projection are selectable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RowContent {
    /// A data row of the projection, or an application row of a band.
    #[default]
    Data,
    /// A group caption; its key hash is `group_hash`.
    Caption,
    /// A group footer; its key hash is `footer_hash`.
    Footer,
    /// The totals row of a band; its key hash is `totals_hash`.
    Totals,
}

/// What a painted row is in a grouped view or a tree. The default is a data
/// row of a flat view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowOutline {
    pub(crate) content: RowContent,
    /// The ARIA level, `depth + 1`, of a line of a grouped view or a tree;
    /// `None` in a flat view, in the bands and for totals.
    pub(crate) level: Option<u16>,
    /// Whether a caption or a tree row with shown children (`Branch`) is
    /// expanded; `None` for any other row: ARIA allows no `aria-expanded`
    /// on a leaf.
    pub(crate) expanded: Option<bool>,
}

/// Texts the plan writes into descriptions: `live_grid.a11y.up`, `down`,
/// `expanded` and `collapsed` of the catalog. The state resolves them once
/// per locale change, never per frame or cell. The plan translates nothing
/// else: an aggregate's name and "Total" come with the calls that need
/// them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct A11yTexts {
    /// After the column title of a cell with an up arrow.
    pub(crate) rising: SharedString,
    /// After the column title of a cell with a down arrow.
    pub(crate) falling: SharedString,
    /// The state of an expanded caption or tree row.
    pub(crate) expanded: SharedString,
    /// The state of a collapsed caption or tree row.
    pub(crate) collapsed: SharedString,
    /// The note of a cell whose value is a link.
    pub(crate) link: SharedString,
}

/// A visible column painted in the frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaintedColumn<'a> {
    pub(crate) column: ColumnIx,
    pub(crate) view: ViewColumnIx,
    /// `GridColumns::id`: names the focused cell's element.
    pub(crate) id: &'static str,
    /// The column title: the description of its cells.
    pub(crate) title: &'a str,
    pub(crate) zone: ColumnZone,
    /// The left edge in window coordinates, scrolled.
    pub(crate) x: Pixels,
    pub(crate) width: Pixels,
}

/// A row painted in the frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaintedRow {
    /// The band of a pinned row or of totals; `None` for a display line.
    pub(crate) pinned: Option<PinnedEdge>,
    /// The place in its band: the display position of a line. Ignored for
    /// totals, whose place [`RowNumbering`] knows.
    pub(crate) position: usize,
    /// The hash of the row key: the identity of the row element.
    pub(crate) key_hash: u64,
    /// The top edge in window coordinates, scrolled.
    pub(crate) y: Pixels,
    pub(crate) height: Pixels,
    /// The row is selected. Read when the selection is made of rows.
    pub(crate) selected: bool,
}

/// A text of the plan: a range of its text buffer, so a frame copies cell
/// texts into one buffer instead of allocating a string per cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Span {
    start: u32,
    end: u32,
}

impl Span {
    fn push(texts: &mut String, text: &str) -> Self {
        let start = texts.len() as u32;
        texts.push_str(text);
        Self {
            start,
            end: texts.len() as u32,
        }
    }

    fn push_number(texts: &mut String, number: usize) -> Self {
        let start = texts.len() as u32;
        let _ = write!(texts, "{number}");
        Self {
            start,
            end: texts.len() as u32,
        }
    }

    /// Joins the non-empty `parts` into one text, each after its separator;
    /// the separator of the first one written is dropped. A single
    /// non-empty part is returned as it is, without a copy.
    fn join(texts: &mut String, parts: &[(&str, Span)]) -> Self {
        let mut parts = parts.iter().filter(|(_, part)| !part.is_empty());
        let Some(&(_, first)) = parts.next() else {
            return Self::default();
        };
        let Some(&(separator, second)) = parts.next() else {
            return first;
        };
        let start = texts.len() as u32;
        texts.extend_from_within(first.range());
        texts.push_str(separator);
        texts.extend_from_within(second.range());
        for &(separator, part) in parts {
            texts.push_str(separator);
            texts.extend_from_within(part.range());
        }
        Self {
            start,
            end: texts.len() as u32,
        }
    }

    /// This span, or `fallback` if it is empty.
    fn or(self, fallback: Self) -> Self {
        if self.is_empty() { fallback } else { self }
    }

    fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }

    fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// [`A11yTexts`] in the text buffer of a frame.
#[derive(Clone, Copy, Debug, Default)]
struct TextSpans {
    rising: Span,
    falling: Span,
    expanded: Span,
    collapsed: Span,
}

impl TextSpans {
    fn trend(&self, trend: Option<ChangeDirection>) -> Span {
        match trend {
            Some(ChangeDirection::Up) => self.rising,
            Some(ChangeDirection::Down) => self.falling,
            None => Span::default(),
        }
    }

    fn state(&self, expanded: Option<bool>) -> Span {
        match expanded {
            Some(true) => self.expanded,
            Some(false) => self.collapsed,
            None => Span::default(),
        }
    }
}

/// A painted column, as the input and the plan keep it.
#[derive(Clone, Copy, Debug)]
struct ColumnEntry {
    column: ColumnIx,
    zone: ColumnZone,
    x: Pixels,
    width: Pixels,
    id: &'static str,
    index: usize,
    index_text: Span,
    title: Span,
}

#[derive(Clone, Copy, Debug)]
struct RowEntry {
    row: PaintedRow,
    outline: RowOutline,
    index: usize,
    index_text: Span,
    /// Its cells: a range of the input cells.
    cells: (u32, u32),
    caption: Option<CaptionEntry>,
    /// The row's own name, such as "Total"; empty for none.
    label: Span,
}

/// The caption text cell of a caption row.
#[derive(Clone, Copy, Debug)]
struct CaptionEntry {
    text: Span,
    /// The columns it spans from column 1: at least one.
    span: u16,
    bounds: Bounds<Pixels>,
}

#[derive(Clone, Copy, Debug)]
struct CellEntry {
    /// The column's slot in the painted columns.
    slot: u16,
    text: Span,
    /// The description before the state and the trend; empty for the
    /// column title.
    description: Span,
    trend: Option<ChangeDirection>,
    /// The state of the cell's edits, "saving" or "not saved" and why;
    /// empty for none.
    edit_note: Span,
    selected: bool,
    /// A real element is the cell's node (a cell host): no synthetic node
    /// is planned for it.
    real: bool,
}

/// What the focus is on.
#[derive(Clone, Copy, Debug)]
enum FocusPlace {
    /// A cell of a data row.
    Cell(ColumnIx),
    /// A group caption: its caption cell.
    Caption,
}

#[derive(Clone, Copy, Debug)]
struct FocusTarget {
    key_hash: u64,
    place: FocusPlace,
    tooltip: Option<Span>,
}

/// What a frame painted, as plain data: rows, cells, their texts, focus and
/// selection. The frame fills it in its cell walk.
pub(crate) struct A11yInput {
    numbering: RowNumbering,
    scale_factor: f32,
    selection: Option<SelectionUnit>,
    /// Each band's clip in window coordinates, by band slot.
    band_clips: [Bounds<Pixels>; 3],
    /// Each zone's left edge and width in window coordinates, by zone slot.
    zones: [(Pixels, Pixels); 3],
    columns: Vec<ColumnEntry>,
    rows: Vec<RowEntry>,
    cells: Vec<CellEntry>,
    focus: Option<FocusTarget>,
    /// The column of a tree's chevron.
    tree_column: Option<ColumnIx>,
    words: TextSpans,
    /// "1": the column index text of caption cells, written once.
    caption_index_text: Span,
    texts: String,
}

impl A11yInput {
    /// An input for a frame of a window with `scale_factor`, numbering rows
    /// with `numbering`.
    pub(crate) fn new(numbering: RowNumbering, scale_factor: f32) -> Self {
        Self {
            numbering,
            scale_factor,
            selection: None,
            band_clips: [Bounds::default(); 3],
            zones: [(Pixels::default(), Pixels::default()); 3],
            columns: Vec::new(),
            rows: Vec::new(),
            cells: Vec::new(),
            focus: None,
            tree_column: None,
            words: TextSpans::default(),
            caption_index_text: Span::default(),
            texts: String::new(),
        }
    }

    /// Reserves room for `rows` rows and `cells` cells, such as the last
    /// frame's.
    pub(crate) fn reserve(&mut self, rows: usize, cells: usize) {
        self.rows.reserve(rows);
        self.cells.reserve(cells);
        self.texts.reserve(cells * 8);
    }

    /// What one selected item is; `None` puts no selection flags on nodes.
    pub(crate) fn set_selection_unit(&mut self, unit: Option<SelectionUnit>) {
        self.selection = unit;
    }

    /// The area of a band in window coordinates (`pinned` is `None` for the
    /// data rows). Rows and cells of the band are clipped to it; every band
    /// with rows needs one.
    pub(crate) fn set_band_clip(&mut self, pinned: Option<PinnedEdge>, clip: Bounds<Pixels>) {
        self.band_clips[band_slot(pinned)] = clip;
    }

    /// The left edge and width of a zone in window coordinates. Cells of the
    /// zone are clipped to it.
    pub(crate) fn set_zone(&mut self, zone: ColumnZone, x: Pixels, width: Pixels) {
        self.zones[zone.slot()] = (x, width);
    }

    /// The focused cell: the data row with `key_hash`, in `column`, with the
    /// text of its tooltip, if any. Set it only while the grid holds the
    /// focus; it becomes the real cell element only if painted.
    pub(crate) fn set_focus(&mut self, key_hash: u64, column: ColumnIx, tooltip: Option<&str>) {
        let tooltip = tooltip.map(|tooltip| Span::push(&mut self.texts, tooltip));
        self.focus = Some(FocusTarget {
            key_hash,
            place: FocusPlace::Cell(column),
            tooltip,
        });
    }

    /// Adds a painted column and returns its slot for [`Self::push_cell`].
    pub(crate) fn push_column(&mut self, column: PaintedColumn<'_>) -> usize {
        let index = column_index(column.view);
        let entry = ColumnEntry {
            column: column.column,
            zone: column.zone,
            x: column.x,
            width: column.width,
            id: column.id,
            index,
            index_text: Span::push_number(&mut self.texts, index),
            title: Span::push(&mut self.texts, column.title),
        };
        self.columns.push(entry);
        self.columns.len() - 1
    }

    /// Adds a painted row of a flat view or of a band. Its cells follow with
    /// [`Self::push_cell`].
    #[cfg(test)]
    pub(crate) fn push_row(&mut self, row: PaintedRow) {
        self.push_outlined_row(row, RowOutline::default());
    }

    /// Adds a painted row with what it is in a grouped view or a tree. Its
    /// cells follow: [`Self::push_caption`], [`Self::push_aggregate_cell`],
    /// [`Self::push_cell`]. Captions and footers are display lines; totals
    /// sit in the band [`RowNumbering::with_totals`] names.
    pub(crate) fn push_outlined_row(&mut self, row: PaintedRow, outline: RowOutline) {
        let index = match outline.content {
            RowContent::Data => self.numbering.row_index(row.pinned, row.position),
            RowContent::Caption | RowContent::Footer => {
                debug_assert!(row.pinned.is_none(), "captions and footers are lines");
                self.numbering.row_index(None, row.position)
            }
            RowContent::Totals => {
                debug_assert!(
                    row.pinned.is_some() && row.pinned == self.numbering.totals,
                    "totals sit in the band of `RowNumbering::with_totals`"
                );
                self.numbering
                    .totals_row_index()
                    .unwrap_or_else(|| self.numbering.row_index(row.pinned, row.position))
            }
        };
        let start = self.cells.len() as u32;
        self.rows.push(RowEntry {
            row,
            outline,
            index,
            index_text: Span::push_number(&mut self.texts, index),
            cells: (start, start),
            caption: None,
            label: Span::default(),
        });
    }

    /// Names the last row, such as the totals row "Total": its name is there
    /// whether or not a cell shows the text.
    pub(crate) fn set_row_label(&mut self, label: &str) {
        let label = Span::push(&mut self.texts, label);
        if let Some(row) = self.rows.last_mut() {
            row.label = label;
        } else {
            debug_assert!(false, "push the row first");
        }
    }

    /// Adds a cell of the last row: the column's `slot`, the cell's full
    /// text (not truncated), and whether the cell is selected, which is read
    /// when the selection is made of cells.
    pub(crate) fn push_cell(&mut self, slot: usize, text: &str, selected: bool) {
        self.push_cell_entry(slot, text, Span::default(), selected);
    }

    fn push_cell_entry(&mut self, slot: usize, text: &str, description: Span, selected: bool) {
        debug_assert!(slot < self.columns.len(), "push the column first");
        let row = self.rows.last_mut().expect("push the row first");
        row.cells.1 += 1;
        let text = Span::push(&mut self.texts, text);
        self.cells.push(CellEntry {
            slot: slot as u16,
            text,
            description,
            trend: None,
            edit_note: Span::default(),
            selected,
            real: false,
        });
    }

    /// Marks the last cell as one whose node is a real element, a cell
    /// host's: the plan builds no synthetic node for it. As the focused cell
    /// it is still planned, for the host to claim.
    pub(crate) fn set_real_cell(&mut self) {
        if let Some(cell) = self.cells.last_mut() {
            cell.real = true;
        } else {
            debug_assert!(false, "push the cell first");
        }
    }
}

impl A11yInput {
    /// The selected state a cell's node carries: `Some` when the selection
    /// is made of cells (a cell host writes it on its own node).
    pub(crate) fn cell_selected(&self, selected: bool) -> Option<bool> {
        (self.selection == Some(SelectionUnit::Cell)).then_some(selected)
    }
}

/// The element id name of a data or pinned row's element: its
/// accessibility row's, which also holds the row's cell hosts.
pub(crate) const fn data_row_id_name(pinned: Option<PinnedEdge>) -> &'static str {
    ROW_IDS[id_slot(pinned, RowContent::Data)]
}

/// Grouped views and trees.
impl A11yInput {
    /// The texts of descriptions; without them, descriptions carry no state
    /// and no trend.
    pub(crate) fn set_texts(&mut self, texts: &A11yTexts) {
        self.words = TextSpans {
            rising: Span::push(&mut self.texts, &texts.rising),
            falling: Span::push(&mut self.texts, &texts.falling),
            expanded: Span::push(&mut self.texts, &texts.expanded),
            collapsed: Span::push(&mut self.texts, &texts.collapsed),
        };
    }

    /// The column whose cells show a tree row's chevron (`TreeSpec::column`):
    /// the cell of this column of a tree row with children tells its state.
    pub(crate) fn set_tree_column(&mut self, column: ColumnIx) {
        self.tree_column = Some(column);
    }

    /// The caption text cell of the last row, a caption: the caption's full
    /// `text` (not truncated), from the first visible column up to `end`
    /// (the view position of the first column with an aggregate, or the
    /// column count), over the caption lane `bounds` in window coordinates.
    ///
    /// With `end` at 0 (an aggregate in the first visible column) the cell
    /// spans that column alone, and the aggregate's cell of that column goes
    /// into the caption's description instead of a cell of its own.
    pub(crate) fn push_caption(&mut self, text: &str, end: ViewColumnIx, bounds: Bounds<Pixels>) {
        if self.caption_index_text.is_empty() {
            self.caption_index_text = Span::push_number(&mut self.texts, 1);
        }
        let text = Span::push(&mut self.texts, text);
        let row = self.rows.last_mut().expect("push the row first");
        debug_assert_eq!(row.outline.content, RowContent::Caption);
        debug_assert!(row.caption.is_none(), "one caption cell per row");
        row.caption = Some(CaptionEntry {
            text,
            span: end.get().clamp(1, u16::MAX as usize) as u16,
            bounds,
        });
    }

    /// An aggregate cell of the last row, a caption, a footer or totals: the
    /// value's text in the column's `slot`, described by the `aggregate`'s
    /// name and the column title ("Sum, Volume"). An empty name (a custom
    /// aggregate without one) leaves the column title alone.
    pub(crate) fn push_aggregate_cell(&mut self, slot: usize, text: &str, aggregate: &str) {
        debug_assert!(slot < self.columns.len(), "push the column first");
        let description = if aggregate.is_empty() {
            Span::default()
        } else {
            let title = self.columns[slot].title;
            let start = self.texts.len() as u32;
            self.texts.push_str(aggregate);
            if !title.is_empty() {
                self.texts.push_str(AGGREGATE_SEPARATOR);
                self.texts.extend_from_within(title.range());
            }
            Span {
                start,
                end: self.texts.len() as u32,
            }
        };
        self.push_cell_entry(slot, text, description, false);
    }

    /// The arrow of the cell just pushed (`CellStyle::trend`): its
    /// description tells the direction after the column title.
    /// Tells the state of the last cell's edits, after its
    /// column title and its row's state, before its trend: "saving", or
    /// "not saved" and why; or that the cell is a link.
    pub(crate) fn set_edit_note(&mut self, note: &str) {
        let note = Span::push(&mut self.texts, note);
        if let Some(cell) = self.cells.last_mut() {
            cell.edit_note = note;
        } else {
            debug_assert!(false, "push the cell first");
        }
    }

    pub(crate) fn set_trend(&mut self, trend: ChangeDirection) {
        let has_cell = self
            .rows
            .last()
            .is_some_and(|row| row.cells.1 > row.cells.0);
        debug_assert!(has_cell, "push the cell first");
        if has_cell && let Some(cell) = self.cells.last_mut() {
            cell.trend = Some(trend);
        }
    }

    /// The focused caption: the caption row with `key_hash` (`group_hash`),
    /// with the text of its caption's tooltip, if any. Its caption cell is
    /// planned as the real focus element, if painted. Set it only while the
    /// grid holds the focus.
    pub(crate) fn set_caption_focus(&mut self, key_hash: u64, tooltip: Option<&str>) {
        let tooltip = tooltip.map(|tooltip| Span::push(&mut self.texts, tooltip));
        self.focus = Some(FocusTarget {
            key_hash,
            place: FocusPlace::Caption,
            tooltip,
        });
    }
}

/// A row of the plan: a row element with a `Row` node.
#[derive(Clone, Debug)]
pub(crate) struct A11yRow {
    pinned: Option<PinnedEdge>,
    outline: RowOutline,
    key_hash: u64,
    index: usize,
    index_text: Span,
    bounds: Bounds<Pixels>,
    selected: Option<bool>,
    /// Its synthetic cells: a range of the plan cells.
    cells: (u32, u32),
    /// The row's own name; empty for none.
    label: Span,
}

impl A11yRow {
    /// The band of a pinned row or of totals; `None` for a display line.
    #[cfg(test)]
    pub(crate) fn pinned(&self) -> Option<PinnedEdge> {
        self.pinned
    }

    /// The row element id is `NamedInteger(id_name, key_hash)`.
    pub(crate) fn id_name(&self) -> &'static str {
        ROW_IDS[id_slot(self.pinned, self.outline.content)]
    }

    /// See [`Self::id_name`].
    pub(crate) fn key_hash(&self) -> u64 {
        self.key_hash
    }

    pub(crate) fn row_index(&self) -> usize {
        self.index
    }

    /// The row's visible part (its band's clip) in window coordinates: the
    /// bounds of the row element.
    pub(crate) fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// `Some` when the selection is made of rows: an unselected data row
    /// carries `false`, so platforms see the row as selectable.
    pub(crate) fn selected(&self) -> Option<bool> {
        self.selected
    }
}

/// Readers for the row element: its place in a grouped view or a tree.
impl A11yRow {
    pub(crate) fn content(&self) -> RowContent {
        self.outline.content
    }

    /// The ARIA level; `None` in a flat view.
    pub(crate) fn level(&self) -> Option<usize> {
        self.outline.level.map(usize::from)
    }

    /// `Some` for a caption and a tree row with shown children.
    pub(crate) fn expanded(&self) -> Option<bool> {
        self.outline.expanded
    }

    /// The action the row's node offers, opposite to its state: `Expand`
    /// when collapsed, `Collapse` when expanded. The row element listens
    /// for it (`on_a11y_action`) and expands or collapses as the chevron
    /// does.
    pub(crate) fn disclosure_action(&self) -> Option<Action> {
        self.outline.expanded.map(|expanded| {
            if expanded {
                Action::Collapse
            } else {
                Action::Expand
            }
        })
    }
}

/// A cell of the plan: a synthetic `GridCell` leaf, or the focused cell.
#[derive(Clone, Debug)]
pub(crate) struct A11yCell {
    column: ColumnIx,
    /// The column's slot in the painted columns; [`CAPTION_SLOT`] for a
    /// caption cell.
    slot: u16,
    /// The columns a caption cell spans; 0 for any other cell, which sets
    /// no `column_span`.
    span: u16,
    text: Span,
    description: Span,
    bounds: Bounds<Pixels>,
    selected: Option<bool>,
}

impl A11yCell {
    /// The synthetic node id is `builder.synthetic_node_id(column)`, unique
    /// in its row. A caption cell has a key of its own, which no column has.
    pub(crate) fn column(&self) -> ColumnIx {
        self.column
    }

    /// The cell's visible part (cell ∩ its zone ∩ its band) in window
    /// coordinates; for a caption cell, the caption lane ∩ its row.
    pub(crate) fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// `Some` when the selection is made of cells and the cell is in a data
    /// row.
    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<bool> {
        self.selected
    }

    /// Whether this is the caption cell of a group caption.
    pub(crate) fn is_caption(&self) -> bool {
        self.slot == CAPTION_SLOT
    }
}

/// The focused cell: a real element inside its row element, which claims
/// the active descendant.
#[derive(Clone, Debug)]
pub(crate) struct A11yFocus {
    /// Its row: an index of [`A11yPlan::rows`].
    row_ix: usize,
    cell: A11yCell,
    column_id: &'static str,
    description: Span,
}

impl A11yFocus {
    /// The index of its row in [`A11yPlan::rows`].
    pub(crate) fn row_ix(&self) -> usize {
        self.row_ix
    }

    pub(crate) fn cell(&self) -> &A11yCell {
        &self.cell
    }

    /// `GridColumns::id` of its column, or [`CAPTION_FOCUS_ID`] for a
    /// caption: the element id is `NamedChild(Name(FOCUS_CELL_ID),
    /// column_id)` in its row's id scope.
    pub(crate) fn column_id(&self) -> &'static str {
        self.column_id
    }
}

/// The accessibility tree of the body for one frame: rows in tree order
/// (top band, display lines, bottom band), their synthetic cells, and the
/// focused cell apart from them.
pub(crate) struct A11yPlan {
    scale_factor: f32,
    columns: Vec<ColumnEntry>,
    rows: Vec<A11yRow>,
    cells: Vec<A11yCell>,
    focus: Option<A11yFocus>,
    caption_index_text: Span,
    texts: String,
}

impl A11yPlan {
    /// Rows in tree order.
    pub(crate) fn rows(&self) -> &[A11yRow] {
        &self.rows
    }

    /// The synthetic cells of `row`, left to right. The focused cell is not
    /// among them.
    pub(crate) fn cells(&self, row: &A11yRow) -> &[A11yCell] {
        &self.cells[row.cells.0 as usize..row.cells.1 as usize]
    }

    /// Synthetic cells of all rows.
    pub(crate) fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// The focused cell, if it was painted.
    pub(crate) fn focus(&self) -> Option<&A11yFocus> {
        self.focus.as_ref()
    }

    // What a test reads of the focused cell.
    /// The description of the focused cell, if it was painted: the column
    /// title, the state, the trend and the tooltip text.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn focus_description(&self) -> Option<&str> {
        self.focus
            .as_ref()
            .map(|focus| self.text(focus.description))
    }

    /// Writes the row's name, index, index text, selection, level and
    /// expansion into the node of its row element, with the action that
    /// expands or collapses it.
    pub(crate) fn write_row(&self, row: &A11yRow, node: &mut Node) {
        if !row.label.is_empty() {
            node.set_label(self.text(row.label));
        }
        node.set_row_index(row.index);
        node.set_row_index_text(self.text(row.index_text));
        if let Some(selected) = row.selected {
            node.set_selected(selected);
        }
        if let Some(level) = row.outline.level {
            node.set_level(usize::from(level));
        }
        if let Some(expanded) = row.outline.expanded {
            node.set_expanded(expanded);
            node.add_action(if expanded {
                Action::Collapse
            } else {
                Action::Expand
            });
        }
    }

    /// The synthetic `GridCell` node of a cell of `row`.
    pub(crate) fn cell_node(&self, row: &A11yRow, cell: &A11yCell) -> Node {
        let mut node = Node::new(Role::GridCell);
        self.write_cell(row, cell, cell.description, &mut node);
        node
    }

    /// Writes the focused cell into the node of its element: everything a
    /// synthetic cell has, with the tooltip text at the end of the
    /// description.
    pub(crate) fn write_focus(&self, focus: &A11yFocus, node: &mut Node) {
        node.set_role(Role::GridCell);
        let row = &self.rows[focus.row_ix];
        self.write_cell(row, &focus.cell, focus.description, node);
    }

    fn write_cell(&self, row: &A11yRow, cell: &A11yCell, description: Span, node: &mut Node) {
        let (index, index_text) = if cell.slot == CAPTION_SLOT {
            (1, self.caption_index_text)
        } else {
            let column = &self.columns[cell.slot as usize];
            (column.index, column.index_text)
        };
        // An empty cell gets an empty label: the cell is there, without a value.
        node.set_label(self.text(cell.text));
        if !description.is_empty() {
            node.set_description(self.text(description));
        }
        node.set_column_index(index);
        // Windows reads only the index texts (AccessKit 0.24).
        node.set_column_index_text(self.text(index_text));
        if cell.span > 0 {
            node.set_column_span(usize::from(cell.span));
        }
        node.set_row_index_text(self.text(row.index_text));
        if let Some(selected) = cell.selected {
            node.set_selected(selected);
        }
        node.set_bounds(device_rect(cell.bounds, self.scale_factor));
    }

    /// Builds and drops every node GPUI builds from the plan when
    /// accessibility is active: each row's, its synthetic cells' and the
    /// focused cell's. For a frame planned with
    /// `LiveGridState::set_a11y_forced` in a window whose accessibility is
    /// off, so headless tests and the benchmark (S10) run and time that work
    /// too. Returns the number of nodes.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn build_nodes(&self) -> usize {
        use std::hint::black_box;

        let mut count = 0;
        for row in &self.rows {
            let mut node = Node::new(Role::Row);
            self.write_row(row, &mut node);
            black_box(node);
            for cell in self.cells(row) {
                black_box(self.cell_node(row, cell));
            }
            count += 1 + self.cells(row).len();
        }
        if let Some(focus) = &self.focus {
            let mut node = Node::new(Role::GridCell);
            self.write_focus(focus, &mut node);
            black_box(node);
            count += 1;
        }
        count
    }

    fn text(&self, span: Span) -> &str {
        &self.texts[span.range()]
    }
}

/// Plans the accessibility tree of a frame.
///
/// Rows come out in tree order. A row whose key hash repeats an earlier
/// row's with the same id name is left out (two keys with one hash would
/// make one node id twice); it is still painted. The focused cell is planned
/// apart, and only if both its row and its column were painted; a focused
/// caption, only if its caption cell was.
pub(crate) fn plan_a11y(input: A11yInput) -> A11yPlan {
    let A11yInput {
        numbering: _,
        scale_factor,
        selection,
        band_clips,
        zones,
        columns,
        rows: input_rows,
        cells: input_cells,
        focus: focus_target,
        tree_column,
        words,
        caption_index_text,
        mut texts,
    } = input;

    // First by id name and key hash to find repeats, then in tree order.
    let mut order: Vec<RowOrder> = input_rows
        .iter()
        .enumerate()
        .map(|(ix, entry)| RowOrder {
            index: entry.index,
            id_slot: id_slot(entry.row.pinned, entry.outline.content),
            key_hash: entry.row.key_hash,
            ix: ix as u32,
            kept: true,
        })
        .collect();
    order.sort_unstable_by_key(|row| (row.id_slot, row.key_hash, row.index, row.ix));
    for i in 1..order.len() {
        let previous = order[i - 1];
        let current = &mut order[i];
        if (previous.id_slot, previous.key_hash) == (current.id_slot, current.key_hash) {
            current.kept = false;
            if cfg!(debug_assertions) {
                log::warn!(
                    "live grid: rows {} and {} share key hash {:#x}; the second is left out of \
                     the accessibility tree",
                    previous.index,
                    current.index,
                    current.key_hash,
                );
            }
        }
    }
    order.retain(|row| row.kept);
    order.sort_unstable_by_key(|row| (row.index, row.ix));

    // A cell's mask: its band's clip ∩ its zone.
    let masks = band_clips.map(|clip| {
        zones.map(|(x, width)| {
            Bounds::new(point(x, clip.origin.y), size(width, clip.size.height)).intersect(&clip)
        })
    });

    let mut rows = Vec::with_capacity(order.len());
    let mut cells = Vec::with_capacity(input_cells.len());
    let mut focus = None;
    for RowOrder { ix, .. } in order {
        let entry = &input_rows[ix as usize];
        let (row, outline) = (&entry.row, entry.outline);
        let band = band_slot(row.pinned);
        let is_data = row.pinned.is_none() && outline.content == RowContent::Data;
        let clip = band_clips[band];
        let row_bounds = Bounds::new(
            point(clip.origin.x, row.y),
            size(clip.size.width, row.height),
        )
        .intersect(&clip);
        // Captions, footers and totals are never selected, not even with an
        // explicit `false`.
        let row_selected =
            (is_data && selection == Some(SelectionUnit::Row)).then_some(row.selected);
        let cell_selection = is_data && selection == Some(SelectionUnit::Cell);
        // Key hashes of kept rows are unique per id name: at most one row
        // matches.
        let focused = focus_target
            .filter(|target| target.key_hash == row.key_hash)
            .map(|target| target.place)
            .filter(|place| match place {
                FocusPlace::Cell(_) => is_data,
                FocusPlace::Caption => outline.content == RowContent::Caption,
            });
        let focus_column = match focused {
            Some(FocusPlace::Cell(column)) => Some(column),
            _ => None,
        };
        let state = words.state(outline.expanded);
        let row_cells = &input_cells[entry.cells.0 as usize..entry.cells.1 as usize];

        let start = cells.len() as u32;
        // The caption cell spans columns 1..=span. A cell under it, the
        // aggregate of the first column, goes into its description: two
        // cells of a row never share a column index.
        let caption_span = entry.caption.map_or(0, |caption| usize::from(caption.span));
        if let Some(caption) = entry.caption {
            let mut under = Span::default();
            for cell in row_cells
                .iter()
                .filter(|cell| columns[cell.slot as usize].index <= caption_span)
            {
                let title = columns[cell.slot as usize].title;
                let item = Span::join(
                    &mut texts,
                    &[
                        ("", cell.description.or(title)),
                        (VALUE_SEPARATOR, cell.text),
                        (DESCRIPTION_SEPARATOR, words.trend(cell.trend)),
                    ],
                );
                under = Span::join(&mut texts, &[("", under), (DESCRIPTION_SEPARATOR, item)]);
            }
            let planned = A11yCell {
                column: CAPTION_COLUMN,
                slot: CAPTION_SLOT,
                span: caption.span,
                text: caption.text,
                description: Span::join(&mut texts, &[("", state), (DESCRIPTION_SEPARATOR, under)]),
                bounds: caption.bounds.intersect(&row_bounds),
                selected: None,
            };
            if matches!(focused, Some(FocusPlace::Caption)) {
                focus = Some(A11yFocus {
                    row_ix: rows.len(),
                    description: planned.description,
                    cell: planned,
                    column_id: CAPTION_FOCUS_ID,
                });
            } else {
                cells.push(planned);
            }
        }
        for cell in row_cells {
            let column = &columns[cell.slot as usize];
            if column.index <= caption_span {
                continue;
            }
            let bounds = Bounds::new(point(column.x, row.y), size(column.width, row.height))
                .intersect(&masks[band][column.zone.slot()]);
            // The tree column's cell of a tree row with children tells the
            // row's state; a flat cell keeps its column title alone.
            let row_state =
                if outline.content == RowContent::Data && tree_column == Some(column.column) {
                    state
                } else {
                    Span::default()
                };
            let named = cell.description.or(column.title);
            let description =
                if row_state.is_empty() && cell.trend.is_none() && cell.edit_note.is_empty() {
                    // A flat cell: no copy.
                    named
                } else {
                    Span::join(
                        &mut texts,
                        &[
                            ("", named),
                            (DESCRIPTION_SEPARATOR, row_state),
                            (DESCRIPTION_SEPARATOR, cell.edit_note),
                            (DESCRIPTION_SEPARATOR, words.trend(cell.trend)),
                        ],
                    )
                };
            let planned = A11yCell {
                column: column.column,
                slot: cell.slot,
                span: 0,
                text: cell.text,
                description,
                bounds,
                selected: cell_selection.then_some(cell.selected),
            };
            if focus_column == Some(column.column) {
                focus = Some(A11yFocus {
                    row_ix: rows.len(),
                    description: planned.description,
                    cell: planned,
                    column_id: column.id,
                });
            } else if !cell.real {
                cells.push(planned);
            }
        }
        rows.push(A11yRow {
            pinned: row.pinned,
            outline,
            key_hash: row.key_hash,
            index: entry.index,
            index_text: entry.index_text,
            bounds: row_bounds,
            selected: row_selected,
            cells: (start, cells.len() as u32),
            label: entry.label,
        });
    }

    // The focused cell's description ends with its tooltip, unless the
    // tooltip of a caption repeats the caption, the label already (the
    // frame drops a cell's own repeats).
    if let (Some(focus), Some(tooltip)) = (
        focus.as_mut(),
        focus_target.and_then(|target| target.tooltip),
    ) && !tooltip.is_empty()
        && !(focus.cell.is_caption() && texts[tooltip.range()] == texts[focus.cell.text.range()])
    {
        focus.description = Span::join(
            &mut texts,
            &[("", focus.description), (DESCRIPTION_SEPARATOR, tooltip)],
        );
    }

    A11yPlan {
        scale_factor,
        columns,
        rows,
        cells,
        focus,
        caption_index_text,
        texts,
    }
}

/// An input row on its way to the plan.
#[derive(Clone, Copy)]
struct RowOrder {
    index: usize,
    id_slot: usize,
    key_hash: u64,
    /// Its place in the input.
    ix: u32,
    kept: bool,
}

/// A band's place in per-band arrays, in tree order.
const fn band_slot(pinned: Option<PinnedEdge>) -> usize {
    match pinned {
        Some(PinnedEdge::Top) => 0,
        None => 1,
        Some(PinnedEdge::Bottom) => 2,
    }
}

/// A row's id name in [`ROW_IDS`]: application rows by band, rows the grid
/// makes by what they are.
const fn id_slot(pinned: Option<PinnedEdge>, content: RowContent) -> usize {
    match content {
        RowContent::Data => band_slot(pinned),
        RowContent::Caption => 3,
        RowContent::Footer => 4,
        RowContent::Totals => 5,
    }
}

/// Bounds in device pixels, as GPUI computes them for real nodes.
fn device_rect(bounds: Bounds<Pixels>, scale_factor: f32) -> Rect {
    let x = bounds.origin.x.as_f32();
    let y = bounds.origin.y.as_f32();
    let right = x + bounds.size.width.as_f32();
    let bottom = y + bounds.size.height.as_f32();
    Rect {
        x0: (x * scale_factor) as f64,
        y0: (y * scale_factor) as f64,
        x1: (right * scale_factor) as f64,
        y1: (bottom * scale_factor) as f64,
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::accesskit::{Action, Node, Rect, Role};
    use gpui_kit::{Bounds, Pixels, point, px, size};

    use super::{
        A11yCell, A11yInput, A11yPlan, A11yRow, A11yTexts, CAPTION_COLUMN, CAPTION_FOCUS_ID,
        FOCUS_CELL_ID, HEADER_ROW_INDEX, PaintedColumn, PaintedRow, RowContent, RowNumbering,
        RowOutline, SelectionUnit, data_row_id_name, plan_a11y, root_role,
    };
    use crate::{
        highlight::ChangeDirection,
        model::ColumnIx,
        state::{ColumnZone, ViewColumnIx},
        store::PinnedEdge,
    };

    const ROW_HEIGHT: Pixels = px(20.);

    /// The painted columns of a body at x 100..500: `c3` frozen left at
    /// 100..180, `c0`..`c2` in the scrolling zone at 180..420 (scrolled by
    /// 50 px, so `c0` is half under the left zone and `c2` is cut by the
    /// right zone; `c4` is scrolled out), `c5` frozen right at 420..500.
    /// Display order differs from schema order on purpose.
    const COLUMNS: [PaintedColumn<'static>; 5] = [
        column(3, 0, "Side", ColumnZone::FrozenLeft, 100., 80.),
        column(0, 1, "Price", ColumnZone::Scrolling, 130., 100.),
        column(1, 2, "Size", ColumnZone::Scrolling, 230., 100.),
        column(2, 3, "Comment", ColumnZone::Scrolling, 330., 100.),
        column(5, 5, "Total", ColumnZone::FrozenRight, 420., 80.),
    ];
    const PRICE: usize = 1;
    const COMMENT: usize = 3;
    const IDS: [&str; 6] = ["c0", "c1", "c2", "c3", "c4", "c5"];

    const fn column(
        ix: u16,
        view: u16,
        title: &'static str,
        zone: ColumnZone,
        x: f32,
        width: f32,
    ) -> PaintedColumn<'static> {
        PaintedColumn {
            column: ColumnIx::new(ix),
            view: ViewColumnIx::new(view),
            id: IDS[ix as usize],
            title,
            zone,
            x: px(x),
            width: px(width),
        }
    }

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    /// The hash the frame gives a data row of `key` (`state::key_hash`).
    fn key_hash(key: u64) -> u64 {
        crate::state::key_hash(&key)
    }

    fn text(key: u64, slot: usize) -> String {
        format!("{key}:{}", COLUMNS[slot].id)
    }

    /// An input with the data band at y 50..350 and the zones of `COLUMNS`.
    fn frame_input(numbering: RowNumbering, scale_factor: f32) -> A11yInput {
        let mut input = A11yInput::new(numbering, scale_factor);
        input.set_band_clip(None, bounds(100., 50., 400., 300.));
        input.set_zone(ColumnZone::FrozenLeft, px(100.), px(80.));
        input.set_zone(ColumnZone::Scrolling, px(180.), px(240.));
        input.set_zone(ColumnZone::FrozenRight, px(420.), px(80.));
        for column in COLUMNS {
            input.push_column(column);
        }
        input
    }

    /// A row of `key` with every column painted and `selected_slots`
    /// selected; `y` is its top edge.
    fn push_row(
        input: &mut A11yInput,
        pinned: Option<PinnedEdge>,
        position: usize,
        key: u64,
        y: f32,
        selected_slots: &[usize],
    ) {
        input.push_row(PaintedRow {
            pinned,
            position,
            key_hash: key_hash(key),
            y: px(y),
            height: ROW_HEIGHT,
            selected: !selected_slots.is_empty(),
        });
        for slot in 0..COLUMNS.len() {
            input.push_cell(slot, &text(key, slot), selected_slots.contains(&slot));
        }
    }

    /// Data rows `keys` from display position `first`, from the top of the
    /// data band.
    fn push_data_rows(input: &mut A11yInput, first: usize, keys: &[u64]) {
        for (line, &key) in keys.iter().enumerate() {
            let y = 50. + ROW_HEIGHT.as_f32() * line as f32;
            push_row(input, None, first + line, key, y, &[]);
        }
    }

    fn row_node(plan: &A11yPlan, row: &A11yRow) -> Node {
        let mut node = Node::new(Role::Row);
        plan.write_row(row, &mut node);
        node
    }

    fn row_of(plan: &A11yPlan, key: u64) -> &A11yRow {
        plan.rows()
            .iter()
            .find(|row| row.pinned().is_none() && row.key_hash() == key_hash(key))
            .expect("the row is planned")
    }

    #[test]
    fn test_a11y_indices_with_pinned_rows() {
        let numbering = RowNumbering::new(2, 30, 1);
        assert_eq!(HEADER_ROW_INDEX, 1);
        assert_eq!(numbering.row_count(), 34);

        // Top band at 50..90, data at 90..330, bottom band at 330..350; data
        // scrolled to display position 10.
        let mut input = frame_input(numbering, 1.);
        input.set_band_clip(Some(PinnedEdge::Top), bounds(100., 50., 400., 40.));
        input.set_band_clip(None, bounds(100., 90., 400., 240.));
        input.set_band_clip(Some(PinnedEdge::Bottom), bounds(100., 330., 400., 20.));
        // Bands are pushed around the data rows; the tree is in index order.
        push_row(&mut input, Some(PinnedEdge::Bottom), 0, 900, 330., &[]);
        for line in 0..12 {
            let y = 90. + ROW_HEIGHT.as_f32() * line as f32;
            push_row(&mut input, None, 10 + line, 100 + line as u64, y, &[]);
        }
        for j in 0..2 {
            let y = 50. + ROW_HEIGHT.as_f32() * j as f32;
            push_row(&mut input, Some(PinnedEdge::Top), j, 800 + j as u64, y, &[]);
        }
        let plan = plan_a11y(input);

        let indices: Vec<usize> = plan.rows().iter().map(A11yRow::row_index).collect();
        let expected: Vec<usize> = [2, 3].into_iter().chain(14..=25).chain([34]).collect();
        assert_eq!(indices, expected);
        let names: Vec<&str> = plan.rows().iter().map(A11yRow::id_name).collect();
        assert_eq!(names[..2], ["live-grid-pinned-top"; 2]);
        assert_eq!(names[2..14], ["live-grid-row"; 12]);
        assert_eq!(names[14], "live-grid-pinned-bottom");

        for row in plan.rows() {
            let node = row_node(&plan, row);
            assert_eq!(node.row_index(), Some(row.row_index()));
            assert_eq!(
                node.row_index_text(),
                Some(row.row_index().to_string().as_str())
            );
            // `ViewColumnIx + 1` in all three zones, not `ColumnIx + 1`.
            let columns: Vec<_> = plan
                .cells(row)
                .iter()
                .map(|cell| {
                    let node = plan.cell_node(row, cell);
                    assert_eq!(
                        node.row_index_text(),
                        Some(row.row_index().to_string().as_str())
                    );
                    assert_eq!(
                        node.column_index_text(),
                        node.column_index().map(|ix| ix.to_string()).as_deref()
                    );
                    node.column_index().unwrap()
                })
                .collect();
            assert_eq!(columns, [1, 2, 3, 4, 6]);
        }
    }

    #[test]
    fn test_a11y_cell_nodes() {
        // Scrolled 10 px into the first data row; a 2x display.
        let mut input = frame_input(RowNumbering::new(0, 10, 0), 2.);
        input.push_row(PaintedRow {
            pinned: None,
            position: 0,
            key_hash: key_hash(1),
            y: px(40.),
            height: ROW_HEIGHT,
            selected: false,
        });
        let long = "A comment far wider than its 100 px column, cut with an ellipsis on screen";
        for slot in 0..COLUMNS.len() {
            let text = match slot {
                PRICE => "101.25",
                COMMENT => long,
                _ => "",
            };
            input.push_cell(slot, text, false);
        }
        let plan = plan_a11y(input);
        let row = &plan.rows()[0];
        assert_eq!(row.bounds(), bounds(100., 50., 400., 10.));
        let nodes: Vec<Node> = plan
            .cells(row)
            .iter()
            .map(|cell| plan.cell_node(row, cell))
            .collect();

        for node in &nodes {
            assert_eq!(node.role(), Role::GridCell);
            assert_eq!(node.row_index_text(), Some("2"));
            assert_eq!(node.live(), None);
            assert_eq!(node.is_selected(), None);
        }
        let price = &nodes[PRICE];
        assert_eq!(price.label(), Some("101.25"));
        assert_eq!(price.description(), Some("Price"));
        assert_eq!(price.column_index(), Some(2));
        assert_eq!(price.column_index_text(), Some("2"));
        // Half under the frozen left zone, cut by the data band's top edge;
        // in device pixels.
        assert_eq!(
            price.bounds(),
            Some(Rect {
                x0: 360.,
                y0: 100.,
                x1: 460.,
                y1: 120.,
            })
        );
        // The full text: the screen truncates it, the label does not.
        let comment = &nodes[COMMENT];
        assert_eq!(comment.label(), Some(long));
        assert_eq!(comment.description(), Some("Comment"));
        // Cut by the frozen right zone.
        assert_eq!(
            comment.bounds(),
            Some(Rect {
                x0: 660.,
                y0: 100.,
                x1: 840.,
                y1: 120.,
            })
        );
        // An empty cell has an empty label.
        assert_eq!(nodes[0].label(), Some(""));
        assert_eq!(nodes[0].description(), Some("Side"));
        assert_eq!(nodes[0].column_index_text(), Some("1"));
    }

    #[test]
    fn test_a11y_focus_cell_excluded_from_synthetic() {
        let price = COLUMNS[PRICE].column;

        let mut input = frame_input(RowNumbering::new(1, 10, 0), 1.);
        input.set_band_clip(Some(PinnedEdge::Top), bounds(100., 30., 400., 20.));
        // A pinned row with the focused key: pinned rows are never focused.
        push_row(&mut input, Some(PinnedEdge::Top), 0, 2, 30., &[]);
        push_data_rows(&mut input, 0, &[1, 2, 3]);
        input.set_focus(key_hash(2), price, Some("Last trade 10:31"));
        let plan = plan_a11y(input);

        let focus = plan.focus().expect("the focused cell is painted");
        let row = &plan.rows()[focus.row_ix()];
        assert_eq!(row.pinned(), None);
        assert_eq!(row.key_hash(), key_hash(2));
        assert_eq!(focus.cell().column(), price);
        assert_eq!(focus.column_id(), "c0");
        assert_eq!(focus.cell().bounds(), bounds(180., 70., 50., 20.));
        assert!(plan.cells(row).iter().all(|cell| cell.column() != price));
        assert_eq!(plan.cells(row).len(), COLUMNS.len() - 1);
        for other in plan
            .rows()
            .iter()
            .filter(|other| other.key_hash() != key_hash(2))
        {
            assert_eq!(plan.cells(other).len(), COLUMNS.len());
        }
        assert_eq!(plan.cell_count(), 4 * COLUMNS.len() - 1);
        // What GPUI builds from it: a node per row, per synthetic cell and
        // the focused cell's.
        assert_eq!(plan.build_nodes(), 4 + plan.cell_count() + 1);

        let mut node = Node::new(Role::Unknown);
        plan.write_focus(focus, &mut node);
        assert_eq!(node.role(), Role::GridCell);
        assert_eq!(node.label(), Some(text(2, PRICE).as_str()));
        assert_eq!(node.description(), Some("Price. Last trade 10:31"));
        assert_eq!(node.column_index(), Some(2));
        assert_eq!(node.column_index_text(), Some("2"));
        assert_eq!(node.row_index_text(), Some("4"));
        assert_eq!(node.live(), None);

        // Without a tooltip the description is the column title.
        let mut input = frame_input(RowNumbering::new(0, 10, 0), 1.);
        push_data_rows(&mut input, 0, &[1, 2, 3]);
        input.set_focus(key_hash(2), price, None);
        let plan = plan_a11y(input);
        let mut node = Node::new(Role::GridCell);
        plan.write_focus(plan.focus().unwrap(), &mut node);
        assert_eq!(node.description(), Some("Price"));

        // Focus on a row scrolled off screen, or on a column scrolled out:
        // no focused cell, every painted cell is synthetic.
        let off_screen = [(key_hash(9), price), (key_hash(2), ColumnIx::new(4))];
        for (hash, column) in off_screen {
            let mut input = frame_input(RowNumbering::new(0, 10, 0), 1.);
            push_data_rows(&mut input, 0, &[1, 2, 3]);
            input.set_focus(hash, column, Some("Tooltip"));
            let plan = plan_a11y(input);
            assert!(plan.focus().is_none());
            assert_eq!(plan.cell_count(), 3 * COLUMNS.len());
        }
    }

    #[test]
    fn test_a11y_selection_flags() {
        let price = COLUMNS[PRICE].column;
        let plan_with = |unit: Option<SelectionUnit>| {
            let mut input = frame_input(RowNumbering::new(1, 10, 0), 1.);
            input.set_band_clip(Some(PinnedEdge::Top), bounds(100., 30., 400., 20.));
            input.set_selection_unit(unit);
            // Pinned rows have no selection, whatever the input says.
            push_row(&mut input, Some(PinnedEdge::Top), 0, 7, 30., &[PRICE]);
            push_row(&mut input, None, 0, 1, 50., &[PRICE]);
            push_row(&mut input, None, 1, 2, 70., &[]);
            // The focused cell is selectable like the others.
            input.set_focus(key_hash(1), price, None);
            plan_a11y(input)
        };
        let row_flags = |plan: &A11yPlan| -> Vec<Option<bool>> {
            plan.rows()
                .iter()
                .map(|row| {
                    assert_eq!(row_node(plan, row).is_selected(), row.selected());
                    row.selected()
                })
                .collect()
        };
        let cell_flags = |plan: &A11yPlan, row: &A11yRow| -> Vec<Option<bool>> {
            plan.cells(row)
                .iter()
                .map(|cell| {
                    assert_eq!(plan.cell_node(row, cell).is_selected(), cell.selected());
                    cell.selected()
                })
                .collect()
        };
        let focus_flag = |plan: &A11yPlan| {
            let mut node = Node::new(Role::GridCell);
            plan.write_focus(plan.focus().unwrap(), &mut node);
            node.is_selected()
        };

        // Row modes: explicit `false` on unselected data rows.
        let plan = plan_with(Some(SelectionUnit::Row));
        assert_eq!(row_flags(&plan), [None, Some(true), Some(false)]);
        for row in plan.rows() {
            assert!(cell_flags(&plan, row).iter().all(Option::is_none));
        }
        assert_eq!(focus_flag(&plan), None);

        // Cell modes: every data cell carries the flag, rows none.
        let plan = plan_with(Some(SelectionUnit::Cell));
        assert_eq!(row_flags(&plan), [None, None, None]);
        assert_eq!(cell_flags(&plan, &plan.rows()[0]), [None; 5]);
        // Key 1 without its focused price cell, which is selected.
        assert_eq!(cell_flags(&plan, &plan.rows()[1]), [Some(false); 4]);
        assert_eq!(focus_flag(&plan), Some(true));
        assert_eq!(cell_flags(&plan, &plan.rows()[2]), [Some(false); 5]);

        // No selection unit: no flags at all.
        let plan = plan_with(None);
        assert_eq!(row_flags(&plan), [None, None, None]);
        for row in plan.rows() {
            assert!(cell_flags(&plan, row).iter().all(Option::is_none));
        }
        assert_eq!(focus_flag(&plan), None);
    }

    #[test]
    fn test_a11y_key_hash_collision() {
        let mut input = frame_input(RowNumbering::new(1, 10, 0), 1.);
        input.set_band_clip(Some(PinnedEdge::Top), bounds(100., 30., 400., 20.));
        // Display positions 0..3: keys 1, 2, 3. Key 3 is pushed first, with
        // the hash of key 1; the row of key 1 is the earlier and stays.
        let mut push = |position: usize, key: u64, hash: u64| {
            input.push_row(PaintedRow {
                pinned: None,
                position,
                key_hash: hash,
                y: px(50. + ROW_HEIGHT.as_f32() * position as f32),
                height: ROW_HEIGHT,
                selected: false,
            });
            for slot in 0..COLUMNS.len() {
                input.push_cell(slot, &text(key, slot), false);
            }
        };
        push(2, 3, key_hash(1));
        push(0, 1, key_hash(1));
        push(1, 2, key_hash(2));
        // A pinned row with the same hash: another band, another id.
        push_row(&mut input, Some(PinnedEdge::Top), 0, 1, 30., &[]);
        let plan = plan_a11y(input);

        let rows: Vec<(Option<PinnedEdge>, u64, usize)> = plan
            .rows()
            .iter()
            .map(|row| (row.pinned(), row.key_hash(), row.row_index()))
            .collect();
        assert_eq!(
            rows,
            [
                (Some(PinnedEdge::Top), key_hash(1), 2),
                (None, key_hash(1), 3),
                (None, key_hash(2), 4),
            ]
        );
        let kept = &plan.rows()[1];
        let labels: Vec<String> = plan
            .cells(kept)
            .iter()
            .map(|cell| plan.cell_node(kept, cell).label().unwrap().to_owned())
            .collect();
        let expected: Vec<String> = (0..COLUMNS.len()).map(|slot| text(1, slot)).collect();
        assert_eq!(labels, expected);
        assert_eq!(plan.cell_count(), 3 * COLUMNS.len());
    }

    #[test]
    fn test_a11y_identity_survives_resort() {
        let comment = COLUMNS[COMMENT].column;
        let plan_of = |keys: &[u64]| {
            let mut input = frame_input(RowNumbering::new(0, 5, 0), 1.);
            push_data_rows(&mut input, 0, keys);
            input.set_focus(key_hash(3), comment, None);
            plan_a11y(input)
        };
        let ascending = plan_of(&[1, 2, 3, 4, 5]);
        let descending = plan_of(&[5, 4, 3, 2, 1]);

        // The same row ids by key, each at its new index.
        for key in 1..=5u64 {
            let (before, after) = (row_of(&ascending, key), row_of(&descending, key));
            assert_eq!(before.id_name(), after.id_name());
            assert_eq!(before.key_hash(), after.key_hash());
            assert_eq!(before.row_index(), 1 + key as usize);
            assert_eq!(after.row_index(), 7 - key as usize);
            // Synthetic node ids hash the column, not its place.
            let columns = |plan: &A11yPlan, row: &A11yRow| -> Vec<ColumnIx> {
                plan.cells(row).iter().map(|cell| cell.column()).collect()
            };
            assert_eq!(columns(&ascending, before), columns(&descending, after));
        }

        // The focused cell's element id is the same; only its row index moves.
        let focus_id = |plan: &A11yPlan| {
            let focus = plan.focus().unwrap();
            let row = &plan.rows()[focus.row_ix()];
            let mut node = Node::new(Role::GridCell);
            plan.write_focus(focus, &mut node);
            (
                (row.id_name(), row.key_hash()),
                (FOCUS_CELL_ID, focus.column_id()),
                node.row_index_text().map(str::to_owned),
            )
        };
        let (row_before, cell_before, index_before) = focus_id(&ascending);
        let (row_after, cell_after, index_after) = focus_id(&descending);
        assert_eq!(row_before, ("live-grid-row", key_hash(3)));
        assert_eq!(row_before, row_after);
        assert_eq!(cell_before, ("live-grid-focus", "c2"));
        assert_eq!(cell_before, cell_after);
        assert_eq!(index_before.as_deref(), Some("4"));
        assert_eq!(index_after.as_deref(), Some("4"));
    }

    /// S10 in miniature: the visible area of S1 (50 rows × 40 columns, S1
    /// values), 300 frames. Run in release:
    /// `cargo test --release -p gpui-kit-grid --lib a11y -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; run in release"]
    #[cfg(not(target_family = "wasm"))]
    fn test_a11y_plan_timing() {
        use std::{hint::black_box, time::Instant};

        use crate::model::{CellValue, display_text};

        const ROWS: usize = 50;
        const COLUMNS: usize = 40;
        const FRAMES: usize = 300;
        const SIDES: [&str; 2] = ["Buy", "Sell"];
        const TEXTS: [&str; 4] = ["AAPL", "XNAS", "Desk 7", "Partial fill, client 2041"];
        let titles: Vec<String> = (0..COLUMNS).map(|ix| format!("Column {ix}")).collect();
        let value = |row: usize, column: usize| match column {
            0 => CellValue::Int(row as i64),
            1..=20 => CellValue::Float(100. + (row * 31 + column * 7) as f64 / 8.),
            21..=30 => CellValue::Int((row * 100 + column) as i64),
            32 => CellValue::Text(SIDES[row % 2]),
            _ => CellValue::Text(TEXTS[(row + column) % TEXTS.len()]),
        };

        let mut fill = Vec::with_capacity(FRAMES);
        let mut plan_and_nodes = Vec::with_capacity(FRAMES);
        let mut drops = Vec::with_capacity(FRAMES);
        let mut nodes = Vec::with_capacity(ROWS * (COLUMNS + 1));
        let mut buffer = String::new();
        for frame in 0..FRAMES {
            let started = Instant::now();
            let mut input = A11yInput::new(RowNumbering::new(0, 10_000, 0), 2.);
            input.reserve(ROWS, ROWS * COLUMNS);
            input.set_band_clip(None, bounds(0., 30., 3200., 1300.));
            input.set_zone(ColumnZone::Scrolling, px(0.), px(3200.));
            for ix in 0..COLUMNS {
                input.push_column(PaintedColumn {
                    column: ColumnIx::new(ix as u16),
                    view: ViewColumnIx::new(ix as u16),
                    id: "column",
                    title: &titles[ix],
                    zone: ColumnZone::Scrolling,
                    x: px(80. * ix as f32),
                    width: px(80.),
                });
            }
            for line in 0..ROWS {
                let row = frame + line;
                input.push_row(PaintedRow {
                    pinned: None,
                    position: row,
                    key_hash: row as u64,
                    y: px(30. + 26. * line as f32),
                    height: px(26.),
                    selected: line == 3,
                });
                for column in 0..COLUMNS {
                    let text = display_text(value(row, column), &mut buffer);
                    input.push_cell(column, text, false);
                }
            }
            input.set_selection_unit(Some(SelectionUnit::Row));
            input.set_focus(frame as u64 + 7, ColumnIx::new(5), Some("Last trade 10:31"));
            let filled = Instant::now();

            let plan = plan_a11y(input);
            for row in plan.rows() {
                let mut node = Node::new(Role::Row);
                plan.write_row(row, &mut node);
                nodes.push(node);
                for cell in plan.cells(row) {
                    nodes.push(plan.cell_node(row, cell));
                }
            }
            let focus = plan.focus().unwrap();
            let mut node = Node::new(Role::GridCell);
            plan.write_focus(focus, &mut node);
            nodes.push(node);
            let planned = Instant::now();
            black_box(&nodes);
            assert_eq!(plan.cell_count(), ROWS * COLUMNS - 1);
            nodes.clear();
            drop(plan);
            let dropped = Instant::now();

            fill.push(filled - started);
            plan_and_nodes.push(planned - filled);
            drops.push(dropped - planned);
        }
        let report = |name: &str, samples: &mut Vec<std::time::Duration>| {
            samples.sort_unstable();
            let p50 = samples[samples.len() / 2];
            let p99 = samples[samples.len() * 99 / 100];
            let max = samples[samples.len() - 1];
            println!("{name}: p50 {p50:?}, p99 {p99:?}, max {max:?}");
            p99
        };
        report("input fill (display_text, copies)", &mut fill);
        let p99 = report("plan_a11y + nodes (S10)", &mut plan_and_nodes);
        report("drop nodes and plan", &mut drops);
        if cfg!(not(debug_assertions)) {
            assert!(p99.as_secs_f64() <= 0.001, "S10: p99 {p99:?} over 1 ms");
        }
    }

    /// The painted columns of a grouped body at x 100..600, all in the
    /// scrolling zone, in schema order.
    const GROUP_COLUMNS: [PaintedColumn<'static>; 5] = [
        column(0, 0, "Instrument", ColumnZone::Scrolling, 100., 100.),
        column(1, 1, "Side", ColumnZone::Scrolling, 200., 100.),
        column(2, 2, "Volume", ColumnZone::Scrolling, 300., 100.),
        column(3, 3, "Price", ColumnZone::Scrolling, 400., 100.),
        column(4, 4, "Last", ColumnZone::Scrolling, 500., 100.),
    ];
    const INSTRUMENT: usize = 0;
    const SIDE: usize = 1;
    const VOLUME: usize = 2;
    const LAST: usize = 4;

    fn texts() -> A11yTexts {
        A11yTexts {
            rising: "rising".into(),
            falling: "falling".into(),
            expanded: "Expanded".into(),
            collapsed: "Collapsed".into(),
            link: "link".into(),
        }
    }

    /// An input with the data band at y 50..350 and the columns of
    /// `GROUP_COLUMNS`, with the texts.
    fn group_input(numbering: RowNumbering) -> A11yInput {
        let mut input = A11yInput::new(numbering, 1.);
        input.set_band_clip(None, bounds(100., 50., 500., 300.));
        input.set_zone(ColumnZone::Scrolling, px(100.), px(500.));
        for column in GROUP_COLUMNS {
            input.push_column(column);
        }
        input.set_texts(&texts());
        input
    }

    /// The hash the frame gives the caption of the group at `path` (`group_hash`).
    fn group_hash(path: &str) -> u64 {
        crate::state::key_hash(&("live-grid-group", path))
    }

    /// Likewise for its footer (`footer_hash`).
    fn footer_hash(path: &str) -> u64 {
        crate::state::key_hash(&("live-grid-group-footer", path))
    }

    /// Likewise for the totals of a band (`totals_hash`).
    fn totals_hash(edge: PinnedEdge) -> u64 {
        crate::state::key_hash(&("live-grid-totals", edge == PinnedEdge::Top))
    }

    fn caption(level: u16, expanded: bool) -> RowOutline {
        RowOutline {
            content: RowContent::Caption,
            level: Some(level),
            expanded: Some(expanded),
        }
    }

    fn line(level: u16, expanded: Option<bool>) -> RowOutline {
        RowOutline {
            content: RowContent::Data,
            level: Some(level),
            expanded,
        }
    }

    fn footer(level: u16) -> RowOutline {
        RowOutline {
            content: RowContent::Footer,
            level: Some(level),
            expanded: None,
        }
    }

    /// A display line at `position` from the top of the data band, marked
    /// selected: only data rows may carry it.
    fn push_line(input: &mut A11yInput, position: usize, key_hash: u64, outline: RowOutline) {
        input.push_outlined_row(
            PaintedRow {
                pinned: None,
                position,
                key_hash,
                y: px(50. + ROW_HEIGHT.as_f32() * position as f32),
                height: ROW_HEIGHT,
                selected: true,
            },
            outline,
        );
    }

    /// Every cell of the last row, with texts by `key`.
    fn push_cells(input: &mut A11yInput, key: u64) {
        for slot in 0..GROUP_COLUMNS.len() {
            input.push_cell(slot, &format!("{key}:{}", GROUP_COLUMNS[slot].id), true);
        }
    }

    fn cell_nodes(plan: &A11yPlan, row: &A11yRow) -> Vec<Node> {
        plan.cells(row)
            .iter()
            .map(|cell| plan.cell_node(row, cell))
            .collect()
    }

    fn focus_node(plan: &A11yPlan) -> Node {
        let mut node = Node::new(Role::Unknown);
        plan.write_focus(plan.focus().expect("the focus is planned"), &mut node);
        node
    }

    #[test]
    fn test_a11y_levels_and_expanded() {
        let mut input = group_input(RowNumbering::new(0, 6, 0));
        input.set_selection_unit(Some(SelectionUnit::Row));
        push_line(&mut input, 0, group_hash("AAPL"), caption(1, true));
        push_line(&mut input, 1, group_hash("AAPL/Buy"), caption(2, false));
        push_line(&mut input, 2, group_hash("AAPL/Sell"), caption(2, true));
        push_line(&mut input, 3, key_hash(1), line(3, None));
        push_cells(&mut input, 1);
        push_line(&mut input, 4, key_hash(2), line(3, None));
        push_cells(&mut input, 2);
        push_line(&mut input, 5, footer_hash("AAPL/Sell"), footer(2));
        let plan = plan_a11y(input);

        let rows = plan.rows();
        let nodes: Vec<Node> = rows.iter().map(|row| row_node(&plan, row)).collect();
        let levels: Vec<Option<usize>> = nodes.iter().map(Node::level).collect();
        assert_eq!(levels, [1, 2, 2, 3, 3, 2].map(Some));
        assert!(
            rows.iter()
                .zip(&nodes)
                .all(|(row, node)| row.level() == node.level()
                    && row.expanded() == node.is_expanded())
        );
        let expanded: Vec<Option<bool>> = nodes.iter().map(Node::is_expanded).collect();
        assert_eq!(
            expanded,
            [Some(true), Some(false), Some(true), None, None, None]
        );
        // The action opposite to the state, and only on captions.
        let actions: Vec<(bool, bool)> = nodes
            .iter()
            .map(|node| {
                (
                    node.supports_action(Action::Expand),
                    node.supports_action(Action::Collapse),
                )
            })
            .collect();
        assert_eq!(
            actions,
            [
                (false, true),
                (true, false),
                (false, true),
                (false, false),
                (false, false),
                (false, false),
            ]
        );
        let disclosure: Vec<Option<Action>> = rows.iter().map(A11yRow::disclosure_action).collect();
        assert_eq!(
            disclosure,
            [
                Some(Action::Collapse),
                Some(Action::Expand),
                Some(Action::Collapse),
                None,
                None,
                None,
            ]
        );
        // Lines are numbered by display position, whatever they are.
        let indices: Vec<usize> = rows.iter().map(A11yRow::row_index).collect();
        assert_eq!(indices, [2, 3, 4, 5, 6, 7]);
        let names: Vec<&str> = rows.iter().map(A11yRow::id_name).collect();
        assert_eq!(
            names,
            [
                "live-grid-group-row",
                "live-grid-group-row",
                "live-grid-group-row",
                "live-grid-row",
                "live-grid-row",
                "live-grid-group-footer-row",
            ]
        );
        let contents: Vec<RowContent> = rows.iter().map(A11yRow::content).collect();
        assert_eq!(contents[0], RowContent::Caption);
        assert_eq!(contents[3], RowContent::Data);
        assert_eq!(contents[5], RowContent::Footer);
        // Only data rows are selectable.
        let selected: Vec<Option<bool>> = nodes.iter().map(Node::is_selected).collect();
        assert_eq!(selected, [None, None, None, Some(true), Some(true), None]);

        // A flat view: no level, no state, no action.
        let mut input = frame_input(RowNumbering::new(0, 10, 0), 1.);
        input.set_texts(&texts());
        push_data_rows(&mut input, 0, &[1, 2]);
        let plan = plan_a11y(input);
        for row in plan.rows() {
            let node = row_node(&plan, row);
            assert_eq!((node.level(), node.is_expanded()), (None, None));
            assert!(!node.supports_action(Action::Expand));
            assert!(!node.supports_action(Action::Collapse));
            assert_eq!(row.disclosure_action(), None);
            for node in cell_nodes(&plan, row) {
                assert_eq!(node.column_span(), None);
            }
        }
    }

    #[test]
    fn test_a11y_caption_cells() {
        let long = "Instrument: AAPL, Apple Inc. common stock, Nasdaq Global Select Market (120)";
        let mut input = group_input(RowNumbering::new(0, 3, 0));
        input.set_selection_unit(Some(SelectionUnit::Cell));
        // Aggregates in columns 3 and 5: the caption lane runs from the
        // first column to the first aggregate, x 100..300.
        push_line(&mut input, 0, group_hash("AAPL"), caption(1, true));
        input.push_caption(
            long,
            ViewColumnIx::new(VOLUME as u16),
            bounds(100., 50., 200., 20.),
        );
        input.push_aggregate_cell(VOLUME, "1 234", "Sum");
        input.push_aggregate_cell(LAST, "101.5", "Average");
        // A custom aggregate without a name, over an empty value.
        push_line(&mut input, 1, group_hash("MSFT"), caption(1, false));
        input.push_caption(
            "Instrument: MSFT (3)",
            ViewColumnIx::new(VOLUME as u16),
            bounds(100., 70., 200., 20.),
        );
        input.push_aggregate_cell(VOLUME, "", "");
        input.push_aggregate_cell(LAST, "99", "Average");
        let plan = plan_a11y(input);

        let row = &plan.rows()[0];
        let nodes = cell_nodes(&plan, row);
        // Columns 2 and 4 have no aggregate and no cell.
        let indices: Vec<usize> = nodes
            .iter()
            .map(|node| node.column_index().unwrap())
            .collect();
        assert_eq!(indices, [1, 3, 5]);
        for node in &nodes {
            assert_eq!(node.role(), Role::GridCell);
            assert_eq!(node.row_index_text(), Some("2"));
            assert_eq!(
                node.column_index_text(),
                node.column_index().map(|ix| ix.to_string()).as_deref()
            );
            // Never selected, even in a cell selection.
            assert_eq!(node.is_selected(), None);
            assert_eq!(node.live(), None);
        }
        let cells = plan.cells(row);
        assert!(cells[0].is_caption());
        assert!(!cells[1].is_caption() && !cells[2].is_caption());
        // Synthetic node keys are unique in the row.
        assert_eq!(
            cells.iter().map(A11yCell::column).collect::<Vec<_>>(),
            [CAPTION_COLUMN, ColumnIx::new(2), ColumnIx::new(4)]
        );

        // The full caption, not the one the screen cuts.
        let caption_node = &nodes[0];
        assert_eq!(caption_node.label(), Some(long));
        assert_eq!(caption_node.description(), Some("Expanded"));
        assert_eq!(caption_node.column_span(), Some(2));
        assert_eq!(
            caption_node.bounds(),
            Some(Rect {
                x0: 100.,
                y0: 50.,
                x1: 300.,
                y1: 70.,
            })
        );
        assert_eq!(nodes[1].label(), Some("1 234"));
        assert_eq!(nodes[1].description(), Some("Sum, Volume"));
        assert_eq!(nodes[1].column_span(), None);
        assert_eq!(nodes[2].label(), Some("101.5"));
        assert_eq!(nodes[2].description(), Some("Average, Last"));

        let nodes = cell_nodes(&plan, &plan.rows()[1]);
        assert_eq!(nodes[0].description(), Some("Collapsed"));
        assert_eq!(nodes[1].label(), Some(""));
        assert_eq!(nodes[1].description(), Some("Volume"));
        assert_eq!(nodes[2].description(), Some("Average, Last"));
    }

    #[test]
    fn test_a11y_aggregate_in_first_column() {
        let mut input = group_input(RowNumbering::new(0, 1, 0));
        push_line(&mut input, 0, group_hash("AAPL"), caption(1, true));
        // The frame may push the first column's aggregate before the caption.
        input.push_aggregate_cell(INSTRUMENT, "120", "Count");
        input.set_trend(ChangeDirection::Up);
        input.push_caption(
            "Instrument: AAPL (120)",
            ViewColumnIx::new(0),
            bounds(100., 50., 100., 20.),
        );
        input.push_aggregate_cell(VOLUME, "1 234", "Sum");
        let plan = plan_a11y(input);

        let row = &plan.rows()[0];
        let nodes = cell_nodes(&plan, row);
        // One cell in column 1: the caption, its span one column.
        let indices: Vec<usize> = nodes
            .iter()
            .map(|node| node.column_index().unwrap())
            .collect();
        assert_eq!(indices, [1, 3]);
        assert!(plan.cells(row)[0].is_caption());
        assert_eq!(nodes[0].column_span(), Some(1));
        assert_eq!(nodes[0].label(), Some("Instrument: AAPL (120)"));
        assert_eq!(
            nodes[0].description(),
            Some("Expanded. Count, Instrument: 120. rising")
        );
        assert_eq!(nodes[1].description(), Some("Sum, Volume"));
    }

    #[test]
    fn test_a11y_totals_and_footer_rows() {
        let numbering = RowNumbering::new(0, 4, 2).with_totals(Some(PinnedEdge::Bottom));
        assert_eq!(numbering.row_count(), 8);
        assert_eq!(numbering.totals_row_index(), Some(8));
        assert_eq!(numbering.row_index(Some(PinnedEdge::Bottom), 0), 6);
        assert_eq!(RowNumbering::new(0, 4, 2).totals_row_index(), None);

        let plan_with = |unit: SelectionUnit| {
            let mut input = group_input(numbering);
            input.set_band_clip(Some(PinnedEdge::Bottom), bounds(100., 350., 500., 60.));
            input.set_selection_unit(Some(unit));
            // Totals first, though they come last; their position is ignored.
            let totals = totals_hash(PinnedEdge::Bottom);
            input.push_outlined_row(
                PaintedRow {
                    pinned: Some(PinnedEdge::Bottom),
                    position: 0,
                    key_hash: totals,
                    y: px(390.),
                    height: ROW_HEIGHT,
                    selected: true,
                },
                RowOutline {
                    content: RowContent::Totals,
                    ..RowOutline::default()
                },
            );
            input.set_row_label("Total");
            input.push_cell(INSTRUMENT, "Total", true);
            input.push_aggregate_cell(VOLUME, "5 000", "Sum");
            // The second application row hashes as the totals do: another
            // id name, so both stay.
            for (position, hash) in [key_hash(900), totals].into_iter().enumerate() {
                input.push_row(PaintedRow {
                    pinned: Some(PinnedEdge::Bottom),
                    position,
                    key_hash: hash,
                    y: px(350. + ROW_HEIGHT.as_f32() * position as f32),
                    height: ROW_HEIGHT,
                    selected: true,
                });
                push_cells(&mut input, 900 + position as u64);
            }
            push_line(&mut input, 0, group_hash("AAPL"), caption(1, true));
            input.push_caption(
                "Instrument: AAPL (2)",
                ViewColumnIx::new(VOLUME as u16),
                bounds(100., 50., 200., 20.),
            );
            input.push_aggregate_cell(VOLUME, "5 000", "Sum");
            for (position, key) in [(1, 1), (2, 2)] {
                push_line(&mut input, position, key_hash(key), line(2, None));
                push_cells(&mut input, key);
            }
            push_line(&mut input, 3, footer_hash("AAPL"), footer(1));
            input.push_aggregate_cell(VOLUME, "5 000", "Sum");
            plan_a11y(input)
        };

        let plan = plan_with(SelectionUnit::Row);
        let rows = plan.rows();
        let indices: Vec<usize> = rows.iter().map(A11yRow::row_index).collect();
        assert_eq!(indices, [2, 3, 4, 5, 6, 7, 8]);
        let names: Vec<&str> = rows.iter().map(A11yRow::id_name).collect();
        assert_eq!(
            names,
            [
                "live-grid-group-row",
                "live-grid-row",
                "live-grid-row",
                "live-grid-group-footer-row",
                "live-grid-pinned-bottom",
                "live-grid-pinned-bottom",
                "live-grid-totals-row",
            ]
        );
        assert_eq!(rows[6].key_hash(), rows[5].key_hash());
        assert_eq!(rows[6].pinned(), Some(PinnedEdge::Bottom));
        let selected: Vec<Option<bool>> = rows.iter().map(A11yRow::selected).collect();
        assert_eq!(
            selected,
            [None, Some(true), Some(true), None, None, None, None]
        );

        let footer_row = row_node(&plan, &rows[3]);
        assert_eq!(footer_row.level(), Some(1));
        assert_eq!(footer_row.is_expanded(), None);
        let footer_cells = cell_nodes(&plan, &rows[3]);
        assert_eq!(footer_cells.len(), 1);
        assert_eq!(footer_cells[0].column_index(), Some(3));
        assert_eq!(footer_cells[0].description(), Some("Sum, Volume"));

        let totals_row = row_node(&plan, &rows[6]);
        // The totals row is named; other rows are not.
        assert_eq!(totals_row.label(), Some("Total"));
        assert_eq!(footer_row.label(), None);
        assert_eq!(totals_row.row_index(), Some(8));
        assert_eq!(totals_row.level(), None);
        assert_eq!(totals_row.is_selected(), None);
        let totals_cells = cell_nodes(&plan, &rows[6]);
        assert_eq!(totals_cells[0].label(), Some("Total"));
        assert_eq!(totals_cells[0].description(), Some("Instrument"));
        assert_eq!(totals_cells[0].column_index(), Some(1));
        assert_eq!(totals_cells[1].label(), Some("5 000"));
        assert_eq!(totals_cells[1].description(), Some("Sum, Volume"));
        assert_eq!(totals_cells[1].row_index_text(), Some("8"));

        // A cell selection flags data cells alone.
        let plan = plan_with(SelectionUnit::Cell);
        for row in plan.rows() {
            let data = row.pinned().is_none() && row.content() == RowContent::Data;
            for node in cell_nodes(&plan, row) {
                assert_eq!(node.is_selected(), data.then_some(true));
            }
        }

        // Totals at the top come first, before the application rows.
        let numbering = RowNumbering::new(2, 3, 0).with_totals(Some(PinnedEdge::Top));
        assert_eq!(numbering.row_count(), 7);
        let mut input = group_input(numbering);
        input.set_band_clip(Some(PinnedEdge::Top), bounds(100., 0., 500., 60.));
        for position in 0..2 {
            input.push_row(PaintedRow {
                pinned: Some(PinnedEdge::Top),
                position,
                key_hash: key_hash(800 + position as u64),
                y: px(20. + ROW_HEIGHT.as_f32() * position as f32),
                height: ROW_HEIGHT,
                selected: false,
            });
        }
        input.push_outlined_row(
            PaintedRow {
                pinned: Some(PinnedEdge::Top),
                position: 2,
                key_hash: totals_hash(PinnedEdge::Top),
                y: px(0.),
                height: ROW_HEIGHT,
                selected: false,
            },
            RowOutline {
                content: RowContent::Totals,
                ..RowOutline::default()
            },
        );
        push_line(&mut input, 0, key_hash(1), RowOutline::default());
        let plan = plan_a11y(input);
        let rows: Vec<(&str, usize)> = plan
            .rows()
            .iter()
            .map(|row| (row.id_name(), row.row_index()))
            .collect();
        assert_eq!(
            rows,
            [
                ("live-grid-totals-row", 2),
                ("live-grid-pinned-top", 3),
                ("live-grid-pinned-top", 4),
                ("live-grid-row", 5),
            ]
        );
    }

    #[test]
    fn test_a11y_caption_focus() {
        let caption_text = "Instrument: AAPL (120)";
        let plan_with = |expanded: bool, focused: u64, tooltip: Option<&str>| {
            let mut input = group_input(RowNumbering::new(0, 3, 0));
            push_line(&mut input, 0, group_hash("AAPL"), caption(1, expanded));
            input.push_caption(
                caption_text,
                ViewColumnIx::new(VOLUME as u16),
                bounds(100., 50., 200., 20.),
            );
            input.push_aggregate_cell(VOLUME, "1 234", "Sum");
            // A data row with the caption's hash: not a caption, not focused.
            push_line(&mut input, 1, group_hash("AAPL"), line(2, None));
            push_cells(&mut input, 1);
            push_line(&mut input, 2, key_hash(2), line(2, None));
            push_cells(&mut input, 2);
            input.set_caption_focus(focused, tooltip);
            plan_a11y(input)
        };

        // The default tooltip repeats the caption: the state alone.
        let plan = plan_with(true, group_hash("AAPL"), Some(caption_text));
        let focus = plan.focus().expect("the caption is painted");
        assert_eq!(focus.column_id(), CAPTION_FOCUS_ID);
        assert_eq!(
            (FOCUS_CELL_ID, focus.column_id()),
            ("live-grid-focus", "group")
        );
        assert!(focus.cell().is_caption());
        assert_eq!(focus.cell().bounds(), bounds(100., 50., 200., 20.));
        let row = &plan.rows()[focus.row_ix()];
        assert_eq!(
            (row.id_name(), row.key_hash()),
            ("live-grid-group-row", group_hash("AAPL"))
        );
        // No synthetic twin: the caption row keeps its aggregate cell alone.
        assert_eq!(plan.cells(row).len(), 1);
        assert!(!plan.cells(row)[0].is_caption());
        assert_eq!(plan.cell_count(), 1 + 2 * GROUP_COLUMNS.len());
        assert_eq!(plan.build_nodes(), 3 + plan.cell_count() + 1);
        let node = focus_node(&plan);
        assert_eq!(node.role(), Role::GridCell);
        assert_eq!(node.label(), Some(caption_text));
        assert_eq!(node.description(), Some("Expanded"));
        assert_eq!(node.column_index(), Some(1));
        assert_eq!(node.column_index_text(), Some("1"));
        assert_eq!(node.column_span(), Some(2));
        assert_eq!(node.row_index_text(), Some("2"));
        assert_eq!(node.is_selected(), None);

        // Collapsed, with a tooltip of its own: the same element (row id and
        // child name), the other state, then the tooltip.
        let plan = plan_with(false, group_hash("AAPL"), Some("Last trade 10:31"));
        let focus = plan.focus().unwrap();
        let row = &plan.rows()[focus.row_ix()];
        assert_eq!(
            (row.id_name(), row.key_hash(), focus.column_id()),
            ("live-grid-group-row", group_hash("AAPL"), "group")
        );
        assert_eq!(
            focus_node(&plan).description(),
            Some("Collapsed. Last trade 10:31")
        );
        assert_eq!(row_node(&plan, row).is_expanded(), Some(false));

        // A caption that is not painted has no focus; every painted caption
        // stays synthetic.
        let plan = plan_with(true, group_hash("MSFT"), None);
        assert!(plan.focus().is_none());
        assert_eq!(plan.cell_count(), 2 + 2 * GROUP_COLUMNS.len());
        assert!(plan.cells(&plan.rows()[0])[0].is_caption());
    }

    #[test]
    fn test_a11y_trend_description() {
        let price = COLUMNS[PRICE].column;
        const SIZE: usize = 2;
        let mut input = frame_input(RowNumbering::new(0, 10, 0), 1.);
        input.set_texts(&texts());
        input.push_row(PaintedRow {
            pinned: None,
            position: 0,
            key_hash: key_hash(1),
            y: px(50.),
            height: ROW_HEIGHT,
            selected: false,
        });
        for slot in 0..COLUMNS.len() {
            input.push_cell(slot, &text(1, slot), false);
            match slot {
                PRICE => input.set_trend(ChangeDirection::Up),
                SIZE => input.set_trend(ChangeDirection::Down),
                _ => {}
            }
        }
        push_data_rows(&mut input, 1, &[2]);
        input.set_focus(key_hash(1), price, Some("Last trade 10:31"));
        let plan = plan_a11y(input);

        // The focused price: the column, the direction, the tooltip.
        assert_eq!(
            focus_node(&plan).description(),
            Some("Price. rising. Last trade 10:31")
        );
        let descriptions = |row: &A11yRow| -> Vec<String> {
            cell_nodes(&plan, row)
                .iter()
                .map(|node| node.description().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            descriptions(&plan.rows()[0]),
            ["Side", "Size. falling", "Comment", "Total"]
        );
        // Without an arrow: the column titles alone.
        assert_eq!(
            descriptions(&plan.rows()[1]),
            ["Side", "Price", "Size", "Comment", "Total"]
        );

        // An aggregate's arrow follows its name and column.
        let mut input = group_input(RowNumbering::new(0, 1, 0));
        push_line(&mut input, 0, footer_hash("AAPL"), footer(1));
        input.push_aggregate_cell(VOLUME, "1 234", "Sum");
        input.set_trend(ChangeDirection::Down);
        let plan = plan_a11y(input);
        let nodes = cell_nodes(&plan, &plan.rows()[0]);
        assert_eq!(nodes[0].description(), Some("Sum, Volume. falling"));

        // Without texts, no words to add.
        let mut input = frame_input(RowNumbering::new(0, 10, 0), 1.);
        push_data_rows(&mut input, 0, &[1]);
        input.set_trend(ChangeDirection::Up);
        let plan = plan_a11y(input);
        let nodes = cell_nodes(&plan, &plan.rows()[0]);
        assert_eq!(nodes[COLUMNS.len() - 1].description(), Some("Total"));
    }

    #[test]
    fn test_a11y_tree_branch() {
        let instrument = GROUP_COLUMNS[INSTRUMENT].column;
        let mut input = group_input(RowNumbering::new(0, 3, 0));
        input.set_tree_column(instrument);
        input.set_selection_unit(Some(SelectionUnit::Row));
        // An expanded node, its leaf, and a collapsed node.
        push_line(&mut input, 0, key_hash(1), line(1, Some(true)));
        push_cells(&mut input, 1);
        push_line(&mut input, 1, key_hash(2), line(2, None));
        push_cells(&mut input, 2);
        push_line(&mut input, 2, key_hash(3), line(2, Some(false)));
        push_cells(&mut input, 3);
        input.set_focus(key_hash(3), instrument, None);
        let plan = plan_a11y(input);

        let rows = plan.rows();
        let nodes: Vec<Node> = rows.iter().map(|row| row_node(&plan, row)).collect();
        // Tree rows are data rows: their own ids, selectable.
        for (row, node) in rows.iter().zip(&nodes) {
            assert_eq!(row.id_name(), "live-grid-row");
            assert_eq!(node.is_selected(), Some(true));
        }
        assert_eq!(nodes[0].level(), Some(1));
        assert_eq!(nodes[0].is_expanded(), Some(true));
        assert!(nodes[0].supports_action(Action::Collapse));
        let cells = cell_nodes(&plan, &rows[0]);
        assert_eq!(
            cells[INSTRUMENT].description(),
            Some("Instrument. Expanded")
        );
        assert_eq!(cells[SIDE].description(), Some("Side"));

        // A leaf: a level, no state.
        assert_eq!(nodes[1].level(), Some(2));
        assert_eq!(nodes[1].is_expanded(), None);
        assert!(!nodes[1].supports_action(Action::Expand));
        assert!(!nodes[1].supports_action(Action::Collapse));
        let cells = cell_nodes(&plan, &rows[1]);
        assert_eq!(cells[INSTRUMENT].description(), Some("Instrument"));

        // The focused cell of a collapsed node tells its state too.
        assert_eq!(nodes[2].is_expanded(), Some(false));
        assert!(nodes[2].supports_action(Action::Expand));
        let focus = plan.focus().unwrap();
        assert_eq!(focus.row_ix(), 2);
        assert_eq!(focus.column_id(), "c0");
        assert_eq!(
            focus_node(&plan).description(),
            Some("Instrument. Collapsed")
        );
    }

    #[test]
    fn test_a11y_real_cells() {
        let price = COLUMNS[PRICE].column;
        let mut input = frame_input(RowNumbering::new(0, 10, 0), 1.);
        // Row 1 hosts its price cell; row 2 too, and the cell has the focus.
        for (line, key) in [1u64, 2, 3].into_iter().enumerate() {
            input.push_row(PaintedRow {
                pinned: None,
                position: line,
                key_hash: key_hash(key),
                y: px(50. + ROW_HEIGHT.as_f32() * line as f32),
                height: ROW_HEIGHT,
                selected: false,
            });
            for slot in 0..COLUMNS.len() {
                input.push_cell(slot, &text(key, slot), false);
                if slot == PRICE && key != 3 {
                    input.set_real_cell();
                }
            }
        }
        input.set_focus(key_hash(2), price, None);
        let plan = plan_a11y(input);
        let cells = |key| -> Vec<ColumnIx> {
            plan.cells(row_of(&plan, key))
                .iter()
                .map(A11yCell::column)
                .collect()
        };
        assert!(!cells(1).contains(&price));
        assert_eq!(cells(1).len(), COLUMNS.len() - 1);
        assert_eq!(cells(3).len(), COLUMNS.len());
        // The focused real cell is planned for its host to claim.
        assert_eq!(plan.focus().map(|focus| focus.cell().column()), Some(price));
        assert!(!cells(2).contains(&price));
        assert_eq!(data_row_id_name(None), "live-grid-row");
        assert_eq!(
            data_row_id_name(Some(PinnedEdge::Bottom)),
            "live-grid-pinned-bottom"
        );
    }

    #[test]
    fn test_a11y_root_role() {
        assert_eq!(root_role(true), Role::TreeGrid);
        assert_eq!(root_role(false), Role::Grid);
    }

    /// S10b in miniature: 54 display lines, 10 of them group captions on
    /// two levels with three aggregates each, 40 columns, the focus on a
    /// caption, 300 frames. Run in release:
    /// `cargo test --release -p gpui-kit-grid --lib a11y -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; run in release"]
    #[cfg(not(target_family = "wasm"))]
    fn test_a11y_grouped_plan_timing() {
        use std::{fmt::Write as _, hint::black_box, time::Instant};

        use crate::model::{CellValue, display_text};

        const LINES: usize = 54;
        const COLUMNS: usize = 40;
        const FRAMES: usize = 300;
        const AGGREGATES: [(usize, &str); 3] = [(10, "Sum"), (20, "Average"), (30, "Max")];
        let titles: Vec<String> = (0..COLUMNS).map(|ix| format!("Column {ix}")).collect();
        let texts = texts();
        let mut buffer = String::new();
        let mut caption = String::new();

        let mut samples = Vec::with_capacity(FRAMES);
        let mut nodes = Vec::with_capacity(LINES * (COLUMNS + 1));
        for frame in 0..FRAMES {
            let mut input = A11yInput::new(RowNumbering::new(0, 10_000, 0), 2.);
            input.reserve(LINES, LINES * COLUMNS);
            input.set_band_clip(None, bounds(0., 30., 3200., 1300.));
            input.set_zone(ColumnZone::Scrolling, px(0.), px(3200.));
            input.set_texts(&texts);
            for ix in 0..COLUMNS {
                input.push_column(PaintedColumn {
                    column: ColumnIx::new(ix as u16),
                    view: ViewColumnIx::new(ix as u16),
                    id: "column",
                    title: &titles[ix],
                    zone: ColumnZone::Scrolling,
                    x: px(80. * ix as f32),
                    width: px(80.),
                });
            }
            // A caption on every fifth line of the first 50: levels 1 and 2.
            for line in 0..LINES {
                let row = PaintedRow {
                    pinned: None,
                    position: frame + line,
                    key_hash: (frame + line) as u64,
                    y: px(30. + 26. * line as f32),
                    height: px(26.),
                    selected: false,
                };
                if line % 5 == 0 && line < 50 {
                    let level = if line % 25 == 0 { 1 } else { 2 };
                    input.push_outlined_row(
                        row,
                        RowOutline {
                            content: RowContent::Caption,
                            level: Some(level),
                            expanded: Some(line % 10 == 0),
                        },
                    );
                    caption.clear();
                    let _ = write!(caption, "Instrument: SYM{line} ({})", line * 7);
                    input.push_caption(&caption, ViewColumnIx::new(10), bounds(0., 0., 800., 26.));
                    for (column, name) in AGGREGATES {
                        let value = CellValue::Float((line * column) as f64 / 8.);
                        input.push_aggregate_cell(column, display_text(value, &mut buffer), name);
                    }
                } else {
                    input.push_outlined_row(
                        row,
                        RowOutline {
                            content: RowContent::Data,
                            level: Some(3),
                            expanded: None,
                        },
                    );
                    for column in 0..COLUMNS {
                        let value = CellValue::Float(100. + (line * 31 + column * 7) as f64 / 8.);
                        input.push_cell(column, display_text(value, &mut buffer), false);
                    }
                }
            }
            input.set_caption_focus((frame + 5) as u64, Some("Instrument: SYM5 (35)"));

            let started = Instant::now();
            let plan = plan_a11y(input);
            for row in plan.rows() {
                let mut node = Node::new(Role::Row);
                plan.write_row(row, &mut node);
                nodes.push(node);
                for cell in plan.cells(row) {
                    nodes.push(plan.cell_node(row, cell));
                }
            }
            let focus = plan.focus().expect("the focused caption is painted");
            let mut node = Node::new(Role::GridCell);
            plan.write_focus(focus, &mut node);
            nodes.push(node);
            samples.push(started.elapsed());
            black_box(&nodes);
            assert_eq!(plan.cell_count(), 44 * COLUMNS + 10 * 4 - 1);
            nodes.clear();
        }
        samples.sort_unstable();
        let p50 = samples[FRAMES / 2];
        let p99 = samples[FRAMES * 99 / 100];
        let max = samples[FRAMES - 1];
        println!("plan_a11y + nodes, grouped (S10b): p50 {p50:?}, p99 {p99:?}, max {max:?}");
        if cfg!(not(debug_assertions)) {
            assert!(p99.as_secs_f64() <= 0.001, "S10b: p99 {p99:?} over 1 ms");
        }
    }
}
