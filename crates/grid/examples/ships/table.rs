//! The vessels table: one grid over the fleet's rows, flat or grouped by
//! ship type or navigational status, sorted by speed (fastest first) so
//! the order moves as vessels speed up and slow down. Each change flashes
//! its cells; the speed carries an arrow. A vessel gone quiet dims, and
//! one gone for good fades out.

use std::{fmt::Write as _, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement as _, ParentElement as _, Styled as _, Window, div, px,
};
use gpui_kit_grid::{
    Aggregate, AggregateOrder, CellContent, CellElementContext, CellStyle, CellStyleContext,
    ColumnAlign, ColumnSet, ColumnZone, GridColumn, GridColumns as _, GridSchema, GroupBy,
    GroupCaptionContext, HighlightPolicy, LiveGridDelegate, NumberFormat, OwnedValue,
    SortDirection, StoreOptions, ViewSpec,
};

use super::{
    rows::{ShipColumn, VesselRow},
    theme::category_color,
};

/// How long a change shows, and a removed vessel takes to fade out.
const FLASH: Duration = Duration::from_millis(1_800);
const FADE_OUT: Duration = Duration::from_millis(2_000);
/// The opacity of a quiet vessel's text.
const STALE_OPACITY: f32 = 0.45;
/// Knots from which a speed is bold: high-speed craft.
const FAST: f64 = 20.;

/// The store's options: a flash for each change, a fade for each vessel
/// that leaves.
pub fn store_options() -> StoreOptions {
    StoreOptions::default()
        .with_highlight(HighlightPolicy::new(FLASH, 6).with_removal_fade(FADE_OUT))
}

/// How the table groups the vessels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    #[default]
    None,
    Category,
    Status,
}

impl Grouping {
    pub const ALL: [Self; 3] = [Self::None, Self::Category, Self::Status];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Flat",
            Self::Category => "By type",
            Self::Status => "By status",
        }
    }

    /// The same in a word, for a phone.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::None => "Flat",
            Self::Category => "Type",
            Self::Status => "Status",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    /// Flat, by type, by status, flat again.
    pub fn next(self) -> Self {
        match self {
            Self::None => Self::Category,
            Self::Category => Self::Status,
            Self::Status => Self::None,
        }
    }

    /// The grouped column, which the group's caption names instead.
    fn column(self) -> Option<ShipColumn> {
        match self {
            Self::None => None,
            Self::Category => Some(ShipColumn::Category),
            Self::Status => Some(ShipColumn::Status),
        }
    }

    /// The columns this grouping hides: the one the captions name.
    pub fn column_visibility(self) -> [(ShipColumn, bool); 2] {
        [
            (ShipColumn::Category, self != Self::Category),
            (ShipColumn::Status, self != Self::Status),
        ]
    }
}

/// The sort at start: fastest first.
pub fn default_sort() -> Vec<(ShipColumn, SortDirection)> {
    vec![(ShipColumn::Speed, SortDirection::Descending)]
}

/// The view grouped by `grouping`, sorted by `sort`, and only the moving
/// vessels if `moving_only`. Groups come largest first, each caption with
/// its moving and berthed vessels and their average speed.
pub fn view(
    grouping: Grouping,
    moving_only: bool,
    sort: Vec<(ShipColumn, SortDirection)>,
) -> ViewSpec<VesselRow> {
    let view = ViewSpec::new().with_sort(sort);
    let view = if moving_only {
        let mut depends_on = ColumnSet::EMPTY;
        depends_on.insert(ShipColumn::Moving.ix());
        view.with_filter(depends_on, |row: &VesselRow| row.moving)
    } else {
        view
    };
    let Some(column) = grouping.column() else {
        return view;
    };
    view.with_group_by([GroupBy::column(column)])
        .with_group_order(AggregateOrder::new(
            ShipColumn::Vessels,
            SortDirection::Descending,
        ))
        .with_aggregates([
            (ShipColumn::Vessels, Aggregate::Sum),
            (ShipColumn::Moving, Aggregate::Sum),
            (ShipColumn::Berthed, Aggregate::Sum),
            (ShipColumn::MovingSpeed, Aggregate::Avg),
        ])
}

fn column(column: ShipColumn, title: &'static str, width: f32) -> GridColumn<ShipColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

/// The name frozen on the left; knots and draughts to a tenth, positions
/// to four places; the counters the captions read internal.
pub fn schema() -> GridSchema<ShipColumn> {
    let number = |id, title, width| column(id, title, width).with_align(ColumnAlign::End);
    let decimals = |id, title, width, decimals| {
        number(id, title, width).with_number_format(NumberFormat::new().with_decimals(decimals))
    };
    let hidden = |id, title| column(id, title, 60.).with_internal(true);
    GridSchema::new()
        .column(column(ShipColumn::Name, "Name", 176.).with_zone(ColumnZone::FrozenLeft))
        .column(column(ShipColumn::Category, "Type", 92.))
        .column(column(ShipColumn::Status, "Status", 128.))
        // An element only where the speed is unknown: a dimmed dash.
        .column(decimals(ShipColumn::Speed, "kn", 62., 1).with_content(CellContent::Element))
        .column(number(ShipColumn::Course, "Course", 66.))
        .column(number(ShipColumn::Heading, "Heading", 72.))
        .column(column(ShipColumn::Destination, "Destination", 132.))
        .column(column(ShipColumn::Eta, "ETA UTC", 104.))
        .column(number(ShipColumn::Length, "Length m", 76.))
        .column(decimals(ShipColumn::Draught, "Draught m", 82., 1))
        .column(column(ShipColumn::CallSign, "Call sign", 88.))
        .column(number(ShipColumn::Mmsi, "MMSI", 90.))
        .column(decimals(ShipColumn::Latitude, "Lat", 78., 4))
        .column(decimals(ShipColumn::Longitude, "Lon", 78., 4))
        .column(column(ShipColumn::Seen, "Seen UTC", 78.))
        .column(hidden(ShipColumn::Vessels, "Vessels"))
        .column(hidden(ShipColumn::Moving, "Moving"))
        .column(hidden(ShipColumn::Berthed, "Berthed"))
        .column(hidden(ShipColumn::MovingSpeed, "Moving speed"))
        .column(hidden(ShipColumn::Stale, "Quiet"))
}

/// The columns the search box looks in.
pub const SEARCHED: [ShipColumn; 4] = [
    ShipColumn::Name,
    ShipColumn::Destination,
    ShipColumn::CallSign,
    ShipColumn::Mmsi,
];

/// The table's hooks, colours from the theme only: the type in its
/// category's colour (as on the map), a status that asks others to keep
/// clear in `danger`, berthed vessels and identifiers muted, a high-speed
/// craft's speed bold, an unknown speed (none reported, or one the fleet's
/// `SpeedRule` does not believe) a dimmed dash, and a vessel gone quiet
/// dimmed whole. Captions
/// count the group's vessels, those moving and berthed, and their average
/// speed. They take `&self`, never read the grid and never allocate.
pub struct ShipTable;

impl LiveGridDelegate for ShipTable {
    type Row = VesselRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, VesselRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        if row.stale {
            style.set_foreground(theme.muted_foreground.opacity(STALE_OPACITY));
            return;
        }
        match cell.column() {
            ShipColumn::Category => style.set_foreground(category_color(row.category, theme)),
            ShipColumn::Status if row.status.is_alert() => style.set_foreground(theme.danger),
            // Berthed, or "under way" while lying still (a status left on).
            ShipColumn::Status
                if row.is_berthed() || (row.status.is_under_way() && !row.moving) =>
            {
                style.set_foreground(theme.muted_foreground)
            }
            ShipColumn::Speed => {
                if row.speed.is_some_and(|speed| speed >= FAST) {
                    style.set_font_weight(FontWeight::BOLD);
                } else if !row.moving {
                    style.set_foreground(theme.muted_foreground);
                }
            }
            ShipColumn::Course | ShipColumn::Heading if !row.moving => {
                style.set_foreground(theme.muted_foreground)
            }
            ShipColumn::Mmsi
            | ShipColumn::CallSign
            | ShipColumn::Latitude
            | ShipColumn::Longitude
            | ShipColumn::Seen => style.set_foreground(theme.muted_foreground),
            _ => {}
        }
    }

    /// An unknown speed as a dimmed dash; a known one as text.
    fn render_cell(
        &self,
        cell: &CellElementContext<'_, VesselRow>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        if cell.column() != ShipColumn::Speed || cell.row().speed.is_some() {
            return None;
        }
        let theme = cx.theme();
        Some(
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_end()
                .text_color(theme.muted_foreground.opacity(STALE_OPACITY))
                .child("—")
                .into_any_element(),
        )
    }

    fn group_caption(
        &self,
        caption: &GroupCaptionContext<'_, VesselRow>,
        out: &mut String,
        _: &App,
    ) {
        let int = |column: ShipColumn| match caption.aggregate(column) {
            Some(OwnedValue::Int(value)) => (*value).max(0) as u64,
            _ => 0,
        };
        let _ = write!(
            out,
            "{}  ·  {} vessel{}  ·  {} moving  ·  {} berthed",
            caption.value_text(),
            caption.row_count(),
            if caption.row_count() == 1 { "" } else { "s" },
            int(ShipColumn::Moving),
            int(ShipColumn::Berthed),
        );
        if let Some(OwnedValue::Float(speed)) = caption.aggregate(ShipColumn::MovingSpeed) {
            let _ = write!(out, "  ·  {speed:.1} kn average under way");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_groupings_cycle_and_hide_their_column() {
        assert_eq!(Grouping::None.next(), Grouping::Category);
        assert_eq!(Grouping::Status.next(), Grouping::None);
        let hidden = |grouping: Grouping| {
            grouping
                .column_visibility()
                .iter()
                .filter(|(_, visible)| !visible)
                .map(|(column, _)| *column)
                .collect::<Vec<_>>()
        };
        assert!(hidden(Grouping::None).is_empty());
        assert_eq!(hidden(Grouping::Category), [ShipColumn::Category]);
        assert_eq!(hidden(Grouping::Status), [ShipColumn::Status]);
    }
}
