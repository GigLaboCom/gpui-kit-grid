//! The station board: a second grid with the departures from one station
//! by the timetable, "Scheduled departures" (no delays: the example uses
//! no live data for Munich). One row per departing run, keyed as its
//! vehicle is, so a row that comes onto the board flashes, its countdown
//! ticks silently, and it fades out as the train departs. Sorted by the
//! countdown. The producer writes it (`producer.rs`); `app.rs` places it
//! under the map. The rows have no GPUI types.

use std::{sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement as _, ParentElement as _, SharedString, Styled as _,
    Window, div, px,
};
use gpui_kit_grid::{
    CellContent, CellElementContext, CellStyle, CellStyleContext, CellValue, ColumnAlign, ColumnIx,
    ColumnSet, ColumnZone, GridColumn, GridColumns, GridRow, GridSchema, HighlightPolicy,
    LiveGridDelegate, RowChange, SortDirection, StoreOptions, ViewSpec,
};

use super::{
    rows::Countdown,
    source::{Departure, Line, VehicleKey},
    table::line_colors,
};

/// The most departures a board shows.
pub const BOARD_ROWS: usize = 24;
/// A departure this close is boarding: its countdown bold.
const BOARDING: i32 = 60;
const FLASH: Duration = Duration::from_millis(1_500);
const FADE: Duration = Duration::from_millis(1_200);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoardColumn {
    Line,
    Destination,
    Platform,
    /// The countdown to the departure.
    Departs,
    NextStop,
    /// Minutes from the departure to the next stop.
    ToNext,
}

impl GridColumns for BoardColumn {
    fn all() -> &'static [Self] {
        use BoardColumn::*;
        &[Line, Destination, Platform, Departs, NextStop, ToNext]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Destination => "destination",
            Self::Platform => "platform",
            Self::Departs => "departs",
            Self::NextStop => "next_stop",
            Self::ToNext => "to_next",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// One departure on the board, as of the producer's last sweep.
#[derive(Clone, Debug, PartialEq)]
pub struct BoardRow {
    pub key: VehicleKey,
    pub line: Arc<Line>,
    pub headsign: Arc<str>,
    pub platform: Arc<str>,
    pub departs: Countdown,
    pub next_stop: Arc<str>,
    /// Whole minutes to the next stop, at least 1.
    pub to_next_min: u16,
}

impl BoardRow {
    /// The row of `departure` at `now_ms`.
    pub fn new(departure: &Departure, now_ms: f64) -> Self {
        Self {
            key: departure.key,
            line: departure.line.clone(),
            headsign: departure.headsign.clone(),
            platform: departure.platform.clone(),
            departs: Countdown::between(now_ms, departure.depart_ms),
            next_stop: departure.next_stop.clone(),
            to_next_min: departure
                .to_next_s
                .div_ceil(60)
                .clamp(1, u32::from(u16::MAX)) as u16,
        }
    }
}

impl GridRow for BoardRow {
    type Key = VehicleKey;
    type Column = BoardColumn;

    fn key(&self) -> &VehicleKey {
        &self.key
    }

    fn value(&self, column: BoardColumn) -> CellValue<'_> {
        match column {
            BoardColumn::Line => CellValue::Custom(self.line.as_ref()),
            BoardColumn::Destination => CellValue::Text(&self.headsign),
            BoardColumn::Platform if self.platform.is_empty() => CellValue::Empty,
            BoardColumn::Platform => CellValue::Text(&self.platform),
            BoardColumn::Departs => CellValue::Custom(&self.departs),
            BoardColumn::NextStop => CellValue::Text(&self.next_stop),
            BoardColumn::ToNext => CellValue::Int(i64::from(self.to_next_min)),
        }
    }

    /// Only the countdown changes from sweep to sweep.
    fn changed_columns(&self, previous: &Self, out: &mut ColumnSet) {
        if self.departs != previous.departs {
            out.insert(BoardColumn::Departs.ix());
        }
        if self.line.rank != previous.line.rank
            || self.headsign != previous.headsign
            || self.platform != previous.platform
            || self.next_stop != previous.next_stop
            || self.to_next_min != previous.to_next_min
        {
            for column in [
                BoardColumn::Line,
                BoardColumn::Destination,
                BoardColumn::Platform,
                BoardColumn::NextStop,
                BoardColumn::ToNext,
            ] {
                if self.value(column) != previous.value(column) {
                    out.insert(column.ix());
                }
            }
        }
    }
}

/// A flash for each departure that comes onto the board, a fade for each
/// that leaves it.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 6).with_removal_fade(FADE))
}

/// Soonest first.
pub fn view() -> ViewSpec<BoardRow> {
    ViewSpec::new().with_sort(vec![(BoardColumn::Departs, SortDirection::Ascending)])
}

fn column(column: BoardColumn, title: &'static str, width: f32) -> GridColumn<BoardColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

/// The line frozen on the left, as in the vehicles table.
pub fn schema() -> GridSchema<BoardColumn> {
    GridSchema::new()
        .column(
            column(BoardColumn::Line, "Line", 56.)
                .with_zone(ColumnZone::FrozenLeft)
                .with_align(ColumnAlign::Start)
                .with_content(CellContent::Element),
        )
        .column(column(BoardColumn::Destination, "Destination", 150.))
        .column(column(BoardColumn::Platform, "Pl.", 44.).with_align(ColumnAlign::Start))
        .column(column(BoardColumn::Departs, "In", 60.).with_align(ColumnAlign::End))
        .column(column(BoardColumn::NextStop, "Next stop", 140.))
        .column(column(BoardColumn::ToNext, "Min", 44.))
}

/// The board's hooks: the line a pill in its colours, a boarding
/// departure's countdown bold, the next stop and its minutes quieter, a
/// departed row's text fading. Colours from the theme and the data only;
/// the styles never allocate (the pill, an element, does).
pub struct Board;

impl LiveGridDelegate for Board {
    type Row = BoardRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, BoardRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        match cell.column() {
            BoardColumn::Departs if row.departs.seconds() <= BOARDING => {
                style.set_font_weight(FontWeight::BOLD);
            }
            BoardColumn::NextStop | BoardColumn::ToNext => {
                style.set_foreground(theme.muted_foreground)
            }
            _ => {}
        }
        if let Some(change) = cell.change()
            && change.change() == RowChange::Removed
        {
            style.set_foreground(change.fade(theme.muted_foreground));
        }
    }

    fn render_cell(
        &self,
        cell: &CellElementContext<'_, BoardRow>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        if cell.column() != BoardColumn::Line {
            return None;
        }
        let row = cell.row();
        let (color, text) = line_colors(&row.line, cx.theme());
        let opacity = cell
            .change()
            .filter(|change| change.change() == RowChange::Removed)
            .map_or(1., |change| change.intensity());
        Some(
            div()
                .h_full()
                .flex()
                .items_center()
                .child(
                    div()
                        .min_w(px(30.))
                        .px(px(4.))
                        .py(px(1.))
                        .rounded(px(3.))
                        .bg(color.opacity(opacity))
                        .text_color(text.opacity(opacity))
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .justify_center()
                        .child(SharedString::from(row.line.name.clone())),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::source::Mode;
    use super::*;

    fn departure(depart_ms: f64, to_next_s: u32) -> Departure {
        Departure {
            key: VehicleKey::new(3),
            line: Arc::new(Line {
                mode: Mode::SBahn,
                name: "S3".into(),
                color: Some(0x8e3b5e),
                text_color: None,
                rank: 5,
            }),
            headsign: "Holzkirchen".into(),
            platform: "2".into(),
            depart_ms,
            next_stop: "Isartor".into(),
            to_next_s,
        }
    }

    #[test]
    fn test_rows_count_down_and_round_the_minutes_up() {
        let row = BoardRow::new(&departure(130_000., 90), 10_000.);
        assert_eq!(row.departs.seconds(), 120);
        assert_eq!(row.to_next_min, 2);
        assert_eq!(BoardRow::new(&departure(0., 0), 0.).to_next_min, 1);
        assert_eq!(row.value(BoardColumn::Platform), CellValue::Text("2"));
        let later = BoardRow::new(&departure(130_000., 90), 11_000.);
        let mut changed = ColumnSet::EMPTY;
        later.changed_columns(&row, &mut changed);
        for &column in BoardColumn::all() {
            let differs = later.value(column) != row.value(column);
            assert_eq!(changed.contains(column.ix()), differs, "{column:?}");
        }
        let mut no_platform = departure(0., 60);
        no_platform.platform = "".into();
        assert_eq!(
            BoardRow::new(&no_platform, 0.).value(BoardColumn::Platform),
            CellValue::Empty
        );
    }
}
