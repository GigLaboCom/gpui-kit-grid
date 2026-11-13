//! Between the vehicles and the view: [`VehicleSource`], what the producer
//! asks once a sweep for every vehicle under way, and what it answers with.
//! Two sources implement it: Munich's timetable (`schedule.rs`,
//! [`TimetableSource`]), which also knows the departures from a station,
//! and Helsinki's live vehicle positions (`hsl.rs`, HSL's high-frequency
//! positioning over MQTT). No GPUI types.
//!
//! [`TimetableSource`]: super::schedule::TimetableSource

use std::{cmp::Ordering, sync::Arc, time::Duration};

use gpui_kit_grid::{CellValue, GridValue, OwnedValue};
use std::fmt::Write as _;

use super::{mqtt::socket::MaybeSend, zone::Zone};

/// A place, in degrees.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

impl GeoPoint {
    pub const fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }

    /// The point `share` of the way to `to` (0 here, 1 there).
    pub fn toward(self, to: Self, share: f64) -> Self {
        Self {
            lat: self.lat + (to.lat - self.lat) * share,
            lon: self.lon + (to.lon - self.lon) * share,
        }
    }
}

/// A box of places, south-west to north-east.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoBounds {
    pub min: GeoPoint,
    pub max: GeoPoint,
}

impl GeoBounds {
    /// The box around `points`, `None` for none.
    pub fn around(points: impl IntoIterator<Item = GeoPoint>) -> Option<Self> {
        points.into_iter().fold(None, |bounds, point| {
            Some(match bounds {
                None => Self {
                    min: point,
                    max: point,
                },
                Some(Self { min, max }) => Self {
                    min: GeoPoint::new(min.lat.min(point.lat), min.lon.min(point.lon)),
                    max: GeoPoint::new(max.lat.max(point.lat), max.lon.max(point.lon)),
                },
            })
        })
    }

    pub fn center(&self) -> GeoPoint {
        self.min.toward(self.max, 0.5)
    }
}

/// Whose vehicles: Munich's by the timetable, or Helsinki's live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum City {
    #[default]
    Munich,
    Helsinki,
}

impl City {
    pub const ALL: [Self; 2] = [Self::Munich, Self::Helsinki];

    pub fn label(self) -> &'static str {
        match self {
            Self::Munich => "Munich",
            Self::Helsinki => "Helsinki",
        }
    }

    /// `munich` or `helsinki`, as `?city=` and `TRANSIT_CITY` name it.
    pub fn id(self) -> &'static str {
        match self {
            Self::Munich => "munich",
            Self::Helsinki => "helsinki",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|city| city.id().eq_ignore_ascii_case(id.trim()))
    }

    pub fn ix(self) -> usize {
        self as usize
    }
}

/// How a line travels: what the table groups by first, in this order.
/// Munich has the U-Bahn, the S-Bahn, regional trains, trams, buses and
/// regional buses; Helsinki the metro, commuter trains, trams, buses and
/// ferries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mode {
    UBahn,
    Metro,
    SBahn,
    /// A commuter train: Helsinki's.
    Train,
    /// A regional train (RB, RE): Munich's, an option.
    Regional,
    Tram,
    Bus,
    /// A regional bus: Munich's, an option.
    RegionalBus,
    Ferry,
}

impl Mode {
    pub const COUNT: usize = 9;
    pub const ALL: [Self; Self::COUNT] = [
        Self::UBahn,
        Self::Metro,
        Self::SBahn,
        Self::Train,
        Self::Regional,
        Self::Tram,
        Self::Bus,
        Self::RegionalBus,
        Self::Ferry,
    ];

    /// The letter the compiled timetable writes: `U`, `M`, `S`, `C`,
    /// `R`, `T`, `B`, `X` or `F`.
    pub fn from_letter(letter: &str) -> Option<Self> {
        match letter {
            "U" => Some(Self::UBahn),
            "M" => Some(Self::Metro),
            "S" => Some(Self::SBahn),
            "C" => Some(Self::Train),
            "R" => Some(Self::Regional),
            "T" => Some(Self::Tram),
            "B" => Some(Self::Bus),
            "X" => Some(Self::RegionalBus),
            "F" => Some(Self::Ferry),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::UBahn => "U-Bahn",
            Self::Metro => "Metro",
            Self::SBahn => "S-Bahn",
            Self::Train => "Train",
            Self::Regional => "Regional train",
            Self::Tram => "Tram",
            Self::Bus => "Bus",
            Self::RegionalBus => "Regional bus",
            Self::Ferry => "Ferry",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    /// Does it run on rails?
    pub fn is_rail(self) -> bool {
        !matches!(self, Self::Bus | Self::RegionalBus | Self::Ferry)
    }
}

/// A mode is a cell value of its own: it sorts U-Bahn, S-Bahn, tram, bus,
/// and reads as its name.
impl GridValue for Mode {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        match other.downcast_ref::<Mode>() {
            Some(other) => self.cmp(other),
            None => std::any::TypeId::of::<Self>().cmp(&other.as_any().type_id()),
        }
    }

    fn write_text(&self, out: &mut String) {
        out.push_str(self.label());
    }
}

/// A line: its mode, its name and, if the data gives one, its colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub mode: Mode,
    pub name: Arc<str>,
    /// `0xRRGGBB`, the data's own colour (GTFS `route_color`); `None` where
    /// the data has none, and the theme's colour of the mode stands in.
    pub color: Option<u32>,
    pub text_color: Option<u32>,
    /// Its place among the source's lines: by mode, then as people sort
    /// line names (S2 before S20).
    pub rank: u32,
}

/// A line is a cell value of its own: it sorts by rank, so 19 comes
/// before 100, and reads as its name.
impl GridValue for Line {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        match other.downcast_ref::<Line>() {
            Some(other) => self.rank.cmp(&other.rank),
            None => std::any::TypeId::of::<Self>().cmp(&other.as_any().type_id()),
        }
    }

    fn write_text(&self, out: &mut String) {
        out.push_str(&self.name);
    }

    /// Kept as itself: groups of lines keep their order, and a caption
    /// finds the line's colours.
    fn to_owned_value(&self) -> OwnedValue {
        OwnedValue::custom(self.clone())
    }
}

/// A vehicle under way, as long as its run lasts: the timetable's trip on
/// its service day, or a live feed's vehicle by its operator and number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VehicleKey(u64);

impl VehicleKey {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One vehicle under way, as a source reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Vehicle {
    pub key: VehicleKey,
    pub line: Arc<Line>,
    pub headsign: Arc<str>,
    /// The leg: `Marienplatz → Isartor`.
    pub leg: Arc<str>,
    /// How far along the leg, 0 to 1: 0 while it waits at a station.
    pub progress: f32,
    /// When it reaches the next station, Unix ms.
    pub next_arrival_ms: f64,
    /// Stations still ahead, the next one included.
    pub stops_left: u16,
    /// Where it is now.
    pub at: GeoPoint,
    /// The stations of its run, for the map: shared by every vehicle on it.
    pub path: Arc<[GeoPoint]>,
    /// The index in `path` of the station it left last (or waits at).
    pub leg_ix: u16,
    /// What a live feed reports beside the place; `None` from a timetable.
    pub live: Option<LiveState>,
}

/// What a live vehicle reports of itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiveState {
    /// Seconds behind the timetable; negative ahead of it.
    pub delay_s: Option<i32>,
    pub speed_kmh: Option<f32>,
    /// How full, where the feed says.
    pub occupancy: Option<Occupancy>,
}

/// How full a vehicle is, 0 to 100, shown `full` at 100 and `40%`
/// otherwise; HSL's buses and trams say 0 (seats free, which the table
/// leaves blank) or 100 (full).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Occupancy(pub u8);

impl GridValue for Occupancy {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        match other.downcast_ref::<Occupancy>() {
            Some(other) => self.cmp(other),
            None => std::any::TypeId::of::<Self>().cmp(&other.as_any().type_id()),
        }
    }

    fn write_text(&self, out: &mut String) {
        match self.0 {
            100.. => out.push_str("full"),
            0 => out.push_str("seats"),
            share => {
                let _ = write!(out, "{share}%");
            }
        }
    }

    fn export_value(&self) -> Option<CellValue<'_>> {
        Some(CellValue::Int(i64::from(self.0)))
    }
}

/// A departure from a station, by the timetable.
#[derive(Clone, Debug, PartialEq)]
pub struct Departure {
    /// The run that departs: the same key as its vehicle's.
    pub key: VehicleKey,
    pub line: Arc<Line>,
    pub headsign: Arc<str>,
    /// Where the feed names one: `2`, `1/2`.
    pub platform: Arc<str>,
    /// When it departs, Unix ms.
    pub depart_ms: f64,
    pub next_stop: Arc<str>,
    /// From the departure to the arrival at the next stop, seconds.
    pub to_next_s: u32,
}

/// What the map draws under the vehicles: the network's stations and the
/// links between them, each in the mode of a line that uses it.
#[derive(Clone, Debug, Default)]
pub struct Network {
    pub stations: Vec<GeoPoint>,
    /// The stations' names, by the same index.
    pub names: Vec<Arc<str>>,
    /// Two stations linked by a line of the mode, each link once.
    pub links: Vec<(u32, u32, Mode)>,
    pub lines: Vec<Arc<Line>>,
    /// Around every station.
    pub region: Option<GeoBounds>,
    /// Around the stations of the city's own lines (Munich: U-Bahn, tram
    /// and bus; Helsinki: metro and tram).
    pub city: Option<GeoBounds>,
}

/// How a source's time works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceClock {
    /// It computes any instant: the clock may start anywhere and run fast
    /// (a timetable).
    AnyTime,
    /// It reports the latest positions only: the clock is the real one (a
    /// live feed).
    LiveOnly,
}

/// What a source says about itself once it is loaded.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub network: Arc<Network>,
    /// The line the status bar ends with: whose data, its licence and
    /// version, and what kind of data it is.
    pub attribution: Arc<str>,
    /// The same, short, for a narrow window: the map's footer carries the
    /// full credit.
    pub short: Arc<str>,
    /// The full credit the data's licence asks for, for the map's footer
    /// and the page.
    pub credit: Arc<str>,
    pub clock: SourceClock,
    /// Where a live-only clock starts: a recording's start; the real time
    /// if `None`.
    pub clock_start_ms: Option<f64>,
    /// Where the clock is shown.
    pub zone: Zone,
    /// Does it know the departures from a station (`departures`)?
    pub has_board: bool,
}

/// How a live feed is doing, for the status bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeedStatus {
    /// `live`, `connecting`, `reconnecting in 16 s (closed)`, `replay`.
    pub connection: Arc<str>,
    pub is_live: bool,
    /// Messages a second, recently.
    pub rate: f32,
    pub messages: u64,
    /// Why the source is not what was asked for: a recording replays.
    pub notice: Option<Arc<str>>,
}

/// Where the vehicles under way come from. The producer owns one and calls
/// it on its own thread natively, between frames in the browser: `load`
/// once, then `poll` every step and `vehicles` once a sweep.
pub trait VehicleSource: MaybeSend + 'static {
    /// Prepares the source: parses a timetable, connects to a feed. `Err`
    /// says why it has no vehicles.
    fn load(&mut self) -> Result<SourceInfo, String>;

    /// Every vehicle under way at `unix_ms`, into `out` (empty on entry).
    /// A timetable computes any instant; a live source reports what it
    /// last heard and may ignore the time.
    fn vehicles(&mut self, unix_ms: f64, out: &mut Vec<Vehicle>);

    /// The trips of the service day of `unix_ms`, if the source knows them.
    fn trips_on_day(&mut self, unix_ms: f64) -> Option<u32>;

    /// Takes what a live feed delivered, at the producer's `now` (a
    /// monotonic time since it started). A timetable has nothing to take.
    fn poll(&mut self, now: Duration) {
        let _ = now;
    }

    /// The departures from `station` (an index into the network's
    /// stations) after `unix_ms`, soonest first, into `out` (empty on
    /// entry): the next hour's, at most `limit`. A source without a
    /// timetable has none.
    fn departures(&mut self, station: u32, unix_ms: f64, limit: usize, out: &mut Vec<Departure>) {
        let _ = (station, unix_ms, limit, out);
    }

    /// How a live feed is doing; `None` for a timetable.
    fn status(&self) -> Option<FeedStatus> {
        None
    }

    /// Closes what the source holds open, politely.
    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use gpui_kit_grid::CellValue;

    use super::*;

    #[test]
    fn test_modes_and_lines_sort_by_rank() {
        let (u, bus) = (Mode::UBahn, Mode::Bus);
        assert!(
            CellValue::Custom(&u)
                .cmp_total(&CellValue::Custom(&bus))
                .is_lt()
        );
        let line = |name: &str, rank| Line {
            mode: Mode::Bus,
            name: name.into(),
            color: None,
            text_color: None,
            rank,
        };
        let (short, long) = (line("19", 3), line("100", 9));
        assert!(
            CellValue::Custom(&short)
                .cmp_total(&CellValue::Custom(&long))
                .is_lt()
        );
        // A group key keeps the line itself, ordered by rank.
        assert!(matches!(long.to_owned_value(), OwnedValue::Custom(value)
            if value.downcast_ref::<Line>() == Some(&long)));
        assert!(
            short
                .to_owned_value()
                .cmp_total(&long.to_owned_value())
                .is_lt()
        );
        assert_eq!(Mode::from_letter("T"), Some(Mode::Tram));
        assert_eq!(Mode::from_letter("Q"), None);
        assert_eq!(Mode::from_letter("X"), Some(Mode::RegionalBus));
    }

    #[test]
    fn test_bounds_and_points() {
        let a = GeoPoint::new(48.1, 11.5);
        let b = GeoPoint::new(48.2, 11.7);
        let middle = a.toward(b, 0.5);
        assert!((middle.lat - 48.15).abs() < 1e-9 && (middle.lon - 11.6).abs() < 1e-9);
        let bounds = GeoBounds::around([a, b, GeoPoint::new(48.0, 11.6)]).expect("points");
        assert_eq!(bounds.min, GeoPoint::new(48.0, 11.5));
        assert_eq!(bounds.max, GeoPoint::new(48.2, 11.7));
        assert!(GeoBounds::around([]).is_none());
    }
}
