//! The timetable as `script/fetch-transit-data` compiles it from a GTFS
//! feed (`transit timetable 2`, described in the script; version 1 reads
//! too): lines, stations, service calendars, stop patterns with their
//! platforms, and trips. Helsinki's network is the same format without
//! trips: its lines and stops carry the ids the live feed names them by.
//! No GPUI types.
//!
//! The data is © Münchner Verkehrs- und Tarifverbund GmbH (MVV), CC BY 4.0:
//! [`Feed`] carries the attribution the example shows (publisher, licence,
//! feed version and the day it was retrieved). The compiled file is a
//! transformation of the feed, which CC BY allows sharing with that
//! attribution.

use std::sync::Arc;

use super::{
    source::{GeoPoint, Line, Mode},
    zone::{Day, Zone},
};

/// Where the timetable comes from: what its attribution names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    pub publisher: Arc<str>,
    pub licence: Arc<str>,
    pub url: Arc<str>,
    pub version: Arc<str>,
    /// When the feed was downloaded, `2026-11-07`.
    pub retrieved: Arc<str>,
    pub first_day: Option<Day>,
    pub last_day: Option<Day>,
}

/// A station: where vehicles stop, by its name and place.
#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    pub name: Arc<str>,
    pub at: GeoPoint,
}

/// The days a service runs: weekdays between two days, plus and minus
/// single days (GTFS `calendar.txt` and `calendar_dates.txt`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calendar {
    /// Bit 0 Monday through bit 6 Sunday.
    pub weekdays: u8,
    pub first: Option<Day>,
    pub last: Option<Day>,
    /// Sorted.
    pub added: Vec<Day>,
    /// Sorted.
    pub removed: Vec<Day>,
}

impl Calendar {
    /// Does the service run on `day`? An exception wins over the weekdays.
    pub fn runs_on(&self, day: Day) -> bool {
        if self.removed.binary_search(&day).is_ok() {
            return false;
        }
        if self.added.binary_search(&day).is_ok() {
            return true;
        }
        let within = match (self.first, self.last) {
            (Some(first), Some(last)) => (first..=last).contains(&day),
            _ => false,
        };
        within && self.weekdays & (1 << day.weekday()) != 0
    }
}

/// The stations a trip calls at and its times there, shared by every trip
/// that has them.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub line: Arc<Line>,
    pub headsign: Arc<str>,
    pub stations: Vec<u32>,
    /// Seconds after the first departure: the arrival at each station (0
    /// at the first) and the departure from it.
    pub arrive: Vec<u32>,
    pub depart: Vec<u32>,
    /// The stations' places, for the map.
    pub path: Arc<[GeoPoint]>,
    /// The platform of each call (`2`, `1/2`, or empty), if the feed names
    /// any.
    pub platforms: Option<Arc<[Arc<str>]>>,
}

impl Pattern {
    /// From the first departure to the arrival at the last station.
    pub fn duration(&self) -> u32 {
        self.arrive.last().copied().unwrap_or(0)
    }

    /// The leg under way `elapsed` seconds after the first departure: the
    /// index of the station the vehicle is at or has left last, `None`
    /// before the first departure or once it has arrived at the last.
    pub fn leg_at(&self, elapsed: u32) -> Option<usize> {
        if elapsed >= self.duration() {
            return None;
        }
        // The last station reached: arrived at or before `elapsed`.
        let reached = self.arrive.partition_point(|&arrive| arrive <= elapsed);
        Some(
            reached
                .saturating_sub(1)
                .min(self.stations.len().saturating_sub(2)),
        )
    }
}

/// One run of a pattern on the days of a calendar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trip {
    pub pattern: u32,
    pub calendar: u32,
    /// Seconds after the service day's origin: past 86,400 after midnight.
    pub start: u32,
}

/// Everything the example knows of the network's timetable.
#[derive(Clone, Debug, Default)]
pub struct Timetable {
    pub feed: Feed,
    pub zone: Zone,
    pub lines: Vec<Arc<Line>>,
    /// Each line's id in a live feed (GTFS `route_id`), empty if none.
    pub line_ids: Vec<Arc<str>>,
    pub stations: Vec<Station>,
    /// Each station's id in a live feed (GTFS `stop_id`), empty if none.
    pub station_ids: Vec<Arc<str>>,
    pub calendars: Vec<Calendar>,
    pub patterns: Vec<Pattern>,
    pub trips: Vec<Trip>,
}

impl Timetable {
    /// Parses the compiled timetable; `Err` names the first line it cannot
    /// read.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.lines().enumerate();
        match lines.next() {
            Some((_, header))
                if matches!(
                    header.trim_end(),
                    "transit-timetable\t1" | "transit-timetable\t2"
                ) => {}
            _ => return Err("not a transit timetable (version 1 or 2)".into()),
        }
        let mut timetable = Self::default();
        for (ix, line) in lines {
            if line.is_empty() {
                continue;
            }
            timetable
                .read_line(line)
                .map_err(|error| format!("line {}: {error}", ix + 1))?;
        }
        if timetable.patterns.is_empty() {
            return Err("no stop patterns".into());
        }
        Ok(timetable)
    }

    fn read_line(&mut self, line: &str) -> Result<(), String> {
        let fields: Vec<&str> = line.split('\t').collect();
        let field = |ix: usize| fields.get(ix).copied().unwrap_or_default();
        match field(0) {
            "source" => {
                self.feed = Feed {
                    publisher: field(1).into(),
                    licence: field(2).into(),
                    url: field(3).into(),
                    version: field(4).into(),
                    retrieved: field(5).into(),
                    first_day: Day::parse_compact(field(6)),
                    last_day: Day::parse_compact(field(7)),
                };
            }
            "zone" => {
                self.zone = Zone::from_name(field(1)).ok_or_else(|| {
                    format!(
                        "zone {}: Europe/Berlin and Europe/Helsinki are known",
                        field(1)
                    )
                })?;
            }
            "L" => {
                let mode = Mode::from_letter(field(1)).ok_or("a line's mode")?;
                self.lines.push(Arc::new(Line {
                    mode,
                    name: field(2).into(),
                    color: parse_color(field(3)),
                    text_color: parse_color(field(4)),
                    rank: self.lines.len() as u32,
                }));
                self.line_ids.push(field(5).into());
            }
            "S" => {
                let coordinate = |ix: usize| {
                    field(ix)
                        .parse::<i64>()
                        .map(|value| value as f64 / 1e5)
                        .map_err(|_| "a station's place")
                };
                self.stations.push(Station {
                    name: field(1).into(),
                    at: GeoPoint::new(coordinate(2)?, coordinate(3)?),
                });
                self.station_ids.push(field(4).into());
            }
            "C" => {
                let days = field(1).as_bytes();
                if days.len() != 7 {
                    return Err("a calendar's weekdays".into());
                }
                let weekdays = days
                    .iter()
                    .enumerate()
                    .fold(0u8, |mask, (ix, day)| mask | (u8::from(*day == b'1') << ix));
                self.calendars.push(Calendar {
                    weekdays,
                    first: Day::parse_compact(field(2)),
                    last: Day::parse_compact(field(3)),
                    added: parse_days(field(4))?,
                    removed: parse_days(field(5))?,
                });
            }
            "P" => {
                let line = field(1)
                    .parse::<usize>()
                    .ok()
                    .and_then(|ix| self.lines.get(ix))
                    .ok_or("a pattern's line")?
                    .clone();
                let stations = field(3)
                    .split(',')
                    .map(|station| match station.parse::<u32>() {
                        Ok(ix) if (ix as usize) < self.stations.len() => Ok(ix),
                        _ => Err("a pattern's station"),
                    })
                    .collect::<Result<Vec<u32>, _>>()?;
                let (arrive, depart) = parse_legs(field(4), stations.len())?;
                let path = stations
                    .iter()
                    .map(|ix| self.stations[*ix as usize].at)
                    .collect();
                let platforms = match field(5) {
                    "" => None,
                    text => {
                        let platforms: Arc<[Arc<str>]> = text.split(',').map(Arc::from).collect();
                        if platforms.len() != stations.len() {
                            return Err("a pattern's platforms".into());
                        }
                        Some(platforms)
                    }
                };
                self.patterns.push(Pattern {
                    line,
                    headsign: field(2).into(),
                    stations,
                    arrive,
                    depart,
                    path,
                    platforms,
                });
            }
            "T" => {
                let pattern: u32 = field(1).parse().map_err(|_| "a trip's pattern")?;
                let calendar: u32 = field(2).parse().map_err(|_| "a trip's calendar")?;
                if pattern as usize >= self.patterns.len()
                    || calendar as usize >= self.calendars.len()
                {
                    return Err("a trip's pattern or calendar".into());
                }
                let mut start = 0u32;
                for (ix, value) in field(3).split(',').enumerate() {
                    let value: u32 = value.parse().map_err(|_| "a trip's departure")?;
                    start = if ix == 0 { value } else { start + value };
                    self.trips.push(Trip {
                        pattern,
                        calendar,
                        start,
                    });
                }
            }
            other => return Err(format!("unknown record {other:?}")),
        }
        Ok(())
    }

    /// The longest trip, in seconds.
    pub fn longest_trip(&self) -> u32 {
        self.patterns
            .iter()
            .map(Pattern::duration)
            .max()
            .unwrap_or(0)
    }

    /// The latest arrival after a service day's origin, in seconds: past
    /// 86,400 when trips run after midnight.
    pub fn latest_arrival(&self) -> u32 {
        self.trips
            .iter()
            .map(|trip| trip.start + self.patterns[trip.pattern as usize].duration())
            .max()
            .unwrap_or(0)
    }
}

/// `408335` as a colour; none for an empty field.
fn parse_color(text: &str) -> Option<u32> {
    (text.len() == 6)
        .then(|| u32::from_str_radix(text, 16).ok())
        .flatten()
}

fn parse_days(text: &str) -> Result<Vec<Day>, String> {
    let mut days = text
        .split(',')
        .filter(|day| !day.is_empty())
        .map(|day| Day::parse_compact(day).ok_or_else(|| format!("a day {day:?}")))
        .collect::<Result<Vec<Day>, String>>()?;
    days.sort_unstable();
    Ok(days)
}

/// A pattern's legs, `120,60+30`: the arrival at and departure from each of
/// `stations` stations, from the first departure on.
fn parse_legs(text: &str, stations: usize) -> Result<(Vec<u32>, Vec<u32>), String> {
    let mut arrive = Vec::with_capacity(stations);
    let mut depart = Vec::with_capacity(stations);
    arrive.push(0);
    depart.push(0);
    for leg in text.split(',').filter(|leg| !leg.is_empty()) {
        let (travel, dwell) = leg.split_once('+').unwrap_or((leg, "0"));
        let travel: u32 = travel.parse().map_err(|_| "a leg's travel time")?;
        let dwell: u32 = dwell.parse().map_err(|_| "a leg's dwell time")?;
        let reached = depart.last().copied().unwrap_or(0) + travel;
        arrive.push(reached);
        depart.push(reached + dwell);
    }
    if arrive.len() != stations || stations < 2 {
        return Err(format!(
            "{} legs for {stations} stations",
            arrive.len().saturating_sub(1)
        ));
    }
    Ok((arrive, depart))
}

/// A timetable for the tests, in the compiled format: one U-Bahn, one
/// tram and one night bus over five stations; the U-Bahn's platforms are
/// known (1 at Alpha, 2/3 at Beta).
///
/// - `U9` (weekdays, 2026-11-01 to 2027-03-31): A → B → C, 06:00 and
///   06:10, two minutes a leg with a 30 s stop at B; not on 2026-11-11,
///   and on Sunday 2026-11-15 too.
/// - `21` (every day): C → D at 23:55, across midnight, ten minutes.
/// - `N40` (2026-10-25, the day the clocks go back, alone): D → E at
///   01:30 and 08:00.
#[cfg(test)]
pub const FIXTURE: &str = "transit-timetable\t2
source\tTest publisher\tCC BY 4.0\thttps://example.org/gtfs.zip\t20261102\t2026-11-07\t20261101\t20270331
zone\tEurope/Berlin
L\tU\tU9\t\t
L\tT\t21\t\t
L\tB\tN40\t2266AA\tFFFFFF
S\tAlpha\t4813000\t1157000
S\tBeta\t4814000\t1158000
S\tGamma\t4815000\t1159000
S\tDelta\t4816000\t1160000
S\tEpsilon\t4817000\t1161000
C\t1111100\t20261101\t20270331\t20261115\t20261111
C\t1111111\t20261101\t20270331\t\t
C\t0000000\t\t\t20261025\t
P\t0\tGamma\t0,1,2\t120+30,120\t1,2/3,
P\t1\tDelta\t2,3\t600
P\t2\tEpsilon\t3,4\t300
T\t0\t0\t21600,600
T\t1\t1\t86100
T\t2\t2\t5400,23400
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixture_parses() {
        let timetable = Timetable::parse(FIXTURE).expect("the fixture");
        assert_eq!(timetable.feed.version.as_ref(), "20261102");
        assert_eq!(timetable.feed.retrieved.as_ref(), "2026-11-07");
        assert_eq!(timetable.lines.len(), 3);
        assert_eq!(timetable.lines[2].color, Some(0x2266AA));
        assert_eq!(timetable.lines[0].color, None);
        assert_eq!(timetable.stations.len(), 5);
        assert_eq!(timetable.trips.len(), 5);
        assert_eq!(timetable.trips[1].start, 22_200);
        let pattern = &timetable.patterns[0];
        assert_eq!(pattern.arrive, [0, 120, 270]);
        assert_eq!(pattern.depart, [0, 150, 270]);
        assert_eq!(pattern.duration(), 270);
        assert_eq!(timetable.longest_trip(), 600);
        let platforms = pattern.platforms.as_deref().expect("platforms");
        let platforms: Vec<&str> = platforms.iter().map(AsRef::as_ref).collect();
        assert_eq!(platforms, ["1", "2/3", ""]);
        assert!(timetable.patterns[1].platforms.is_none());
        assert_eq!(timetable.zone, Zone::Munich);
        assert_eq!(timetable.station_ids.len(), 5);
        assert_eq!(timetable.latest_arrival(), 86_700);
    }

    #[test]
    fn test_parse_names_the_bad_line() {
        let error = Timetable::parse("transit-timetable\t1\nL\tQ\tU1\t\t\n").unwrap_err();
        assert!(error.starts_with("line 2:"), "{error}");
        assert!(Timetable::parse("hello").is_err());
        // Version 1, no ids or platforms, and Helsinki's zone.
        let helsinki = Timetable::parse(
            "transit-timetable\t1\nzone\tEurope/Helsinki\nS\tA\t1\t2\nS\tB\t3\t4\nL\tT\t4\t\t\nP\t0\tB\t0,1\t60\n",
        )
        .expect("version 1");
        assert_eq!(helsinki.zone, Zone::Helsinki);
        assert_eq!(helsinki.line_ids[0].as_ref(), "");
        let error = Timetable::parse(
            "transit-timetable\t1\nS\tA\t1\t2\nS\tB\t3\t4\nL\tU\tU1\t\t\nP\t0\tB\t0,1\t60,60\n",
        )
        .unwrap_err();
        assert!(error.contains("2 legs for 2 stations"), "{error}");
    }

    #[test]
    fn test_calendar_rules() {
        let timetable = Timetable::parse(FIXTURE).expect("the fixture");
        let weekdays = &timetable.calendars[0];
        assert!(weekdays.runs_on(Day::from_ymd(2026, 11, 10)), "a Tuesday");
        assert!(!weekdays.runs_on(Day::from_ymd(2026, 11, 14)), "a Saturday");
        assert!(!weekdays.runs_on(Day::from_ymd(2026, 11, 11)), "removed");
        assert!(
            weekdays.runs_on(Day::from_ymd(2026, 11, 15)),
            "a Sunday, added"
        );
        assert!(
            !weekdays.runs_on(Day::from_ymd(2026, 10, 30)),
            "before the first day"
        );
        assert!(
            !weekdays.runs_on(Day::from_ymd(2027, 4, 1)),
            "after the last day"
        );
        let single = &timetable.calendars[2];
        assert!(single.runs_on(Day::from_ymd(2026, 10, 25)));
        assert!(!single.runs_on(Day::from_ymd(2026, 10, 26)));
    }

    #[test]
    fn test_leg_at() {
        let timetable = Timetable::parse(FIXTURE).expect("the fixture");
        let pattern = &timetable.patterns[0];
        assert_eq!(pattern.leg_at(0), Some(0));
        assert_eq!(pattern.leg_at(119), Some(0));
        // At B, waiting, then on to C: the leg from B.
        assert_eq!(pattern.leg_at(120), Some(1));
        assert_eq!(pattern.leg_at(149), Some(1));
        assert_eq!(pattern.leg_at(269), Some(1));
        assert_eq!(pattern.leg_at(270), None);
    }
}
