//! Saved grid layouts: the data, its text format, how a saved layout meets
//! the current schema, and named layouts per view.
//!
//! No GPUI types: widths are logical pixels as `f32` and identifiers are
//! strings, so the module can move to `gpui-base` with `model` and `engine`.
//! The grid applies a layout and validates the result; nothing here performs
//! I/O.

use std::{collections::BTreeMap, fmt};

use rustc_hash::FxHashSet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    engine::{ExpandDepth, SortDirection},
    model::{ColumnId, OwnedValue},
    state::ColumnZone,
};

/// A saved grid layout: the columns by [`ColumnId`] in display order, each
/// with its width, visibility and zone, the sort, and the grouping with the
/// expansion of its groups.
///
/// Plain data without a GPUI entity and without the application's column
/// type, so one value fits every grid of the same view. Serialize it with
/// serde or a [`LayoutCodec`]. The grid resolves it against its schema when
/// it is applied: unknown columns are ignored and new ones are added.
///
/// The format is version 1 ([`GridLayout::VERSION`]). Defaults are not
/// written: no `view_id` when there is none, no `visible` for a visible
/// column, no `frozen` for a scrolling one, no `sort` or `group_by` when it
/// is empty, and no `expansion` when every group is expanded and none is
/// toggled. Reading ignores unknown fields, so a newer build can add an
/// optional field without a new version, as `group_by` and `expansion` were
/// added; an older build drops that field when it saves again.
///
/// Deserializing with serde keeps the version the data carries. The codec
/// refuses a version this build does not read, and so does the grid when it
/// applies a layout.
///
/// ```
/// use gpui_kit_grid::{
///     ColumnZone, GridLayout, JsonLayoutCodec, LayoutCodec, LayoutColumn, LayoutSort,
///     SortDirection,
/// };
///
/// let layout = GridLayout::new()
///     .with_view_id("trades")
///     .with_column(LayoutColumn::new("id", 80.).with_zone(ColumnZone::FrozenLeft))
///     .with_column(LayoutColumn::new("price", 100.))
///     .with_column(LayoutColumn::new("fee", 90.).with_visible(false))
///     .with_sort([LayoutSort::new("price", SortDirection::Descending)]);
///
/// let codec = JsonLayoutCodec::default();
/// let text = codec.encode(&layout)?;
/// assert_eq!(
///     text,
///     concat!(
///         r#"{"version":1,"view_id":"trades","columns":["#,
///         r#"{"id":"id","width":80.0,"frozen":"left"},"#,
///         r#"{"id":"price","width":100.0},"#,
///         r#"{"id":"fee","width":90.0,"visible":false}],"#,
///         r#""sort":[{"column":"price","direction":"descending"}]}"#,
///     )
/// );
/// assert_eq!(codec.decode(&text)?, layout);
/// # Ok::<(), gpui_kit_grid::LayoutError>(())
/// ```
///
/// # Grouping and expansion
///
/// `group_by` names the grouping levels by column, the top level first. A
/// layout without levels has no grouping: applying it removes the grid's
/// grouping. `expansion` keeps the default depth and the groups the user
/// toggled against it, each as its key values from the top level down; a
/// value is never an index, and the levels of the same layout name its
/// columns.
///
/// ```
/// use gpui_kit_grid::{
///     ExpandDepth, GridLayout, JsonLayoutCodec, LayoutCodec, LayoutExpansion, LayoutGroup,
///     OwnedValue, SortDirection,
/// };
///
/// let aapl = OwnedValue::Text("AAPL".into());
/// let layout = GridLayout::new()
///     .with_group_by([
///         LayoutGroup::new("instrument", SortDirection::Ascending),
///         LayoutGroup::new("side", SortDirection::Descending),
///     ])
///     .with_expansion(LayoutExpansion::new(ExpandDepth::All).with_toggled([aapl]));
///
/// let codec = JsonLayoutCodec::default();
/// let text = codec.encode(&layout)?;
/// assert_eq!(
///     text,
///     concat!(
///         r#"{"version":1,"columns":[],"group_by":["#,
///         r#"{"column":"instrument","direction":"ascending"},"#,
///         r#"{"column":"side","direction":"descending"}],"#,
///         r#""expansion":{"depth":"all","toggled":[["AAPL"]]}}"#,
///     )
/// );
/// assert_eq!(codec.decode(&text)?, layout);
/// # Ok::<(), gpui_kit_grid::LayoutError>(())
/// ```
///
/// # Saving a grid in a dock panel
///
/// The grid does no I/O and does not depend on the dock. A panel hands its
/// grid's layout to the dock in `Panel::dump`, and the panel's builder in the
/// registry (`register_panel`) gives it back to `with_layout`.
/// `LiveGridState::layout` reads state only, so a panel in a tab that was
/// never shown saves its layout too. The complete, tested version is
/// `crates/grid/tests/dock.rs`.
///
/// ```ignore
/// const TRADES: &str = "trades";
///
/// impl TradesPanel {
///     fn new(layout: Option<GridLayout>, cx: &mut Context<Self>) -> Self {
///         let grid = cx.new(|cx| {
///             let grid = LiveGridState::new_standalone(schema(), Trades, StoreOptions::default(), cx)
///                 .with_view_id(TRADES);
///             match layout {
///                 Some(layout) => grid.with_layout(layout),
///                 None => grid,
///             }
///         });
///         Self { grid, focus_handle: cx.focus_handle() }
///     }
/// }
///
/// impl BasePanel for TradesPanel {
///     fn panel_name(&self) -> &'static str {
///         TRADES
///     }
///
///     /// The dock pulls the layout when it saves; the grid needs no render.
///     fn dump(&self, cx: &App) -> PanelState {
///         let mut state = PanelState::new(TRADES);
///         let layout = self.grid.read(cx).layout();
///         state.info = PanelInfo::panel(serde_json::json!({ "grid": layout }));
///         state
///     }
/// }
///
/// // At startup, before `DockArea::load`. A bad layout opens the panel with
/// // the schema's layout instead of failing the load; `with_layout` ignores
/// // a layout of a newer version.
/// register_panel(cx, TRADES, |context, _window, cx| {
///     let layout = match context.info() {
///         PanelInfo::Panel(value) => value
///             .get("grid")
///             .and_then(|grid| serde_json::from_value::<GridLayout>(grid.clone()).ok()),
///         _ => None,
///     };
///     panel_handle(cx.new(|cx| TradesPanel::new(layout, cx)))
/// });
/// ```
///
/// The dock saves on its own edits only (`DockEvent::LayoutChanged`), so
/// changes inside a panel must trigger a save too. They are the grid's
/// `LayoutChange`, `SortChange`, `GroupChange` and `ExpansionChange` events,
/// which come at once, once per finished gesture or call. The application
/// delays the write (0.5–1 s), since groups are expanded and collapsed as
/// often as a chevron is clicked, and also writes when it quits.
///
/// ```ignore
/// cx.subscribe(&grid, |app, _, event: &LiveGridEvent<u64, TradeColumn>, cx| {
///     if matches!(
///         event,
///         LiveGridEvent::LayoutChange { .. }
///             | LiveGridEvent::SortChange { .. }
///             | LiveGridEvent::GroupChange { .. }
///             | LiveGridEvent::ExpansionChange { .. }
///     ) {
///         app.schedule_save(cx);
///     }
/// })
/// .detach();
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GridLayout {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    view_id: Option<String>,
    columns: Vec<LayoutColumn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sort: Vec<LayoutSort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    group_by: Vec<LayoutGroup>,
    #[serde(default, skip_serializing_if = "LayoutExpansion::is_unwritten")]
    expansion: LayoutExpansion,
}

impl GridLayout {
    /// The format version this build writes and the newest it reads.
    pub const VERSION: u32 = 1;

    /// An empty layout of the current version, without a view id.
    pub fn new() -> Self {
        Self {
            version: Self::VERSION,
            view_id: None,
            columns: Vec::new(),
            sort: Vec::new(),
            group_by: Vec::new(),
            expansion: LayoutExpansion::default(),
        }
    }

    /// Names the view the layout belongs to ("trades"). A grid with another
    /// view id refuses the layout.
    pub fn with_view_id(mut self, view_id: impl Into<String>) -> Self {
        self.view_id = Some(view_id.into());
        self
    }

    /// Appends a column; the order of columns is the display order.
    pub fn with_column(mut self, column: LayoutColumn) -> Self {
        self.columns.push(column);
        self
    }

    /// Replaces the sort; the first key sorts first.
    pub fn with_sort(mut self, sort: impl IntoIterator<Item = LayoutSort>) -> Self {
        self.sort = sort.into_iter().collect();
        self
    }

    /// Replaces the grouping levels; the first level groups first. No levels
    /// is no grouping.
    pub fn with_group_by(mut self, levels: impl IntoIterator<Item = LayoutGroup>) -> Self {
        self.group_by = levels.into_iter().collect();
        self
    }

    /// Replaces the expansion of groups. The default, every group expanded
    /// and none toggled, is not written.
    pub fn with_expansion(mut self, expansion: LayoutExpansion) -> Self {
        self.expansion = expansion;
        self
    }

    /// The format version the layout was written in.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The view the layout belongs to, if it names one.
    pub fn view_id(&self) -> Option<&str> {
        self.view_id.as_deref()
    }

    /// The columns in display order, as saved: they may include columns the
    /// schema no longer has, and repeats.
    pub fn columns(&self) -> &[LayoutColumn] {
        &self.columns
    }

    /// The first saved column with this id.
    pub fn column(&self, id: &str) -> Option<&LayoutColumn> {
        self.columns.iter().find(|column| column.id.as_str() == id)
    }

    /// The sort keys, first key first.
    pub fn sort(&self) -> &[LayoutSort] {
        &self.sort
    }

    /// The grouping levels, the top level first, as saved: they may name
    /// columns the schema no longer has or cannot group, and repeats.
    pub fn group_by(&self) -> &[LayoutGroup] {
        &self.group_by
    }

    /// The expansion of groups; `None` when the layout keeps the default:
    /// every group expanded and none toggled.
    pub fn expansion(&self) -> Option<&LayoutExpansion> {
        (!self.expansion.is_default()).then_some(&self.expansion)
    }
}

/// One column of a saved layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LayoutColumn {
    id: ColumnId,
    #[serde(deserialize_with = "deserialize_width")]
    width: f32,
    #[serde(default = "default_visible", skip_serializing_if = "is_visible")]
    visible: bool,
    #[serde(
        rename = "frozen",
        default,
        skip_serializing_if = "is_scrolling",
        with = "frozen"
    )]
    zone: ColumnZone,
}

impl LayoutColumn {
    /// A visible, scrolling column. `width` is in logical pixels, so a
    /// layout saved on a Retina screen opens with the same widths on a 1×
    /// screen; it accepts `f32` or GPUI `Pixels`.
    pub fn new(id: impl Into<ColumnId>, width: impl Into<f32>) -> Self {
        Self {
            id: id.into(),
            width: width.into(),
            visible: true,
            zone: ColumnZone::Scrolling,
        }
    }

    /// Sets the width in logical pixels. The grid clamps it to the column's
    /// limits and replaces a width that is not a positive number.
    pub fn with_width(mut self, width: impl Into<f32>) -> Self {
        self.width = width.into();
        self
    }

    /// Shows or hides the column. Default: visible.
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Sets the zone. Default: [`ColumnZone::Scrolling`].
    pub fn with_zone(mut self, zone: ColumnZone) -> Self {
        self.zone = zone;
        self
    }

    /// The column's identifier.
    pub fn id(&self) -> &ColumnId {
        &self.id
    }

    /// The width in logical pixels, as saved.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// Returns true if the column is shown.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The zone the column is shown in.
    pub fn zone(&self) -> ColumnZone {
        self.zone
    }
}

fn default_visible() -> bool {
    true
}

fn is_visible(visible: &bool) -> bool {
    *visible
}

fn is_scrolling(zone: &ColumnZone) -> bool {
    *zone == ColumnZone::Scrolling
}

/// JSON has no NaN or infinity, and serde_json writes them as `null`. Read
/// `null` back as NaN, which the grid replaces with the schema width, so a
/// written layout always reads back.
fn deserialize_width<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    Ok(Option::<f32>::deserialize(deserializer)?.unwrap_or(f32::NAN))
}

/// The zone as the `frozen` field: `"left"`, `"right"`, or absent for a
/// scrolling column. The format lives here rather than in the names of the
/// `ColumnZone` variants, so renaming a variant cannot change saved layouts.
mod frozen {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::state::ColumnZone;

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum FrozenSide {
        Left,
        Right,
    }

    pub(super) fn serialize<S: Serializer>(
        zone: &ColumnZone,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match zone {
            ColumnZone::FrozenLeft => Some(FrozenSide::Left),
            ColumnZone::Scrolling => None,
            ColumnZone::FrozenRight => Some(FrozenSide::Right),
        }
        .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ColumnZone, D::Error> {
        Ok(match Option::<FrozenSide>::deserialize(deserializer)? {
            Some(FrozenSide::Left) => ColumnZone::FrozenLeft,
            None => ColumnZone::Scrolling,
            Some(FrozenSide::Right) => ColumnZone::FrozenRight,
        })
    }
}

/// One sort key of a saved layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutSort {
    column: ColumnId,
    direction: SortDirection,
}

impl LayoutSort {
    /// A sort key on a column.
    pub fn new(column: impl Into<ColumnId>, direction: SortDirection) -> Self {
        Self {
            column: column.into(),
            direction,
        }
    }

    /// The sorted column.
    pub fn column(&self) -> &ColumnId {
        &self.column
    }

    /// The direction.
    pub fn direction(&self) -> SortDirection {
        self.direction
    }
}

/// One grouping level of a saved layout: a column and the order of its
/// groups. Serialized as a sort key is:
/// `{"column": "instrument", "direction": "ascending"}`.
///
/// A level that groups by a function of the row (`GroupKey`) has no column
/// and is not saved: a function cannot be read back from a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LayoutGroup {
    column: ColumnId,
    direction: SortDirection,
}

impl LayoutGroup {
    /// A grouping level on a column, its groups in `direction`.
    pub fn new(column: impl Into<ColumnId>, direction: SortDirection) -> Self {
        Self {
            column: column.into(),
            direction,
        }
    }

    /// The grouped column.
    pub fn column(&self) -> &ColumnId {
        &self.column
    }

    /// The order of the level's groups.
    pub fn direction(&self) -> SortDirection {
        self.direction
    }
}

/// The saved expansion of groups: the default depth, and the groups the
/// user toggled against it. A toggled group is its key values from the top
/// level down, one per level of the layout's `group_by`; the levels name the
/// columns, so a path holds values only.
///
/// Serialized as `{"depth": "all", "toggled": [["AAPL"], ["MSFT", "Sell"]]}`.
/// `depth` is `"all"`, `"captions"` or the number of expanded levels
/// ([`ExpandDepth`]); a value is written as an [`OwnedValue`] is, so `12`
/// reads back as an integer and `12.0` as a float. `toggled` is not written
/// when it is empty. JSON has no NaN or infinity: a path with one is not
/// written, and that group's toggle is not saved.
///
/// Reading drops a path with a value this build cannot hold, such as a
/// decimal without the `decimal` feature or a kind of value a newer build
/// added, and keeps the rest of the layout; the grid's
/// [`LayoutReport::dropped_expansion`] counts it. Tree rows are not saved:
/// their keys are the application's own type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LayoutExpansion {
    #[serde(default)]
    depth: ExpandDepth,
    #[serde(default, skip_serializing_if = "TogglePaths::is_unwritten")]
    toggled: TogglePaths,
}

impl LayoutExpansion {
    /// `depth`, with no group toggled.
    pub fn new(depth: ExpandDepth) -> Self {
        Self {
            depth,
            toggled: TogglePaths::default(),
        }
    }

    /// Adds a group toggled against the depth: its key values, the top level
    /// first.
    pub fn with_toggled(mut self, path: impl IntoIterator<Item = OwnedValue>) -> Self {
        self.toggled.paths.push(path.into_iter().collect());
        self
    }

    /// The default depth.
    pub fn depth(&self) -> ExpandDepth {
        self.depth
    }

    /// The toggled groups in saved order, each as its key values from the
    /// top level down.
    pub fn toggled(&self) -> &[Vec<OwnedValue>] {
        &self.toggled.paths
    }

    /// Returns true for the default: every group expanded, none toggled.
    pub fn is_default(&self) -> bool {
        self.depth == ExpandDepth::All && self.toggled.paths.is_empty()
    }

    /// Returns true if writing would give the default: `ExpandDepth::All`
    /// and no path that JSON can hold.
    fn is_unwritten(&self) -> bool {
        self.depth == ExpandDepth::All && self.toggled.is_unwritten()
    }
}

/// The toggled groups of a [`LayoutExpansion`], and the number of saved
/// paths that could not be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TogglePaths {
    paths: Vec<Vec<OwnedValue>>,
    /// Saved paths with a value this build cannot hold: dropped when read,
    /// counted by the report, never written.
    unreadable: usize,
}

impl TogglePaths {
    fn is_unwritten(&self) -> bool {
        !self.paths.iter().any(|path| is_writable(path))
    }
}

/// JSON has no NaN or infinity, so a path with one is not written.
fn is_writable(path: &[OwnedValue]) -> bool {
    path.iter()
        .all(|value| !matches!(value, OwnedValue::Float(float) if !float.is_finite()))
}

impl Serialize for TogglePaths {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.paths.iter().filter(|path| is_writable(path)))
    }
}

impl<'de> Deserialize<'de> for TogglePaths {
    /// A list of lists, or the layout is not read. Each value is read as
    /// JSON first, so a value this build cannot hold drops its path only.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let saved = Vec::<Vec<serde_json::Value>>::deserialize(deserializer)?;
        let mut toggled = Self::default();
        for path in saved {
            match path
                .into_iter()
                .map(OwnedValue::deserialize)
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(path) => toggled.paths.push(path),
                Err(_) => toggled.unreadable += 1,
            }
        }
        Ok(toggled)
    }
}

/// Why a layout was not read, written or applied. A grid keeps its current
/// layout.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayoutError {
    /// Written by a newer build (`found > GridLayout::VERSION`) or malformed
    /// (`found == 0`).
    UnsupportedVersion {
        /// The version the layout carries; `u32::MAX` if it is larger.
        found: u32,
    },
    /// Saved for another grid.
    ViewMismatch {
        /// The grid's view id.
        expected: String,
        /// The layout's view id.
        found: String,
    },
    /// [`LayoutLibrary::insert`] needs a layout with a view id.
    MissingViewId,
    /// The text is not a layout in this codec's format.
    Decode(String),
    /// The codec could not write the layout.
    Encode(String),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found } => write!(
                f,
                "layout version {found} is not supported; this build reads 1 to {}",
                GridLayout::VERSION
            ),
            Self::ViewMismatch { expected, found } => {
                write!(f, "the layout is for view \"{found}\", not \"{expected}\"")
            }
            Self::MissingViewId => f.write_str("the layout has no view id"),
            Self::Decode(message) => write!(f, "the layout could not be read: {message}"),
            Self::Encode(message) => write!(f, "the layout could not be written: {message}"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Checks that this build reads a layout version: `1..=GridLayout::VERSION`.
pub(crate) fn check_version(found: u32) -> Result<(), LayoutError> {
    if (1..=GridLayout::VERSION).contains(&found) {
        Ok(())
    } else {
        Err(LayoutError::UnsupportedVersion { found })
    }
}

/// What applying a saved layout had to adjust to fit the current schema.
///
/// The grid computes it from the schema and the saved layout, whatever the
/// [`LayoutMerge`] returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutReport {
    ignored_columns: Vec<ColumnId>,
    added_columns: Vec<ColumnId>,
    dropped_sort: Vec<ColumnId>,
    dropped_group_by: Vec<ColumnId>,
    dropped_expansion: usize,
}

impl LayoutReport {
    /// Compares a saved layout with the schema in `context`: saved columns
    /// the schema does not have, schema columns the layout does not have,
    /// sort keys that are unknown or repeat an earlier key, and the grouping
    /// and expansion that do not fit (`ResolvedGrouping`). Each list is in
    /// the order of its source and names an id once.
    pub(crate) fn resolve(context: &LayoutMergeContext<'_>, saved: &GridLayout) -> Self {
        let mut ignored_columns = Vec::new();
        for column in saved.columns() {
            if !context.contains(column.id().as_str()) && !ignored_columns.contains(column.id()) {
                ignored_columns.push(column.id().clone());
            }
        }
        let added_columns = context
            .columns()
            .iter()
            .filter(|column| saved.column(column.id().as_str()).is_none())
            .map(|column| column.id().clone())
            .collect();
        let mut sorted = FxHashSet::default();
        let mut dropped_sort = Vec::new();
        for key in saved.sort() {
            let id = key.column();
            let kept = context.contains(id.as_str()) && sorted.insert(id.as_str());
            if !kept && !dropped_sort.contains(id) {
                dropped_sort.push(id.clone());
            }
        }
        let grouping = ResolvedGrouping::new(context, saved);
        Self {
            ignored_columns,
            added_columns,
            dropped_sort,
            dropped_group_by: grouping.dropped_group_by,
            dropped_expansion: grouping.dropped_expansion,
        }
    }

    /// Saved columns that the schema no longer has; ignored.
    pub fn ignored_columns(&self) -> &[ColumnId] {
        &self.ignored_columns
    }

    /// Schema columns the layout did not have; added with schema defaults.
    pub fn added_columns(&self) -> &[ColumnId] {
        &self.added_columns
    }

    /// Sort keys on unknown columns and repeated keys; dropped.
    pub fn dropped_sort(&self) -> &[ColumnId] {
        &self.dropped_sort
    }

    /// Grouping levels on columns the schema does not have or cannot group,
    /// and repeated levels; dropped, and the levels below move up. In saved
    /// order, each id once. A column can be in [`Self::dropped_sort`] too.
    pub fn dropped_group_by(&self) -> &[ColumnId] {
        &self.dropped_group_by
    }

    /// The number of saved toggled groups that do not fit the restored
    /// grouping: empty, deeper than its levels, or with a value this build
    /// cannot hold; dropped.
    pub fn dropped_expansion(&self) -> usize {
        self.dropped_expansion
    }

    /// Returns true if every saved column, sort key, grouping level and
    /// toggled group matched the schema and every schema column was saved.
    /// Widths clamped to a column's limits do not count as adjustments.
    pub fn is_exact(&self) -> bool {
        self.ignored_columns.is_empty()
            && self.added_columns.is_empty()
            && self.dropped_sort.is_empty()
            && self.dropped_group_by.is_empty()
            && self.dropped_expansion == 0
    }
}

/// A saved layout's grouping and expansion resolved against the schema: the
/// levels and toggled groups the grid restores, and what it drops.
/// [`LayoutReport::resolve`] takes the drops from here, so the report names
/// exactly what restoring leaves out.
pub(crate) struct ResolvedGrouping<'a> {
    levels: Vec<&'a LayoutGroup>,
    toggled: Vec<&'a [OwnedValue]>,
    depth: ExpandDepth,
    dropped_group_by: Vec<ColumnId>,
    dropped_expansion: usize,
}

impl<'a> ResolvedGrouping<'a> {
    /// Keeps the levels on schema columns that can be grouped, each column
    /// once (the first level wins); the levels below a dropped one move up.
    /// Keeps the toggled groups that are not empty and no deeper than the
    /// kept levels: their values apply to the kept levels as they are, the
    /// top level first, and are not matched again. A layout without levels
    /// keeps its depth but no toggled group, since no level names a column
    /// for its values. The depth stays as saved, even deeper than the
    /// levels: it then expands as `ExpandDepth::All` does.
    pub(crate) fn new(context: &LayoutMergeContext<'_>, saved: &'a GridLayout) -> Self {
        let mut grouped = FxHashSet::default();
        let mut levels = Vec::with_capacity(saved.group_by.len());
        let mut dropped_group_by = Vec::new();
        for level in &saved.group_by {
            let id = level.column();
            if context.is_groupable(id.as_str()) && grouped.insert(id.as_str()) {
                levels.push(level);
            } else if !dropped_group_by.contains(id) {
                dropped_group_by.push(id.clone());
            }
        }
        let expansion = &saved.expansion;
        let toggled: Vec<&[OwnedValue]> = expansion
            .toggled()
            .iter()
            .map(Vec::as_slice)
            .filter(|path| (1..=levels.len()).contains(&path.len()))
            .collect();
        let dropped_expansion =
            expansion.toggled.unreadable + (expansion.toggled().len() - toggled.len());
        Self {
            levels,
            toggled,
            depth: expansion.depth(),
            dropped_group_by,
            dropped_expansion,
        }
    }

    /// The kept levels, the top level first.
    pub(crate) fn levels(&self) -> &[&'a LayoutGroup] {
        &self.levels
    }

    /// The kept toggled groups in saved order, each as its key values for
    /// the kept levels from the top down.
    pub(crate) fn toggled(&self) -> &[&'a [OwnedValue]] {
        &self.toggled
    }

    /// The default depth, as saved.
    pub(crate) fn depth(&self) -> ExpandDepth {
        self.depth
    }
}

/// Turns a layout into text and back. Called once per import or export.
///
/// The default is [`JsonLayoutCodec`]. Another codec can read a foreign
/// format, such as a LiveGrid or Syncfusion XML layout.
pub trait LayoutCodec: 'static {
    /// Writes a layout.
    fn encode(&self, layout: &GridLayout) -> Result<String, LayoutError>;

    /// Reads a layout. Also migrates older versions of its own format, and
    /// refuses versions it does not read with
    /// [`LayoutError::UnsupportedVersion`].
    fn decode(&self, text: &str) -> Result<GridLayout, LayoutError>;
}

/// The JSON format of [`GridLayout`], compact by default.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonLayoutCodec {
    pretty: bool,
}

impl JsonLayoutCodec {
    /// Writes indented JSON, one field per line, instead of one line.
    pub fn pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }

    /// Returns true if the codec writes indented JSON.
    pub fn is_pretty(&self) -> bool {
        self.pretty
    }
}

impl LayoutCodec for JsonLayoutCodec {
    fn encode(&self, layout: &GridLayout) -> Result<String, LayoutError> {
        if self.pretty {
            serde_json::to_string_pretty(layout)
        } else {
            serde_json::to_string(layout)
        }
        .map_err(|error| LayoutError::Encode(error.to_string()))
    }

    fn decode(&self, text: &str) -> Result<GridLayout, LayoutError> {
        let decode_error = |error: serde_json::Error| LayoutError::Decode(error.to_string());
        let value: serde_json::Value = serde_json::from_str(text).map_err(decode_error)?;
        let Some(object) = value.as_object() else {
            return Err(LayoutError::Decode("expected a JSON object".into()));
        };
        let found = match object.get("version") {
            None => return Err(LayoutError::Decode("missing field `version`".into())),
            Some(version) => version.as_u64().ok_or_else(|| {
                LayoutError::Decode(format!("`version` is not a whole number: {version}"))
            })?,
        };
        check_version(u32::try_from(found).unwrap_or(u32::MAX))?;
        // A new version migrates the older ones here, by editing `value`
        // before it is parsed.
        serde_json::from_value(value).map_err(decode_error)
    }
}

/// The schema as a [`LayoutMerge`] sees it: every column with its default
/// width, visibility and zone, and whether it can be grouped.
#[derive(Clone, Copy, Debug)]
pub struct LayoutMergeContext<'a> {
    columns: &'a [LayoutColumn],
    /// Schema columns the grid may not group by; none by default.
    ungroupable: &'a [ColumnId],
}

impl<'a> LayoutMergeContext<'a> {
    /// The schema's columns in schema order, each with its defaults; every
    /// column can be grouped.
    pub(crate) fn new(columns: &'a [LayoutColumn]) -> Self {
        Self {
            columns,
            ungroupable: &[],
        }
    }

    /// Names the schema columns the grid may not group by.
    pub(crate) fn with_ungroupable(mut self, ids: &'a [ColumnId]) -> Self {
        self.ungroupable = ids;
        self
    }

    /// Schema columns in schema order, with default width, zone and
    /// visibility.
    pub fn columns(&self) -> &'a [LayoutColumn] {
        self.columns
    }

    /// The schema column with this id and its defaults.
    pub fn column(&self, id: &str) -> Option<&'a LayoutColumn> {
        self.columns
            .iter()
            .find(|column| column.id().as_str() == id)
    }

    /// Returns true if the schema has a column with this id.
    pub fn contains(&self, id: &str) -> bool {
        self.column(id).is_some()
    }

    /// Returns true if the schema has a column with this id and the grid may
    /// group by it.
    pub fn is_groupable(&self, id: &str) -> bool {
        self.contains(id) && !self.ungroupable.iter().any(|column| column.as_str() == id)
    }
}

/// Resolves a saved layout against the current schema. Called once per
/// import.
///
/// The default is [`AppendNewColumns`]. An alternative could place a new
/// column next to its neighbour in the schema, or keep unknown columns for a
/// schema that depends on the user's permissions.
pub trait LayoutMerge: 'static {
    /// One entry per schema column, in display order. The grid validates
    /// the result whatever the implementation returns: it adds missing
    /// columns, drops unknown ones and repeats, clamps widths, splits the
    /// order by zone and keeps one column visible.
    fn merge(&self, context: &LayoutMergeContext<'_>, saved: &GridLayout) -> Vec<LayoutColumn>;
}

/// The default [`LayoutMerge`]: saved columns keep their order, width,
/// visibility and zone; columns the schema no longer has are ignored, and a
/// repeated id keeps its first entry. Schema columns the layout does not
/// have go to the end of their zone, in schema order, with schema defaults.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppendNewColumns;

impl LayoutMerge for AppendNewColumns {
    fn merge(&self, context: &LayoutMergeContext<'_>, saved: &GridLayout) -> Vec<LayoutColumn> {
        fn zone_ix(zone: ColumnZone) -> usize {
            match zone {
                ColumnZone::FrozenLeft => 0,
                ColumnZone::Scrolling => 1,
                ColumnZone::FrozenRight => 2,
            }
        }

        let mut merged = FxHashSet::default();
        let mut zones: [Vec<LayoutColumn>; 3] = Default::default();
        for column in saved.columns() {
            let id = column.id().as_str();
            if context.contains(id) && merged.insert(id) {
                zones[zone_ix(column.zone())].push(column.clone());
            }
        }
        for column in context.columns() {
            if !merged.contains(column.id().as_str()) {
                zones[zone_ix(column.zone())].push(column.clone());
            }
        }
        zones.into_iter().flatten().collect()
    }
}

/// Named layouts per view id, for "Save Layout As…" and a layout picker,
/// plus the layout each view opens with.
///
/// Plain data: the application stores it where it keeps its settings and
/// serializes it with serde. A view id names the kind of grid ("trades"),
/// not a panel: every panel of a view shares its layouts. Layouts are kept
/// as given; the grid checks their version when it applies one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutLibrary {
    #[serde(default)]
    views: BTreeMap<String, ViewLayouts>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct ViewLayouts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    #[serde(default)]
    layouts: BTreeMap<String, GridLayout>,
}

impl LayoutLibrary {
    /// An empty library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `layout` under its view id and `name`, replacing a layout with
    /// that name. Fails with [`LayoutError::MissingViewId`] if the layout
    /// has no view id.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        layout: GridLayout,
    ) -> Result<(), LayoutError> {
        let view_id = layout.view_id().ok_or(LayoutError::MissingViewId)?;
        self.views
            .entry(view_id.to_owned())
            .or_default()
            .layouts
            .insert(name.into(), layout);
        Ok(())
    }

    /// The layout of a view with this name.
    pub fn get(&self, view_id: &str, name: &str) -> Option<&GridLayout> {
        self.views.get(view_id)?.layouts.get(name)
    }

    /// The names of a view's layouts, sorted by their text (byte order).
    pub fn names<'a>(&'a self, view_id: &str) -> impl Iterator<Item = &'a str> + use<'a> {
        self.views
            .get(view_id)
            .into_iter()
            .flat_map(|view| view.layouts.keys().map(String::as_str))
    }

    /// Removes a layout. If it was the view's default, the view has no
    /// default afterwards.
    pub fn remove(&mut self, view_id: &str, name: &str) -> Option<GridLayout> {
        let view = self.views.get_mut(view_id)?;
        let layout = view.layouts.remove(name)?;
        if view.default.as_deref() == Some(name) {
            view.default = None;
        }
        if view.layouts.is_empty() {
            self.views.remove(view_id);
        }
        Some(layout)
    }

    /// Renames a layout; the default follows it. Returns false, and changes
    /// nothing, if there is no layout `from` or another layout is named
    /// `to`.
    pub fn rename(&mut self, view_id: &str, from: &str, to: impl Into<String>) -> bool {
        let to = to.into();
        let Some(view) = self.views.get_mut(view_id) else {
            return false;
        };
        if to != from && view.layouts.contains_key(&to) {
            return false;
        }
        let Some(layout) = view.layouts.remove(from) else {
            return false;
        };
        if view.default.as_deref() == Some(from) {
            view.default = Some(to.clone());
        }
        view.layouts.insert(to, layout);
        true
    }

    /// Sets the layout a view opens with when nothing else is restored, or
    /// clears it with `None`. Returns false, and changes nothing, if the
    /// view has no layout with that name.
    pub fn set_default(&mut self, view_id: &str, name: Option<&str>) -> bool {
        let view = self.views.get_mut(view_id);
        match (view, name) {
            (Some(view), Some(name)) if view.layouts.contains_key(name) => {
                view.default = Some(name.to_owned());
                true
            }
            (_, Some(_)) => false,
            (Some(view), None) => {
                view.default = None;
                true
            }
            (None, None) => true,
        }
    }

    /// The name and layout a view opens with when nothing else is restored.
    pub fn default_layout(&self, view_id: &str) -> Option<(&str, &GridLayout)> {
        let view = self.views.get(view_id)?;
        let (name, layout) = view.layouts.get_key_value(view.default.as_deref()?)?;
        Some((name.as_str(), layout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference layout behind `tests/fixtures/layout-v1.json`: every
    /// zone, hidden columns in two zones, a fractional width and two sort
    /// keys.
    fn reference_layout() -> GridLayout {
        GridLayout::new()
            .with_view_id("trades")
            .with_column(LayoutColumn::new("id", 80.).with_zone(ColumnZone::FrozenLeft))
            .with_column(LayoutColumn::new("instrument", 140.))
            .with_column(LayoutColumn::new("time", 92.5))
            .with_column(LayoutColumn::new("fee", 90.).with_visible(false))
            .with_column(LayoutColumn::new("price", 100.))
            .with_column(LayoutColumn::new("side", 60.).with_zone(ColumnZone::FrozenRight))
            .with_column(
                LayoutColumn::new("venue", 72.)
                    .with_visible(false)
                    .with_zone(ColumnZone::FrozenRight),
            )
            .with_sort([
                LayoutSort::new("time", SortDirection::Descending),
                LayoutSort::new("instrument", SortDirection::Ascending),
            ])
    }

    #[test]
    fn test_layout_builder() {
        let empty = GridLayout::new();
        assert_eq!(empty.version(), GridLayout::VERSION);
        assert_eq!(empty.view_id(), None);
        assert!(empty.columns().is_empty() && empty.sort().is_empty());

        let column = LayoutColumn::new("price", 100.);
        assert_eq!(column.id().as_str(), "price");
        assert_eq!(column.width(), 100.);
        assert!(column.is_visible());
        assert_eq!(column.zone(), ColumnZone::Scrolling);

        let layout = GridLayout::new()
            .with_view_id("orders")
            .with_column(
                LayoutColumn::new("price", 100.)
                    .with_width(gpui_kit::px(120.))
                    .with_visible(false)
                    .with_zone(ColumnZone::FrozenRight),
            )
            .with_column(LayoutColumn::new(String::from("qty"), 60.))
            .with_column(LayoutColumn::new("price", 30.))
            .with_sort([LayoutSort::new("qty", SortDirection::Ascending)])
            .with_sort([LayoutSort::new("price", SortDirection::Descending)]);
        assert_eq!(layout.view_id(), Some("orders"));
        let ids: Vec<_> = layout.columns().iter().map(|c| c.id().as_str()).collect();
        assert_eq!(ids, ["price", "qty", "price"]);
        // `column` finds the first entry of a repeated id.
        let price = layout.column("price").unwrap();
        assert_eq!(price.width(), 120.);
        assert!(!price.is_visible());
        assert_eq!(price.zone(), ColumnZone::FrozenRight);
        assert_eq!(layout.column("fee"), None);
        // `with_sort` replaces the sort.
        assert_eq!(layout.sort().len(), 1);
        assert_eq!(layout.sort()[0].column().as_str(), "price");
        assert_eq!(layout.sort()[0].direction(), SortDirection::Descending);

        assert!(!JsonLayoutCodec::default().is_pretty());
        assert!(JsonLayoutCodec::default().pretty(true).is_pretty());
        assert_eq!(
            LayoutError::UnsupportedVersion { found: 2 }.to_string(),
            "layout version 2 is not supported; this build reads 1 to 1"
        );
    }

    #[test]
    fn test_layout_json_matches_fixture() {
        // Git may check the fixture out with CRLF line endings on Windows.
        let fixture = include_str!("../tests/fixtures/layout-v1.json").replace("\r\n", "\n");
        let layout = reference_layout();

        let pretty = JsonLayoutCodec::default().pretty(true);
        assert_eq!(format!("{}\n", pretty.encode(&layout).unwrap()), fixture);
        assert_eq!(pretty.decode(&fixture).unwrap(), layout);

        // Without grouping the layout is written byte for byte as before the
        // grouping fields, and it reads back without them.
        let ungrouped = layout
            .clone()
            .with_group_by(Vec::new())
            .with_expansion(LayoutExpansion::default());
        assert_eq!(ungrouped, layout);
        assert_eq!(format!("{}\n", pretty.encode(&ungrouped).unwrap()), fixture);
        let read = pretty.decode(&fixture).unwrap();
        assert!(read.group_by().is_empty());
        assert_eq!(read.expansion(), None);

        // The compact form and plain serde (the dock recipe's path) read the
        // same layout.
        let codec = JsonLayoutCodec::default();
        let compact = codec.encode(&layout).unwrap();
        assert!(!compact.contains('\n'));
        assert_eq!(codec.decode(&compact).unwrap(), layout);
        assert_eq!(
            serde_json::from_str::<GridLayout>(&fixture).unwrap(),
            layout
        );
    }

    #[test]
    fn test_layout_decode_defaults_and_unknown_fields() {
        let codec = JsonLayoutCodec::default();
        let layout = codec
            .decode(
                r#"{
                    "version": 1,
                    "view_id": null,
                    "columns": [
                        { "id": "a", "width": 50 },
                        { "id": "b", "width": 60.5, "visible": true, "frozen": null, "tint": "red" },
                        { "id": "c", "width": null, "visible": false, "frozen": "right" }
                    ],
                    "filters": ["a"],
                    "density": { "rows": "compact" }
                }"#,
            )
            .unwrap();
        assert_eq!(layout.version(), 1);
        assert_eq!(layout.view_id(), None);
        assert!(layout.sort().is_empty());
        let a = layout.column("a").unwrap();
        assert_eq!(a.width(), 50.);
        assert!(a.is_visible());
        assert_eq!(a.zone(), ColumnZone::Scrolling);
        let b = layout.column("b").unwrap();
        assert_eq!(
            (b.width(), b.is_visible(), b.zone()),
            (60.5, true, ColumnZone::Scrolling)
        );
        // A `null` width (how JSON writes NaN) reads as NaN; the grid replaces
        // it with the schema width.
        let c = layout.column("c").unwrap();
        assert!(c.width().is_nan());
        assert_eq!((c.is_visible(), c.zone()), (false, ColumnZone::FrozenRight));

        // Saving again drops the unknown fields and writes no defaults.
        assert_eq!(
            codec.encode(&layout).unwrap(),
            concat!(
                r#"{"version":1,"columns":[{"id":"a","width":50.0},{"id":"b","width":60.5},"#,
                r#"{"id":"c","width":null,"visible":false,"frozen":"right"}]}"#,
            )
        );

        // Unknown values of known fields are not a layout.
        for text in [
            r#"{"version":1,"columns":[{"id":"a","width":50,"frozen":"top"}]}"#,
            r#"{"version":1,"columns":[{"id":"a","width":"wide"}]}"#,
            r#"{"version":1,"columns":[{"width":50}]}"#,
            r#"{"version":1,"columns":[],"sort":[{"column":"a","direction":"up"}]}"#,
            r#"{"version":1,"columns":[],"group_by":[{"column":"a","direction":"up"}]}"#,
            r#"{"version":1,"columns":[],"group_by":[{"direction":"ascending"}]}"#,
            r#"{"version":1,"columns":[],"group_by":["a"]}"#,
            r#"{"version":1,"columns":[],"expansion":{"depth":"deep"}}"#,
            r#"{"version":1,"columns":[],"expansion":"all"}"#,
            r#"{"version":1,"columns":[],"expansion":{"toggled":"AAPL"}}"#,
            r#"{"version":1,"columns":[],"expansion":{"toggled":["AAPL"]}}"#,
        ] {
            assert!(
                matches!(codec.decode(text), Err(LayoutError::Decode(_))),
                "{text}"
            );
        }
    }

    fn text(value: &str) -> OwnedValue {
        OwnedValue::Text(value.into())
    }

    /// The reference layout behind `tests/fixtures/layout-v1-groups.json`:
    /// two grouping levels, the default depth and toggled groups of every
    /// kind of value, `12` and `12.0` apart. The decimal path needs the
    /// `decimal` feature.
    fn grouped_layout() -> GridLayout {
        let expansion = LayoutExpansion::new(ExpandDepth::All)
            .with_toggled([text("AAPL")])
            .with_toggled([text("MSFT"), text("Sell")])
            .with_toggled([OwnedValue::Empty])
            .with_toggled([OwnedValue::Bool(true)])
            .with_toggled([OwnedValue::Int(12)])
            .with_toggled([OwnedValue::Float(12.0)]);
        #[cfg(feature = "decimal")]
        let expansion =
            expansion.with_toggled([OwnedValue::Decimal(rust_decimal::Decimal::new(1020, 2))]);
        GridLayout::new()
            .with_view_id("trades")
            .with_column(LayoutColumn::new("instrument", 140.))
            .with_column(LayoutColumn::new("side", 60.))
            .with_column(LayoutColumn::new("price", 100.))
            .with_sort([LayoutSort::new("price", SortDirection::Descending)])
            .with_group_by([
                LayoutGroup::new("instrument", SortDirection::Ascending),
                LayoutGroup::new("side", SortDirection::Descending),
            ])
            .with_expansion(expansion)
    }

    fn ids(ids: &[ColumnId]) -> Vec<&str> {
        ids.iter().map(ColumnId::as_str).collect()
    }

    #[test]
    fn test_layout_groups_builder() {
        let level = LayoutGroup::new("side", SortDirection::Descending);
        assert_eq!(level.column().as_str(), "side");
        assert_eq!(level.direction(), SortDirection::Descending);

        let expansion = LayoutExpansion::new(ExpandDepth::Levels(1))
            .with_toggled([text("AAPL")])
            .with_toggled(Vec::new());
        assert_eq!(expansion.depth(), ExpandDepth::Levels(1));
        assert_eq!(expansion.toggled(), [vec![text("AAPL")], Vec::new()]);
        assert!(!expansion.is_default());
        assert!(LayoutExpansion::default().is_default());
        assert!(!LayoutExpansion::new(ExpandDepth::Captions).is_default());
        assert!(
            !LayoutExpansion::default()
                .with_toggled([text("a")])
                .is_default()
        );

        let layout = GridLayout::new()
            .with_group_by([level.clone()])
            .with_group_by([level.clone(), level.clone()])
            .with_expansion(LayoutExpansion::new(ExpandDepth::Captions))
            .with_expansion(expansion.clone());
        // Both builders replace.
        assert_eq!(layout.group_by(), [level.clone(), level]);
        assert_eq!(layout.expansion(), Some(&expansion));
        assert!(GridLayout::new().group_by().is_empty());
        assert_eq!(GridLayout::new().expansion(), None);
        assert_eq!(
            layout
                .with_expansion(LayoutExpansion::default())
                .expansion(),
            None
        );
    }

    #[test]
    fn test_layout_groups_fixture() {
        use std::mem::discriminant;

        let fixture = include_str!("../tests/fixtures/layout-v1-groups.json").replace("\r\n", "\n");
        let layout = grouped_layout();
        let pretty = JsonLayoutCodec::default().pretty(true);
        let read = pretty.decode(&fixture).unwrap();
        // Plain serde (the dock recipe's path) reads the same layout.
        assert_eq!(serde_json::from_str::<GridLayout>(&fixture).unwrap(), read);

        // `12` reads as an integer and `12.0` as a float.
        let toggled = read.expansion().unwrap().toggled();
        assert_eq!(
            discriminant(&toggled[4][0]),
            discriminant(&OwnedValue::Int(12))
        );
        assert_eq!(
            discriminant(&toggled[5][0]),
            discriminant(&OwnedValue::Float(12.0))
        );

        let schema = [
            LayoutColumn::new("instrument", 100.),
            LayoutColumn::new("side", 100.),
            LayoutColumn::new("price", 100.),
        ];
        let context = LayoutMergeContext::new(&schema);
        let report = LayoutReport::resolve(&context, &read);
        assert!(report.dropped_group_by().is_empty());

        #[cfg(feature = "decimal")]
        {
            assert_eq!(format!("{}\n", pretty.encode(&layout).unwrap()), fixture);
            assert_eq!(read, layout);
            assert!(
                matches!(&toggled[6][0], OwnedValue::Decimal(value) if value.to_string() == "10.20")
            );
            assert!(report.is_exact());
        }
        #[cfg(not(feature = "decimal"))]
        {
            // Without the feature a decimal cannot be a key: the layout
            // reads, the path is dropped and the report counts it.
            assert_eq!(read.group_by(), layout.group_by());
            assert_eq!(toggled, layout.expansion().unwrap().toggled());
            assert_eq!(report.dropped_expansion(), 1);
            assert!(!report.is_exact());
            // Saving again writes the rest.
            assert_eq!(
                pretty.encode(&read).unwrap(),
                pretty.encode(&layout).unwrap()
            );
        }
        // Writing again gives the same text.
        let compact = JsonLayoutCodec::default();
        let text = compact.encode(&read).unwrap();
        assert_eq!(
            compact.encode(&compact.decode(&text).unwrap()).unwrap(),
            text
        );
    }

    #[test]
    fn test_expansion_default_omitted() {
        let codec = JsonLayoutCodec::default();
        let empty = r#"{"version":1,"columns":[]}"#;
        // No grouping and the default expansion are not written; nor is a
        // path with a NaN or an infinity, which JSON cannot hold.
        for layout in [
            GridLayout::new().with_expansion(LayoutExpansion::new(ExpandDepth::All)),
            GridLayout::new()
                .with_group_by(Vec::new())
                .with_expansion(LayoutExpansion::default()),
            GridLayout::new().with_expansion(
                LayoutExpansion::default()
                    .with_toggled([text("a"), OwnedValue::Float(f64::NAN)])
                    .with_toggled([OwnedValue::Float(f64::NEG_INFINITY)]),
            ),
        ] {
            assert_eq!(codec.encode(&layout).unwrap(), empty);
        }
        let read = codec.decode(empty).unwrap();
        assert_eq!(read, GridLayout::new());
        assert_eq!(read.expansion(), None);

        // Any other depth is written, without an empty `toggled`.
        for (depth, json) in [
            (ExpandDepth::Captions, r#""captions""#),
            (ExpandDepth::Levels(0), "0"),
            (ExpandDepth::Levels(3), "3"),
        ] {
            let layout = GridLayout::new().with_expansion(LayoutExpansion::new(depth));
            let text = codec.encode(&layout).unwrap();
            assert_eq!(
                text,
                format!(r#"{{"version":1,"columns":[],"expansion":{{"depth":{json}}}}}"#)
            );
            assert_eq!(codec.decode(&text).unwrap(), layout);
            assert_eq!(layout.expansion().map(LayoutExpansion::depth), Some(depth));
        }

        // Only the paths JSON can hold are written.
        let mixed = GridLayout::new().with_expansion(
            LayoutExpansion::new(ExpandDepth::Levels(1))
                .with_toggled([text("a"), OwnedValue::Float(f64::INFINITY)])
                .with_toggled([text("b")]),
        );
        assert_eq!(
            codec.encode(&mixed).unwrap(),
            r#"{"version":1,"columns":[],"expansion":{"depth":1,"toggled":[["b"]]}}"#
        );

        // `depth` reads "all", "captions" and whole numbers, and is "all"
        // when missing.
        for (json, depth) in [
            (r#"{"depth":"all"}"#, ExpandDepth::All),
            (r#"{"depth":"captions"}"#, ExpandDepth::Captions),
            (r#"{"depth":0}"#, ExpandDepth::Levels(0)),
            (r#"{"depth":2,"rows":[[1]]}"#, ExpandDepth::Levels(2)),
            (r#"{}"#, ExpandDepth::All),
            (r#"{"toggled":[]}"#, ExpandDepth::All),
        ] {
            let text = format!(r#"{{"version":1,"columns":[],"expansion":{json}}}"#);
            let layout = codec.decode(&text).unwrap();
            assert_eq!(
                layout
                    .expansion()
                    .map_or(ExpandDepth::All, LayoutExpansion::depth),
                depth,
                "{json}"
            );
        }
        for json in [r#""deep""#, r#""All""#, "-1", "1.5", "65536", "null", "[1]"] {
            let text = format!(r#"{{"version":1,"columns":[],"expansion":{{"depth":{json}}}}}"#);
            assert!(
                matches!(codec.decode(&text), Err(LayoutError::Decode(_))),
                "{json}"
            );
        }
    }

    #[test]
    fn test_layout_report_groups() {
        // Five schema columns; `e` cannot be grouped.
        let schema = ["a", "b", "c", "d", "e"].map(|id| LayoutColumn::new(id, 50.));
        let ungroupable = [ColumnId::from("e")];
        let context = LayoutMergeContext::new(&schema).with_ungroupable(&ungroupable);
        assert!(context.is_groupable("a") && context.contains("e"));
        assert!(!context.is_groupable("e") && !context.is_groupable("gone"));
        assert!(
            LayoutMergeContext::new(&schema).is_groupable("e"),
            "every column can be grouped by default"
        );

        let saved = JsonLayoutCodec::default()
            .decode(
                r#"{
                    "version": 1,
                    "columns": [
                        { "id": "a", "width": 50 }, { "id": "b", "width": 50 },
                        { "id": "c", "width": 50 }, { "id": "d", "width": 50 },
                        { "id": "e", "width": 50 }
                    ],
                    "sort": [
                        { "column": "gone", "direction": "ascending" },
                        { "column": "e", "direction": "ascending" }
                    ],
                    "group_by": [
                        { "column": "a", "direction": "ascending" },
                        { "column": "gone", "direction": "ascending" },
                        { "column": "b", "direction": "descending" },
                        { "column": "a", "direction": "descending" },
                        { "column": "e", "direction": "ascending" },
                        { "column": "gone", "direction": "descending" },
                        { "column": "c", "direction": "ascending" }
                    ],
                    "expansion": {
                        "depth": 9,
                        "toggled": [
                            ["x"], [], [1, 2, 3], [1, 2, 3, 4], ["x", "y"],
                            [{ "date": "2026-11-04" }], ["x", [1]], [18446744073709551615]
                        ]
                    }
                }"#,
            )
            .unwrap();
        // A value this build cannot hold drops its path when read.
        assert_eq!(saved.expansion().unwrap().toggled().len(), 5);

        let report = LayoutReport::resolve(&context, &saved);
        // Unknown, repeated and ungroupable levels, in saved order, once.
        assert_eq!(ids(report.dropped_group_by()), ["gone", "a", "e"]);
        // Empty and too deep paths, and the three unreadable ones.
        assert_eq!(report.dropped_expansion(), 5);
        // A column can leave the sort and the grouping alike.
        assert_eq!(ids(report.dropped_sort()), ["gone"]);
        assert!(report.ignored_columns().is_empty() && report.added_columns().is_empty());
        assert!(!report.is_exact());

        // The lower levels move up; paths apply to them as they are.
        let resolved = ResolvedGrouping::new(&context, &saved);
        let levels: Vec<_> = resolved
            .levels()
            .iter()
            .map(|level| (level.column().as_str(), level.direction()))
            .collect();
        assert_eq!(
            levels,
            [
                ("a", SortDirection::Ascending),
                ("b", SortDirection::Descending),
                ("c", SortDirection::Ascending),
            ]
        );
        let int = OwnedValue::Int;
        assert_eq!(
            resolved.toggled(),
            [
                &[text("x")][..],
                &[int(1), int(2), int(3)],
                &[text("x"), text("y")],
            ]
        );
        // The depth stays as saved, deeper than the levels.
        assert_eq!(resolved.depth(), ExpandDepth::Levels(9));

        // When every level is dropped, so is every path; the depth stays.
        let gone = GridLayout::new()
            .with_group_by([LayoutGroup::new("gone", SortDirection::Ascending)])
            .with_expansion(LayoutExpansion::new(ExpandDepth::Captions).with_toggled([text("x")]));
        let report = LayoutReport::resolve(&context, &gone);
        assert_eq!(ids(report.dropped_group_by()), ["gone"]);
        assert_eq!(report.dropped_expansion(), 1);
        let resolved = ResolvedGrouping::new(&context, &gone);
        assert!(resolved.levels().is_empty() && resolved.toggled().is_empty());
        assert_eq!(resolved.depth(), ExpandDepth::Captions);

        // Without grouping a depth alone is exact; a path names no column.
        let depth_only = |expansion: LayoutExpansion| {
            schema
                .iter()
                .cloned()
                .fold(GridLayout::new(), GridLayout::with_column)
                .with_expansion(expansion)
        };
        let report = LayoutReport::resolve(
            &context,
            &depth_only(LayoutExpansion::new(ExpandDepth::Levels(0))),
        );
        assert!(report.is_exact());
        let report = LayoutReport::resolve(
            &context,
            &depth_only(LayoutExpansion::new(ExpandDepth::Levels(0)).with_toggled([text("x")])),
        );
        assert_eq!(report.dropped_expansion(), 1);

        // A layout that fits is exact.
        let fits = depth_only(
            LayoutExpansion::new(ExpandDepth::All)
                .with_toggled([text("x")])
                .with_toggled([text("x"), OwnedValue::Empty]),
        )
        .with_group_by([
            LayoutGroup::new("d", SortDirection::Descending),
            LayoutGroup::new("a", SortDirection::Ascending),
        ]);
        assert!(LayoutReport::resolve(&context, &fits).is_exact());
        assert_eq!(ResolvedGrouping::new(&context, &fits).toggled().len(), 2);
    }

    #[test]
    fn test_old_build_reads_new_layout() {
        /// `GridLayout` as a build without grouping declares it.
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct OldLayout {
            version: u32,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            view_id: Option<String>,
            columns: Vec<LayoutColumn>,
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            sort: Vec<LayoutSort>,
        }

        // The old build reads the grouped fixture and keeps the rest.
        let fixture = include_str!("../tests/fixtures/layout-v1-groups.json");
        let old: OldLayout = serde_json::from_str(fixture).unwrap();
        let layout = grouped_layout();
        assert_eq!(old.version, GridLayout::VERSION);
        assert_eq!(old.view_id.as_deref(), layout.view_id());
        assert_eq!(old.columns, layout.columns());
        assert_eq!(old.sort, layout.sort());

        // Saving there drops the grouping; this build reads that as a layout
        // without grouping.
        let saved_again = serde_json::to_string(&old).unwrap();
        let read = JsonLayoutCodec::default().decode(&saved_again).unwrap();
        assert_eq!(
            read,
            layout
                .with_group_by(Vec::new())
                .with_expansion(LayoutExpansion::default())
        );

        // Both builds write a layout without grouping alike.
        let reference = reference_layout();
        let old = OldLayout {
            version: reference.version(),
            view_id: reference.view_id().map(str::to_owned),
            columns: reference.columns().to_vec(),
            sort: reference.sort().to_vec(),
        };
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            JsonLayoutCodec::default().encode(&reference).unwrap()
        );
    }

    #[test]
    fn test_layout_round_trip_property() {
        const IDS: [&str; 10] = ["id", "a", "b", "c", "d", "e", "f", "g", "Цена", "量"];
        const PIECES: [&str; 10] = ["a", "Я", "✓", "😀", "\"", "\\", "\n", " ", "é", "中"];

        fn value(rng: &mut fastrand::Rng) -> OwnedValue {
            match rng.u8(..7) {
                0 => OwnedValue::Empty,
                1 => OwnedValue::Bool(rng.bool()),
                2 => OwnedValue::Int(rng.i64(..)),
                // Any finite float, bit for bit.
                3 => loop {
                    let float = f64::from_bits(rng.u64(..));
                    if float.is_finite() {
                        break OwnedValue::Float(float);
                    }
                },
                // Prices, integral floats and zeros of both signs.
                4 => OwnedValue::Float(match rng.u8(..4) {
                    0 => (rng.f64() - 0.5) * 1e6,
                    1 => rng.i32(..) as f64,
                    2 => 0.,
                    _ => -0.,
                }),
                _ => OwnedValue::Text(
                    (0..rng.usize(..6))
                        .map(|_| PIECES[rng.usize(..PIECES.len())])
                        .collect::<String>()
                        .into(),
                ),
            }
        }

        fn direction(rng: &mut fastrand::Rng) -> SortDirection {
            if rng.bool() {
                SortDirection::Ascending
            } else {
                SortDirection::Descending
            }
        }

        let zones = [
            ColumnZone::FrozenLeft,
            ColumnZone::Scrolling,
            ColumnZone::FrozenRight,
        ];
        for seed in 0..300 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let mut layout = GridLayout::new();
            if rng.bool() {
                layout = layout.with_view_id(PIECES[rng.usize(..PIECES.len())]);
            }
            for _ in 0..rng.usize(..=8) {
                layout = layout.with_column(
                    LayoutColumn::new(IDS[rng.usize(..IDS.len())], rng.u16(1..800) as f32 / 2.)
                        .with_visible(rng.bool())
                        .with_zone(zones[rng.usize(..3)]),
                );
            }
            let id = |rng: &mut fastrand::Rng| IDS[rng.usize(..IDS.len())];
            let sort: Vec<_> = (0..rng.usize(..=3))
                .map(|_| LayoutSort::new(id(&mut rng), direction(&mut rng)))
                .collect();
            let levels: Vec<_> = (0..rng.usize(..=3))
                .map(|_| LayoutGroup::new(id(&mut rng), direction(&mut rng)))
                .collect();
            let depth = match rng.u8(..4) {
                0 => ExpandDepth::All,
                1 => ExpandDepth::Captions,
                _ => ExpandDepth::Levels(rng.u16(..5)),
            };
            let mut expansion = LayoutExpansion::new(depth);
            for _ in 0..rng.usize(..=10) {
                let path: Vec<_> = (0..rng.usize(1..=3)).map(|_| value(&mut rng)).collect();
                expansion = expansion.with_toggled(path);
            }
            let layout = layout
                .with_sort(sort)
                .with_group_by(levels)
                .with_expansion(expansion);

            for codec in [
                JsonLayoutCodec::default(),
                JsonLayoutCodec::default().pretty(true),
            ] {
                let text = codec.encode(&layout).unwrap();
                let read = codec.decode(&text).unwrap();
                assert_eq!(read, layout, "seed {seed}: {text}");
                // Integers stay integers and floats floats, bit for bit.
                assert_eq!(codec.encode(&read).unwrap(), text, "seed {seed}");
            }
        }
    }

    /// Microseconds to write and to read a layout of 128 columns, three
    /// grouping levels and ten toggled groups. Run with `cargo test
    /// --release -p gpui-kit-grid --lib -- --ignored --nocapture
    /// test_layout_codec_timing`.
    #[test]
    #[ignore = "timing; run in release with --ignored --nocapture"]
    #[cfg(not(target_family = "wasm"))]
    fn test_layout_codec_timing() {
        use std::{hint::black_box, time::Instant};

        let names: Vec<String> = (0..128).map(|ix| format!("column_{ix}")).collect();
        let mut expansion = LayoutExpansion::new(ExpandDepth::Levels(1));
        for ix in 0..10 {
            expansion = expansion.with_toggled([
                text(&format!("instrument {ix}")),
                text("Buy"),
                OwnedValue::Float(100.25 + f64::from(ix)),
            ]);
        }
        let layout = names
            .iter()
            .map(|name| LayoutColumn::new(name.clone(), 96.5))
            .fold(
                GridLayout::new().with_view_id("trades"),
                GridLayout::with_column,
            )
            .with_sort([LayoutSort::new("column_3", SortDirection::Descending)])
            .with_group_by(
                names[..3]
                    .iter()
                    .map(|name| LayoutGroup::new(name.clone(), SortDirection::Ascending)),
            )
            .with_expansion(expansion);

        let codec = JsonLayoutCodec::default();
        let text = codec.encode(&layout).unwrap();
        let micros = |run: &dyn Fn()| {
            (0..5)
                .map(|_| {
                    let started = Instant::now();
                    for _ in 0..1_000 {
                        run();
                    }
                    started.elapsed().as_secs_f64() * 1e6 / 1_000.
                })
                .fold(f64::MAX, f64::min)
        };
        let encode = micros(&|| {
            black_box(codec.encode(black_box(&layout)).unwrap());
        });
        let decode = micros(&|| {
            black_box(codec.decode(black_box(&text)).unwrap());
        });
        println!(
            "{} bytes: encode {encode:.1} µs, decode {decode:.1} µs",
            text.len()
        );
    }

    #[test]
    fn test_layout_version_rules() {
        let codec = JsonLayoutCodec::default();
        for (text, found) in [
            (r#"{"version":2,"columns":[]}"#, 2),
            (r#"{"version":0,"columns":[]}"#, 0),
            // The version is checked before the rest: a newer build may have
            // changed what the fields mean.
            (r#"{"version":99,"columns":"by id"}"#, 99),
            (r#"{"version":4294967296,"columns":[]}"#, u32::MAX),
        ] {
            assert_eq!(
                codec.decode(text),
                Err(LayoutError::UnsupportedVersion { found }),
                "{text}"
            );
        }
        for text in [
            r#"{"columns":[]}"#,
            r#"{"version":"1","columns":[]}"#,
            r#"{"version":-1,"columns":[]}"#,
            r#"{"version":1.5,"columns":[]}"#,
            r#"{"version":1}"#,
            r#"[{"version":1,"columns":[]}]"#,
            "not json",
            "",
        ] {
            assert!(
                matches!(codec.decode(text), Err(LayoutError::Decode(_))),
                "{text}"
            );
        }

        // Plain serde keeps the version it reads, so the grid can refuse a
        // layout that did not come through a codec.
        let newer: GridLayout = serde_json::from_str(r#"{"version":2,"columns":[]}"#).unwrap();
        assert_eq!(newer.version(), 2);
        assert_eq!(
            check_version(newer.version()),
            Err(LayoutError::UnsupportedVersion { found: 2 })
        );
        assert_eq!(check_version(GridLayout::VERSION), Ok(()));
    }

    #[test]
    fn test_append_new_columns() {
        // Schema order with defaults; `fee`, `flag` and `note` are new.
        let schema = [
            LayoutColumn::new("id", 80.).with_zone(ColumnZone::FrozenLeft),
            LayoutColumn::new("name", 120.),
            LayoutColumn::new("fee", 60.),
            LayoutColumn::new("price", 90.),
            LayoutColumn::new("flag", 40.).with_zone(ColumnZone::FrozenLeft),
            LayoutColumn::new("qty", 70.).with_visible(false),
            LayoutColumn::new("note", 200.).with_visible(false),
            LayoutColumn::new("side", 50.).with_zone(ColumnZone::FrozenRight),
        ];
        let context = LayoutMergeContext::new(&schema);
        assert!(context.contains("fee") && !context.contains("gone"));
        assert_eq!(context.column("qty"), Some(&schema[5]));

        let saved = GridLayout::new()
            .with_column(LayoutColumn::new("price", 101.))
            .with_column(LayoutColumn::new("gone", 55.))
            .with_column(LayoutColumn::new("name", 133.).with_visible(false))
            .with_column(LayoutColumn::new("price", 999.).with_zone(ColumnZone::FrozenLeft))
            .with_column(LayoutColumn::new("qty", 71.).with_zone(ColumnZone::FrozenRight))
            .with_column(LayoutColumn::new("id", 81.).with_zone(ColumnZone::FrozenLeft))
            .with_column(LayoutColumn::new("side", 51.).with_zone(ColumnZone::FrozenRight))
            .with_column(LayoutColumn::new("gone", 56.))
            .with_sort([
                LayoutSort::new("gone", SortDirection::Descending),
                LayoutSort::new("price", SortDirection::Ascending),
                LayoutSort::new("price", SortDirection::Descending),
                LayoutSort::new("side", SortDirection::Ascending),
                LayoutSort::new("zz", SortDirection::Ascending),
            ]);

        // Saved columns keep their width, visibility and zone; the first
        // `price` wins; new columns end their zone in schema order with
        // schema defaults.
        let merged = AppendNewColumns.merge(&context, &saved);
        let expected = [
            LayoutColumn::new("id", 81.).with_zone(ColumnZone::FrozenLeft),
            LayoutColumn::new("flag", 40.).with_zone(ColumnZone::FrozenLeft),
            LayoutColumn::new("price", 101.),
            LayoutColumn::new("name", 133.).with_visible(false),
            LayoutColumn::new("fee", 60.),
            LayoutColumn::new("note", 200.).with_visible(false),
            LayoutColumn::new("qty", 71.).with_zone(ColumnZone::FrozenRight),
            LayoutColumn::new("side", 51.).with_zone(ColumnZone::FrozenRight),
        ];
        assert_eq!(merged, expected);

        // The report comes from the schema and the saved layout, not from the
        // merge.
        let report = LayoutReport::resolve(&context, &saved);
        assert_eq!(ids(report.ignored_columns()), ["gone"]);
        assert_eq!(ids(report.added_columns()), ["fee", "flag", "note"]);
        assert_eq!(ids(report.dropped_sort()), ["gone", "price", "zz"]);
        assert!(!report.is_exact());

        // Without saved columns the merge is the schema split by zone.
        let schema_only = AppendNewColumns.merge(&context, &GridLayout::new());
        let order: Vec<_> = schema_only.iter().map(|c| c.id().as_str()).collect();
        assert_eq!(
            order,
            ["id", "flag", "name", "fee", "price", "qty", "note", "side"]
        );

        // Applying the merged layout again is exact and changes nothing.
        let round_trip = merged
            .iter()
            .cloned()
            .fold(GridLayout::new(), GridLayout::with_column)
            .with_sort([LayoutSort::new("price", SortDirection::Ascending)]);
        assert!(LayoutReport::resolve(&context, &round_trip).is_exact());
        assert_eq!(AppendNewColumns.merge(&context, &round_trip), merged);
    }

    #[test]
    fn test_layout_library() {
        let trades = |width: f32| {
            GridLayout::new()
                .with_view_id("trades")
                .with_column(LayoutColumn::new("price", width))
        };
        let names = |library: &LayoutLibrary, view_id: &str| -> Vec<String> {
            library.names(view_id).map(str::to_owned).collect()
        };

        let mut library = LayoutLibrary::new();
        assert_eq!(
            library.insert("Wide", GridLayout::new()),
            Err(LayoutError::MissingViewId)
        );
        library.insert("Wide", trades(200.)).unwrap();
        library.insert("Compact", trades(60.)).unwrap();
        library.insert("Wide", trades(240.)).unwrap();
        library
            .insert("Default", GridLayout::new().with_view_id("orders"))
            .unwrap();
        assert_eq!(names(&library, "trades"), ["Compact", "Wide"]);
        assert_eq!(names(&library, "orders"), ["Default"]);
        assert_eq!(names(&library, "positions"), Vec::<String>::new());
        assert_eq!(library.get("trades", "Wide"), Some(&trades(240.)));
        assert_eq!(library.get("orders", "Wide"), None);

        assert_eq!(library.default_layout("trades"), None);
        assert!(!library.set_default("trades", Some("Missing")));
        assert!(!library.set_default("positions", Some("Wide")));
        assert!(library.set_default("trades", Some("Compact")));
        assert_eq!(
            library.default_layout("trades"),
            Some(("Compact", &trades(60.)))
        );

        // Renaming keeps the default and never replaces another layout.
        assert!(!library.rename("trades", "Compact", "Wide"));
        assert!(!library.rename("trades", "Missing", "Other"));
        assert!(!library.rename("positions", "Compact", "Other"));
        assert!(library.rename("trades", "Compact", "Compact"));
        assert!(library.rename("trades", "Compact", "Narrow"));
        assert_eq!(names(&library, "trades"), ["Narrow", "Wide"]);
        assert_eq!(
            library.default_layout("trades"),
            Some(("Narrow", &trades(60.)))
        );

        // The library is plain serde data.
        let text = serde_json::to_string(&library).unwrap();
        assert_eq!(
            serde_json::from_str::<LayoutLibrary>(&text).unwrap(),
            library
        );
        let mut orders_only = LayoutLibrary::new();
        orders_only
            .insert("Default", GridLayout::new().with_view_id("orders"))
            .unwrap();
        assert_eq!(
            serde_json::to_string(&orders_only).unwrap(),
            r#"{"views":{"orders":{"layouts":{"Default":{"version":1,"view_id":"orders","columns":[]}}}}}"#
        );

        // Removing the default clears it; the last removal drops the view.
        assert_eq!(library.remove("trades", "Narrow"), Some(trades(60.)));
        assert_eq!(library.remove("trades", "Narrow"), None);
        assert_eq!(library.default_layout("trades"), None);
        assert!(library.set_default("trades", Some("Wide")));
        assert!(library.set_default("trades", None));
        assert_eq!(library.default_layout("trades"), None);
        assert_eq!(library.remove("trades", "Wide"), Some(trades(240.)));
        assert_eq!(names(&library, "trades"), Vec::<String>::new());
        assert!(library.set_default("trades", None));
        assert_eq!(
            library.get("orders", "Default").map(GridLayout::version),
            Some(1)
        );
    }
}
