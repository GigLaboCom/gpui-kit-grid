//! The vehicles table: one grid over the store of vehicles under way,
//! grouped by mode and line (each caption counts its vehicles, and the
//! Next column shows the soonest arrival under it), by mode alone, or
//! flat. Munich's, by the timetable, is sorted by the next arrival, so the
//! order changes every second; Helsinki's, live, by the delay (the
//! captions show the group's average). A new vehicle flashes, a departure
//! (a new next stop) flashes its leg, one that ends fades out.

use std::{fmt::Write as _, sync::Arc, time::Duration};

use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, Window, div, px, relative, rgb,
};
use gpui_kit_grid::{
    Aggregate, CaptionTag, CellChange, CellContent, CellElementContext, CellStyle,
    CellStyleContext, CellValue, ChangePaint, ChangePainter, ChangePalette, ColumnAlign,
    ColumnZone, GridColumn, GridColumns as _, GridSchema, GroupBy, GroupCaptionContext,
    HighlightPolicy, LiveGridDelegate, NumberFormat, OwnedValue, RowChange, SortDirection,
    StoreOptions, ViewSpec,
};

use super::{
    rows::{TransitColumn, VehicleRow},
    source::{City, Line, Mode},
};

/// How long a change shows, and a removed run fades.
const FLASH: Duration = Duration::from_millis(1_800);
const FADE: Duration = Duration::from_millis(1_200);
/// The opacity of a change's tint at full intensity.
const STARTED_TINT: f32 = 0.22;
const DEPARTED_TINT: f32 = 0.30;
/// A vehicle this close to its next station is arriving.
const ARRIVING: i32 = 30;
/// A live vehicle this late, seconds, is late.
const LATE: i64 = 180;

/// The store's options: a flash for each start and departure, a fade for
/// each run that ends.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 6).with_removal_fade(FADE))
}

/// How the table arranges the vehicles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arrangement {
    #[default]
    ByLine,
    ByMode,
    Flat,
}

impl Arrangement {
    pub const ALL: [Self; 3] = [Self::ByLine, Self::ByMode, Self::Flat];

    pub fn label(self) -> &'static str {
        match self {
            Self::ByLine => "By line",
            Self::ByMode => "By mode",
            Self::Flat => "Flat",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    /// The next one, for the grouping key.
    pub fn next(self) -> Self {
        match self {
            Self::ByLine => Self::ByMode,
            Self::ByMode => Self::Flat,
            Self::Flat => Self::ByLine,
        }
    }

    /// The mode column is shown only where it is not a group.
    pub fn shows_mode(self) -> bool {
        self == Self::Flat
    }
}

/// The sort at start: by the timetable the next arrival, soonest first;
/// live the delay, latest first.
pub fn default_sort(city: City) -> Vec<(TransitColumn, SortDirection)> {
    match city {
        City::Munich => vec![(TransitColumn::NextIn, SortDirection::Ascending)],
        City::Helsinki => vec![(TransitColumn::Delay, SortDirection::Descending)],
    }
}

/// The view of `arrangement` with `sort`. Groups follow the modes' and the
/// lines' own order (U-Bahn, S-Bahn, tram, bus; U1 before U2, 19 before
/// 100), their values' `cmp_value`; the captions' Next is the soonest
/// arrival under them, or live their delay the average.
pub fn view(
    arrangement: Arrangement,
    sort: Vec<(TransitColumn, SortDirection)>,
    city: City,
) -> ViewSpec<VehicleRow> {
    let view = ViewSpec::new().with_sort(sort);
    let levels = match arrangement {
        Arrangement::Flat => return view,
        Arrangement::ByMode => vec![GroupBy::column(TransitColumn::Mode)],
        Arrangement::ByLine => vec![
            GroupBy::column(TransitColumn::Mode),
            GroupBy::column(TransitColumn::Line),
        ],
    };
    let aggregate = match city {
        City::Munich => (TransitColumn::NextIn, Aggregate::Min),
        City::Helsinki => (TransitColumn::Delay, Aggregate::Avg),
    };
    view.with_group_by(levels).with_aggregates([aggregate])
}

fn column(column: TransitColumn, title: &'static str, width: f32) -> GridColumn<TransitColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

/// The line frozen on the left; the mode shown when the table is flat.
/// Munich's timetable fills the leg, the progress, the countdown and the
/// stations left; Helsinki's live feed the next stop, the delay, the
/// speed and the occupancy. The other city's columns are internal: never
/// shown, in no chooser and no export.
pub fn schema(city: City) -> GridSchema<TransitColumn> {
    let timetable = city == City::Munich;
    let live = !timetable;
    let number = |id, title, width| column(id, title, width).with_align(ColumnAlign::End);
    GridSchema::new()
        .column(
            column(TransitColumn::Line, "Line", 64.)
                .with_zone(ColumnZone::FrozenLeft)
                .with_align(ColumnAlign::Start)
                .with_content(CellContent::Element),
        )
        .column(column(TransitColumn::Mode, "Mode", 72.).with_visible(false))
        .column(column(TransitColumn::Destination, "Destination", 190.))
        .column(column(
            TransitColumn::Leg,
            if timetable {
                "From → to"
            } else {
                "Next stop"
            },
            if timetable { 310. } else { 220. },
        ))
        .column(
            column(TransitColumn::Progress, "Progress", 96.)
                .with_align(ColumnAlign::Start)
                .with_content(CellContent::Element)
                .with_internal(live),
        )
        .column(number(TransitColumn::NextIn, "Next", 64.).with_internal(live))
        .column(column(TransitColumn::StopsLeft, "Stops", 60.).with_internal(live))
        .column(number(TransitColumn::Delay, "Delay s", 86.).with_internal(timetable))
        .column(
            number(TransitColumn::Speed, "km/h", 64.)
                .with_number_format(NumberFormat::new().with_decimals(0))
                .with_internal(timetable),
        )
        .column(column(TransitColumn::Occupancy, "Load", 64.).with_internal(timetable))
}

/// The colour of a mode, from the theme's chart colours.
pub fn mode_color(mode: Mode, theme: &Theme) -> Hsla {
    match mode {
        Mode::UBahn | Mode::Metro => theme.chart_1,
        Mode::SBahn | Mode::Train => theme.chart_2,
        Mode::Tram => theme.chart_3,
        Mode::Bus | Mode::RegionalBus => theme.chart_4,
        Mode::Regional | Mode::Ferry => theme.chart_5,
    }
}

/// A line's colour and the colour of text on it: the data's own colours
/// (GTFS `route_color` is data), else its mode's from the theme with the
/// theme's background as text.
pub fn line_colors(line: &Line, theme: &Theme) -> (Hsla, Hsla) {
    match line.color {
        Some(color) => (
            rgb(color).into(),
            line.text_color
                .map_or(theme.background, |text| rgb(text).into()),
        ),
        None => (mode_color(line.mode, theme), theme.background),
    }
}

/// The table's hooks, colours from the theme and the data only: the line
/// a pill in its colour, the progress a bar in it, an arriving vehicle's
/// countdown bold, the mode and the stations left quieter. Captions name
/// the group (a line by its pill, as in its cells) and count its vehicles.
/// They take `&self`, never read the grid, and the styles and captions
/// never allocate (the pill and the bar in cells, elements, do). A run
/// that ends keeps its pill and bar while it fades.
pub struct Vehicles;

impl LiveGridDelegate for Vehicles {
    type Row = VehicleRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, VehicleRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        match cell.column() {
            TransitColumn::NextIn if row.next_in.seconds() <= ARRIVING => {
                style.set_font_weight(FontWeight::BOLD);
            }
            TransitColumn::Mode | TransitColumn::StopsLeft => {
                style.set_foreground(theme.muted_foreground)
            }
            TransitColumn::Delay => match cell.value() {
                CellValue::Int(delay) if delay >= LATE => {
                    style.set_foreground(theme.danger);
                    style.set_font_weight(FontWeight::BOLD);
                }
                CellValue::Int(delay) if delay <= 0 => style.set_foreground(theme.muted_foreground),
                _ => {}
            },
            TransitColumn::Speed | TransitColumn::Occupancy => {
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

    /// The line's pill and the leg's progress bar.
    fn render_cell(
        &self,
        cell: &CellElementContext<'_, VehicleRow>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let theme = cx.theme();
        let row = cell.row();
        let (color, text) = line_colors(&row.line, theme);
        let fading = cell
            .change()
            .filter(|change| change.change() == RowChange::Removed);
        let opacity = fading.map_or(1., |change| change.intensity());
        match cell.column() {
            TransitColumn::Line => Some(
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .min_w(px(34.))
                            .px(px(5.))
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
            ),
            TransitColumn::Progress => Some(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(4.))
                            .rounded(px(2.))
                            .bg(theme.border)
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .h_full()
                                    .rounded(px(2.))
                                    .w(relative(row.progress.share()))
                                    .bg(color.opacity(opacity)),
                            ),
                    )
                    .into_any_element(),
            ),
            _ => None,
        }
    }

    fn group_caption(
        &self,
        caption: &GroupCaptionContext<'_, VehicleRow>,
        out: &mut String,
        _: &App,
    ) {
        let count = caption.row_count();
        // A line's name is in its tag.
        if line_of(caption).is_some() {
            let _ = write!(out, "{count} under way");
        } else {
            let _ = write!(out, "{}  ·  {count} under way", caption.value_text());
        }
    }

    /// A line's caption starts with its pill, in the line's colours.
    fn group_caption_tag(
        &self,
        caption: &GroupCaptionContext<'_, VehicleRow>,
        out: &mut String,
        cx: &App,
    ) -> Option<CaptionTag> {
        let line = line_of(caption)?;
        let (color, text) = line_colors(line, cx.theme());
        out.push_str(&line.name);
        Some(
            CaptionTag::new(color, text)
                .with_font_weight(FontWeight::BOLD)
                .with_min_width(px(34.)),
        )
    }
}

/// The line of a caption of the line level: its key, kept as itself.
fn line_of<'a>(caption: &GroupCaptionContext<'a, VehicleRow>) -> Option<&'a Line> {
    match caption.value() {
        OwnedValue::Custom(value) => value.downcast_ref::<Line>(),
        _ => None,
    }
}

/// Paints a run's life: a new run's row tinted in the accent, a departure
/// tints its leg and its stations left (the two columns only a departure
/// changes), an ended run's text fades out. The countdown and the progress
/// change every sweep, silently.
#[derive(Clone, Copy, Debug)]
pub struct DepartureChanges {
    accent: Hsla,
}

impl ChangePainter for DepartureChanges {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        let departure = [TransitColumn::Leg.ix(), TransitColumn::StopsLeft.ix()];
        let tint = match change.change() {
            RowChange::Removed => {
                style.set_foreground(change.fade(palette.removed_foreground()));
                return;
            }
            RowChange::Inserted => STARTED_TINT,
            RowChange::Updated
                if change
                    .column_ix()
                    .is_some_and(|column| departure.contains(&column)) =>
            {
                DEPARTED_TINT
            }
            _ => return,
        };
        style.set_background(self.accent.opacity(tint * change.intensity()));
        style.set_trend(None);
    }
}

/// [`DepartureChanges`] in the theme's accent, as a grid's change paint.
pub fn departure_changes(cx: &App) -> ChangePaint {
    ChangePaint::Custom(Arc::new(DepartureChanges {
        accent: cx.theme().primary,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arrangements_cycle_and_group() {
        assert_eq!(Arrangement::ByLine.next(), Arrangement::ByMode);
        assert_eq!(Arrangement::Flat.next(), Arrangement::ByLine);
        assert!(Arrangement::Flat.shows_mode());
        assert!(!Arrangement::ByLine.shows_mode());
        let sort = || default_sort(City::Munich);
        assert_eq!(
            view(Arrangement::ByLine, sort(), City::Munich)
                .group_by()
                .len(),
            2
        );
        assert_eq!(
            view(Arrangement::ByMode, sort(), City::Munich)
                .group_by()
                .len(),
            1
        );
        assert!(
            view(Arrangement::Flat, sort(), City::Munich)
                .group_by()
                .is_empty()
        );
        assert_eq!(
            default_sort(City::Helsinki),
            [(TransitColumn::Delay, SortDirection::Descending)]
        );
    }
}
