//! The sky table: one grid over the store of objects and branches, shown
//! flat, grouped by constellation or as a tree (constellation → shell →
//! object). Sorted by elevation, highest first, so what is overhead is on
//! top; a rise or a set flashes its row.

use std::{fmt::Write as _, sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla, px};
use gpui_kit_grid::{
    Aggregate, AggregateOrder, CellChange, CellStyle, CellStyleContext, ChangeDirection,
    ChangePaint, ChangePainter, ChangePalette, ColumnSet, ColumnZone, ExpandDepth, GridColumn,
    GridColumns as _, GridSchema, GroupBy, GroupCaptionContext, HighlightPolicy, LiveGridDelegate,
    OwnedValue, RowChange, SortDirection, StoreOptions, TreeSpec, ViewSpec,
};

use super::rows::{Body, SkyColumn, SkyRow};

/// How long a rise or a set shows.
const FLASH: Duration = Duration::from_millis(2_500);
/// The opacity of a flash at full intensity.
const FLASH_TINT: f32 = 0.30;
/// A leaf's own count in the tree's count columns: there, but quiet.
const LEAF_COUNT_OPACITY: f32 = 0.45;
/// The tree opens on the constellations' shells.
pub const TREE_DEPTH: ExpandDepth = ExpandDepth::Levels(1);

/// The store's options: a flash for each rise and set.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 6))
}

/// How the table arranges the objects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arrangement {
    #[default]
    Flat,
    Grouped,
    Tree,
}

impl Arrangement {
    pub const ALL: [Self; 3] = [Self::Flat, Self::Grouped, Self::Tree];

    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "Flat",
            Self::Grouped => "By constellation",
            Self::Tree => "Tree",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    /// The grouping key: flat and grouped trade places; the tree goes to
    /// the grouping.
    pub fn grouping_toggled(self) -> Self {
        match self {
            Self::Grouped => Self::Flat,
            Self::Flat | Self::Tree => Self::Grouped,
        }
    }

    /// The tree key: into the tree, or out of it to the flat table.
    pub fn tree_toggled(self) -> Self {
        match self {
            Self::Tree => Self::Flat,
            Self::Flat | Self::Grouped => Self::Tree,
        }
    }

    /// The columns shown only in some arrangements, and whether this one
    /// shows them: the constellation when it is neither the group nor the
    /// tree's top level, the branches' counts in the tree.
    pub fn column_visibility(self) -> [(SkyColumn, bool); 3] {
        [
            (SkyColumn::Constellation, self == Self::Flat),
            (SkyColumn::Objects, self == Self::Tree),
            (SkyColumn::UpCount, self == Self::Tree),
        ]
    }
}

/// The sort at start: elevation, highest first.
pub fn default_sort() -> Vec<(SkyColumn, SortDirection)> {
    vec![(SkyColumn::Elevation, SortDirection::Descending)]
}

/// The view of `arrangement` with `sort`, and only the objects above the
/// horizon if `up_only`. Flat and grouped show the objects alone; the tree
/// keeps the branches of the objects it shows.
pub fn view(
    arrangement: Arrangement,
    up_only: bool,
    sort: Vec<(SkyColumn, SortDirection)>,
) -> ViewSpec<SkyRow> {
    let mut depends_on = ColumnSet::EMPTY;
    depends_on.insert(SkyColumn::Pass.ix());
    let view = ViewSpec::new().with_sort(sort);
    match arrangement {
        Arrangement::Flat | Arrangement::Grouped => {
            let view = if up_only {
                view.with_filter(depends_on, SkyRow::is_up)
            } else {
                view.with_filter(depends_on, SkyRow::is_object)
            };
            if arrangement == Arrangement::Flat {
                return view;
            }
            view.with_group_by([GroupBy::column(SkyColumn::Constellation)])
                .with_group_order(AggregateOrder::new(
                    SkyColumn::UpCount,
                    SortDirection::Descending,
                ))
                .with_aggregates([
                    (SkyColumn::UpCount, Aggregate::Sum),
                    (SkyColumn::UpRange, Aggregate::Min),
                ])
        }
        Arrangement::Tree => {
            let view = view
                .with_tree(TreeSpec::new().with_column(SkyColumn::Name))
                .with_aggregates([
                    (SkyColumn::Objects, Aggregate::Sum),
                    (SkyColumn::UpCount, Aggregate::Sum),
                ]);
            if up_only {
                view.with_filter(depends_on, SkyRow::is_up)
            } else {
                view
            }
        }
    }
}

fn column(column: SkyColumn, title: &'static str, width: f32) -> GridColumn<SkyColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

/// The name frozen on the left; the shell, the tree's counts and the
/// caption's nearest range hidden until an arrangement shows them.
pub fn schema() -> GridSchema<SkyColumn> {
    GridSchema::new()
        .column(column(SkyColumn::Name, "Name", 200.).with_zone(ColumnZone::FrozenLeft))
        .column(column(SkyColumn::Norad, "NORAD", 72.))
        .column(column(SkyColumn::Constellation, "Constellation", 110.))
        .column(column(SkyColumn::Shell, "Shell", 170.).with_visible(false))
        .column(column(SkyColumn::Objects, "Objects", 76.).with_visible(false))
        .column(column(SkyColumn::UpCount, "Up", 56.).with_visible(false))
        .column(column(SkyColumn::Azimuth, "Azimuth", 80.))
        .column(column(SkyColumn::Elevation, "Elevation", 102.))
        .column(column(SkyColumn::Range, "Range km", 86.))
        .column(column(SkyColumn::RangeRate, "Rate km/s", 96.))
        .column(column(SkyColumn::Altitude, "Alt km", 70.))
        .column(column(SkyColumn::Speed, "km/s", 64.))
        .column(column(SkyColumn::Sunlit, "Sunlit", 88.))
        .column(column(SkyColumn::Pass, "Up/Down", 84.))
        .column(column(SkyColumn::UpRange, "Nearest up", 40.).with_visible(false))
}

/// The sky table's hooks, colours from the theme only: an object above
/// the horizon in the foreground and one below it muted, up in `success`
/// and sunlit in `warning` (their status), shadow in `info`; the range
/// rate's arrow points down while the object approaches. Captions count
/// the group's objects, those up and the nearest. They take `&self`,
/// never read the grid and never allocate.
pub struct SkyTable;

impl LiveGridDelegate for SkyTable {
    type Row = SkyRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, SkyRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        let column = cell.column();
        match &row.body {
            Body::Branch(roll_up) => match column {
                SkyColumn::Name if row.parent.is_none() => {
                    style.set_font_weight(gpui_kit::FontWeight::BOLD)
                }
                SkyColumn::UpCount if roll_up.up > 0 => style.set_foreground(theme.success),
                SkyColumn::Name | SkyColumn::Objects | SkyColumn::Elevation | SkyColumn::Range => {}
                _ => style.set_foreground(theme.muted_foreground),
            },
            Body::Object(sighting) => {
                let below = theme.muted_foreground;
                match column {
                    SkyColumn::Pass if sighting.is_up() => {
                        style.set_foreground(theme.success);
                        style.set_font_weight(gpui_kit::FontWeight::BOLD);
                    }
                    SkyColumn::Sunlit if sighting.sunlit => style.set_foreground(theme.warning),
                    SkyColumn::Sunlit => style.set_foreground(theme.info),
                    SkyColumn::RangeRate => {
                        style.set_trend(Some(if sighting.range_rate < 0. {
                            ChangeDirection::Down
                        } else {
                            ChangeDirection::Up
                        }));
                        if !sighting.is_up() {
                            style.set_foreground(below);
                        }
                    }
                    SkyColumn::Objects | SkyColumn::UpCount => {
                        style.set_foreground(below.opacity(LEAF_COUNT_OPACITY))
                    }
                    SkyColumn::Norad | SkyColumn::Constellation => style.set_foreground(below),
                    SkyColumn::Name if sighting.is_up() => {}
                    _ if !sighting.is_up() => style.set_foreground(below),
                    _ => {}
                }
            }
        }
    }

    fn group_caption(&self, caption: &GroupCaptionContext<'_, SkyRow>, out: &mut String, _: &App) {
        let int = |column: SkyColumn| match caption.aggregate(column) {
            Some(OwnedValue::Int(value)) => Some(*value),
            _ => None,
        };
        let _ = write!(out, "{}  ·  ", caption.value_text());
        push_thousands(out, caption.row_count() as u64);
        out.push_str(" objects  ·  ");
        push_thousands(out, int(SkyColumn::UpCount).unwrap_or(0).max(0) as u64);
        out.push_str(" up");
        if let Some(nearest) = int(SkyColumn::UpRange) {
            out.push_str("  ·  nearest ");
            push_thousands(out, nearest.max(0) as u64);
            out.push_str(" km");
        }
    }
}

/// Appends `5,000` for 5000.
fn push_thousands(out: &mut String, value: u64) {
    let mut divisor = 1;
    while divisor * 1_000 <= value {
        divisor *= 1_000;
    }
    let _ = write!(out, "{}", value / divisor);
    while divisor > 1 {
        divisor /= 1_000;
        let _ = write!(out, ",{:03}", value / divisor % 1_000);
    }
}

/// Paints a rise or a set: the row's changed cells tinted, a rise in the
/// theme's `success` (up is that status here) and a set in its muted
/// foreground, fading. Every other change (an object moves each second)
/// is written without highlight, and a pinned row's moves have no
/// direction: they are not painted.
#[derive(Clone, Copy, Debug)]
pub struct RiseSetChanges {
    rise: Hsla,
    set: Hsla,
}

impl ChangePainter for RiseSetChanges {
    fn paint(&self, change: &CellChange, _: &ChangePalette, style: &mut CellStyle) {
        let color = match (change.change(), change.direction()) {
            (RowChange::Removed, _) => return,
            (_, Some(ChangeDirection::Up)) => self.rise,
            (_, Some(ChangeDirection::Down)) => self.set,
            (_, None) => return,
        };
        style.set_background(color.opacity(FLASH_TINT * change.intensity()));
    }
}

/// [`RiseSetChanges`] in the theme's colours, as a grid's change paint.
pub fn rise_set_changes(cx: &App) -> ChangePaint {
    let theme = cx.theme();
    ChangePaint::Custom(Arc::new(RiseSetChanges {
        rise: theme.success,
        set: theme.muted_foreground,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_thousands() {
        for (value, text) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1,000"),
            (16_872, "16,872"),
            (1_002_003, "1,002,003"),
        ] {
            let mut out = String::new();
            push_thousands(&mut out, value);
            assert_eq!(out, text);
        }
    }

    #[test]
    fn test_arrangements_toggle_and_show_their_columns() {
        assert_eq!(Arrangement::Flat.grouping_toggled(), Arrangement::Grouped);
        assert_eq!(Arrangement::Grouped.grouping_toggled(), Arrangement::Flat);
        assert_eq!(Arrangement::Tree.grouping_toggled(), Arrangement::Grouped);
        assert_eq!(Arrangement::Tree.tree_toggled(), Arrangement::Flat);
        assert_eq!(Arrangement::Grouped.tree_toggled(), Arrangement::Tree);
        let shown = |arrangement: Arrangement, column: SkyColumn| {
            arrangement
                .column_visibility()
                .iter()
                .any(|(shown, visible)| *shown == column && *visible)
        };
        assert!(shown(Arrangement::Flat, SkyColumn::Constellation));
        assert!(!shown(Arrangement::Grouped, SkyColumn::Constellation));
        assert!(shown(Arrangement::Tree, SkyColumn::Objects));
        assert!(!shown(Arrangement::Flat, SkyColumn::UpCount));
    }
}
