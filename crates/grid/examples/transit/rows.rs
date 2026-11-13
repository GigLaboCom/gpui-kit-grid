//! The rows the producer writes and the table shows: one per vehicle under
//! way, keyed by its run ([`VehicleKey`]: the timetable's trip on its
//! service day, or a live vehicle by its operator and number). A live
//! vehicle fills the delay, speed and occupancy instead of the progress,
//! countdown and stations left. No GPUI types; `value` borrows everything
//! it shows, so it never allocates.

use std::{any::TypeId, cmp::Ordering, fmt::Write as _, sync::Arc};

use gpui_kit_grid::{CellValue, ColumnIx, ColumnSet, GridColumns, GridRow, GridValue};

#[cfg(test)]
use super::source::Occupancy;
use super::source::{Line, LiveState, Vehicle, VehicleKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransitColumn {
    Mode,
    Line,
    Destination,
    /// The leg: the station left last and the next one.
    Leg,
    /// Along the leg, 0 to 1.
    Progress,
    /// The time to the next station.
    NextIn,
    StopsLeft,
    /// A live vehicle's delay, seconds behind the timetable.
    Delay,
    /// A live vehicle's speed, km/h.
    Speed,
    /// How full a live vehicle is, where the feed says.
    Occupancy,
}

impl GridColumns for TransitColumn {
    fn all() -> &'static [Self] {
        use TransitColumn::*;
        &[
            Mode,
            Line,
            Destination,
            Leg,
            Progress,
            NextIn,
            StopsLeft,
            Delay,
            Speed,
            Occupancy,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Mode => "mode",
            Self::Line => "line",
            Self::Destination => "destination",
            Self::Leg => "leg",
            Self::Progress => "progress",
            Self::NextIn => "next_in",
            Self::StopsLeft => "stops_left",
            Self::Delay => "delay",
            Self::Speed => "speed",
            Self::Occupancy => "occupancy",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// Seconds to the next station, shown `2:05`: a cell value of its own, so
/// it sorts by time, copies as seconds and reads as minutes and seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Countdown(i32);

impl Countdown {
    /// The whole seconds from `now_ms` to `at_ms`, never below zero.
    pub fn between(now_ms: f64, at_ms: f64) -> Self {
        Self(((at_ms - now_ms) / 1e3).ceil().max(0.) as i32)
    }

    #[cfg(test)]
    pub const fn from_seconds(seconds: i32) -> Self {
        Self(seconds)
    }

    pub fn seconds(self) -> i32 {
        self.0
    }

    /// `2:05`, `0:45`, `1:02:05`.
    pub fn write(self, out: &mut String) {
        let seconds = self.0.max(0);
        let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
        let _ = if hours > 0 {
            write!(out, "{hours}:{minutes:02}:{seconds:02}")
        } else {
            write!(out, "{minutes}:{seconds:02}")
        };
    }
}

impl GridValue for Countdown {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        match other.downcast_ref::<Countdown>() {
            Some(other) => self.cmp(other),
            None => TypeId::of::<Self>().cmp(&other.as_any().type_id()),
        }
    }

    fn write_text(&self, out: &mut String) {
        self.write(out);
    }

    fn export_value(&self) -> Option<CellValue<'_>> {
        Some(CellValue::Int(i64::from(self.0)))
    }
}

/// How far along its leg, in whole percent, shown `45%`: a cell value of
/// its own, so a copy and the quick filter read a share, not `0.45`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Percent(u8);

impl Percent {
    /// `share` (0 to 1) in whole percent.
    pub fn of(share: f32) -> Self {
        Self((share.clamp(0., 1.) * 100.).round() as u8)
    }

    /// The share, 0 to 1.
    pub fn share(self) -> f32 {
        f32::from(self.0) / 100.
    }
}

impl GridValue for Percent {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        match other.downcast_ref::<Percent>() {
            Some(other) => self.cmp(other),
            None => TypeId::of::<Self>().cmp(&other.as_any().type_id()),
        }
    }

    fn write_text(&self, out: &mut String) {
        let _ = write!(out, "{}%", self.0);
    }

    fn export_value(&self) -> Option<CellValue<'_>> {
        Some(CellValue::Int(i64::from(self.0)))
    }
}

/// One vehicle under way, as of the producer's last sweep.
#[derive(Clone, Debug, PartialEq)]
pub struct VehicleRow {
    pub key: VehicleKey,
    pub line: Arc<Line>,
    pub headsign: Arc<str>,
    pub leg: Arc<str>,
    pub leg_ix: u16,
    pub progress: Percent,
    pub next_in: Countdown,
    pub stops_left: u16,
    /// What a live vehicle reports; `None` by the timetable.
    pub live: Option<LiveState>,
}

impl VehicleRow {
    /// The row of `vehicle` at `now_ms`.
    pub fn new(vehicle: &Vehicle, now_ms: f64) -> Self {
        Self {
            key: vehicle.key,
            line: vehicle.line.clone(),
            headsign: vehicle.headsign.clone(),
            leg: vehicle.leg.clone(),
            leg_ix: vehicle.leg_ix,
            // Steps of a percent: a bar a few pixels wide does not need
            // more, and fewer distinct values are fewer writes.
            progress: Percent::of(vehicle.progress),
            next_in: Countdown::between(now_ms, vehicle.next_arrival_ms),
            stops_left: vehicle.stops_left,
            live: vehicle.live,
        }
    }

    /// Has it left a station since `previous`?
    pub fn departed_since(&self, previous: &Self) -> bool {
        self.leg_ix != previous.leg_ix || self.leg != previous.leg
    }
}

impl GridRow for VehicleRow {
    type Key = VehicleKey;
    type Column = TransitColumn;

    fn key(&self) -> &VehicleKey {
        &self.key
    }

    fn value(&self, column: TransitColumn) -> CellValue<'_> {
        match column {
            TransitColumn::Mode => CellValue::Custom(&self.line.mode),
            TransitColumn::Line => CellValue::Custom(self.line.as_ref()),
            TransitColumn::Destination => CellValue::Text(&self.headsign),
            TransitColumn::Leg => CellValue::Text(&self.leg),
            TransitColumn::Progress => CellValue::Custom(&self.progress),
            TransitColumn::NextIn => CellValue::Custom(&self.next_in),
            TransitColumn::StopsLeft => CellValue::Int(i64::from(self.stops_left)),
            TransitColumn::Delay => self
                .live
                .and_then(|live| live.delay_s)
                .map_or(CellValue::Empty, |delay| CellValue::Int(i64::from(delay))),
            TransitColumn::Speed => self
                .live
                .and_then(|live| live.speed_kmh)
                .map_or(CellValue::Empty, |speed| CellValue::Float(f64::from(speed))),
            TransitColumn::Occupancy => match &self.live {
                Some(LiveState {
                    occupancy: Some(occupancy),
                    ..
                }) if occupancy.0 > 0 => CellValue::Custom(occupancy),
                _ => CellValue::Empty,
            },
        }
    }

    /// The fields a vehicle changes every sweep, compared directly.
    fn changed_columns(&self, previous: &Self, out: &mut ColumnSet) {
        let mut changed = |column: TransitColumn, differs: bool| {
            if differs {
                out.insert(column.ix());
            }
        };
        changed(TransitColumn::Mode, self.line.mode != previous.line.mode);
        changed(TransitColumn::Line, self.line.rank != previous.line.rank);
        changed(
            TransitColumn::Destination,
            self.headsign != previous.headsign,
        );
        changed(TransitColumn::Leg, self.leg != previous.leg);
        changed(TransitColumn::Progress, self.progress != previous.progress);
        changed(TransitColumn::NextIn, self.next_in != previous.next_in);
        changed(
            TransitColumn::StopsLeft,
            self.stops_left != previous.stops_left,
        );
        let live = |row: &Self| row.live.unwrap_or_default();
        let (now, before) = (live(self), live(previous));
        changed(TransitColumn::Delay, now.delay_s != before.delay_s);
        changed(TransitColumn::Speed, now.speed_kmh != before.speed_kmh);
        changed(TransitColumn::Occupancy, now.occupancy != before.occupancy);
    }
}

#[cfg(test)]
mod tests {
    use super::super::source::{GeoPoint, Mode};
    use super::*;

    fn row(leg: &str, leg_ix: u16, progress: f32, next_in: i32) -> VehicleRow {
        let progress = Percent::of(progress);
        VehicleRow {
            key: VehicleKey::new(7),
            line: Arc::new(Line {
                mode: Mode::Tram,
                name: "19".into(),
                color: None,
                text_color: None,
                rank: 20,
            }),
            headsign: "Romanplatz".into(),
            leg: leg.into(),
            leg_ix,
            progress,
            next_in: Countdown::from_seconds(next_in),
            stops_left: 9 - leg_ix,
            live: None,
        }
    }

    #[test]
    fn test_changed_columns_match_the_values() {
        let before = row("A → B", 0, 0.5, 30);
        let live = |delay_s, speed_kmh, occupancy| VehicleRow {
            live: Some(LiveState {
                delay_s,
                speed_kmh,
                occupancy,
            }),
            ..before.clone()
        };
        for after in [
            row("B → C", 1, 0., 90),
            row("A → B", 0, 0.75, 15),
            before.clone(),
            live(Some(30), Some(22.5), None),
            live(None, None, Some(Occupancy(100))),
        ] {
            let mut fast = ColumnSet::EMPTY;
            after.changed_columns(&before, &mut fast);
            for &column in TransitColumn::all() {
                let differs = after.value(column) != before.value(column);
                assert_eq!(fast.contains(column.ix()), differs, "{column:?}");
            }
        }
        assert!(row("B → C", 1, 0., 90).departed_since(&before));
        assert!(!row("A → B", 0, 0.75, 15).departed_since(&before));
    }

    #[test]
    fn test_countdown_reads_as_minutes_and_sorts_by_time() {
        let text = |seconds| {
            let mut out = String::new();
            Countdown::from_seconds(seconds).write(&mut out);
            out
        };
        assert_eq!(text(0), "0:00");
        assert_eq!(text(45), "0:45");
        assert_eq!(text(125), "2:05");
        assert_eq!(text(3725), "1:02:05");
        assert_eq!(Countdown::between(1_000., 61_000.).seconds(), 60);
        assert_eq!(Countdown::between(1_000., 61_500.).seconds(), 61);
        assert_eq!(Countdown::between(5_000., 1_000.).seconds(), 0);
        let (soon, later) = (Countdown::from_seconds(9), Countdown::from_seconds(70));
        assert!(
            CellValue::Custom(&soon)
                .cmp_total(&CellValue::Custom(&later))
                .is_lt()
        );
        assert!(matches!(later.export_value(), Some(CellValue::Int(70))));
    }

    #[test]
    fn test_row_of_a_vehicle() {
        let line = Arc::new(Line {
            mode: Mode::UBahn,
            name: "U3".into(),
            color: None,
            text_color: None,
            rank: 3,
        });
        let vehicle = Vehicle {
            key: VehicleKey::new(1),
            line,
            headsign: "Moosach".into(),
            leg: "Marienplatz → Odeonsplatz".into(),
            progress: 0.4567,
            next_arrival_ms: 10_000. + 61_200.,
            stops_left: 8,
            at: GeoPoint::new(48.14, 11.58),
            path: Arc::from([GeoPoint::new(48.13, 11.57), GeoPoint::new(48.14, 11.58)]),
            leg_ix: 0,
            live: None,
        };
        let row = VehicleRow::new(&vehicle, 10_000.);
        assert_eq!(row.progress, Percent::of(0.46));
        let mut percent = String::new();
        row.progress.write_text(&mut percent);
        assert_eq!(percent, "46%");
        assert_eq!(row.next_in.seconds(), 62);
        let mut text = String::new();
        if let CellValue::Custom(mode) = row.value(TransitColumn::Mode) {
            mode.write_text(&mut text);
        }
        assert_eq!(text, "U-Bahn");
    }
}
