//! The schedule engine: which trips of the timetable are under way at an
//! instant, and where each one is, interpolated between its stations by
//! time (the feed has no shapes). [`TimetableSource`] is the
//! [`VehicleSource`] over it, which also answers for a station's board:
//! the departures from it by the timetable, from an index of the patterns
//! that call there made once at load, so a board costs a few binary
//! searches a sweep, never a pass over all trips. No GPUI types.
//!
//! A trip runs on the service days of its calendar, its times counted from
//! each day's origin (`zone::Day::origin_ms`). One that runs past midnight
//! is still the previous day's: an instant looks at its own day in Munich,
//! the next one (whose origin can come an hour early when the clocks go
//! forward) and as many days back as the latest arrival reaches.

use std::{collections::HashMap, sync::Arc};

use super::{
    source::{
        Departure, GeoBounds, Mode, Network, SourceClock, SourceInfo, Vehicle, VehicleKey,
        VehicleSource,
    },
    timetable::{Pattern, Timetable, Trip},
    zone::{Day, Zone},
};

/// How far ahead a station's board looks.
pub const BOARD_HORIZON_S: f64 = 3_600.;

/// Service days whose trips stay sorted for the next sweeps: today,
/// yesterday, tomorrow, and one more while the clock runs fast.
const PLANS_KEPT: usize = 4;

/// The trips of one service day, by first departure.
struct DayPlan {
    day: Day,
    origin_ms: f64,
    /// Whether each calendar runs on the day.
    runs: Vec<bool>,
    trips: Vec<u32>,
    starts: Vec<u32>,
}

/// A departure by the timetable: a trip's call at a station on its service
/// day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Call {
    pub day: Day,
    pub trip: u32,
    /// The call's index in the trip's pattern.
    pub stop: u16,
    /// When it departs, Unix ms.
    pub depart_ms: f64,
}

impl Call {
    /// The key of its run: the same as its vehicle's.
    pub fn key(&self) -> VehicleKey {
        Run {
            day: self.day,
            trip: self.trip,
            elapsed: 0.,
            started_ms: 0.,
        }
        .key()
    }
}

/// A trip under way: on its service day, seconds after its first
/// departure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Run {
    pub day: Day,
    pub trip: u32,
    pub elapsed: f64,
    /// Unix ms of its first departure.
    pub started_ms: f64,
}

impl Run {
    /// The key of the run in the table: the trip on its service day, the
    /// same all day, another tomorrow.
    pub fn key(&self) -> VehicleKey {
        VehicleKey::new((u64::from(self.day.days() as u32) << 32) | u64::from(self.trip))
    }
}

/// Where a run is at its instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    /// The station it left last or waits at, by its index in the pattern.
    pub leg: usize,
    /// Along the leg, 0 to 1.
    pub progress: f64,
    /// When it reaches the next station, Unix ms.
    pub next_arrival_ms: f64,
    /// Stations ahead, the next one included.
    pub stops_left: usize,
}

/// The timetable with the trips of the days around now sorted.
pub struct Schedule {
    timetable: Timetable,
    longest: u32,
    days_back: i32,
    plans: Vec<DayPlan>,
    /// Per station, every pattern that departs from it: the pattern and
    /// the call's index in it (never the last call, which only arrives).
    calls: Vec<Vec<(u32, u16)>>,
    /// Per pattern, its trips by first departure, and those departures.
    pattern_trips: Vec<Vec<u32>>,
    pattern_starts: Vec<Vec<u32>>,
}

impl Schedule {
    /// The schedule of `timetable`, with the board's index: one pass over
    /// the patterns and one over the trips.
    pub fn new(timetable: Timetable) -> Self {
        let mut calls = vec![Vec::new(); timetable.stations.len()];
        for (pattern_ix, pattern) in timetable.patterns.iter().enumerate() {
            let departing = pattern.stations.len().saturating_sub(1);
            for (stop, station) in pattern.stations[..departing].iter().enumerate() {
                calls[*station as usize].push((pattern_ix as u32, stop as u16));
            }
        }
        let mut pattern_trips = vec![Vec::new(); timetable.patterns.len()];
        for (trip_ix, trip) in timetable.trips.iter().enumerate() {
            pattern_trips[trip.pattern as usize].push(trip_ix as u32);
        }
        for trips in &mut pattern_trips {
            trips.sort_by_key(|ix| timetable.trips[*ix as usize].start);
        }
        let pattern_starts = pattern_trips
            .iter()
            .map(|trips| {
                trips
                    .iter()
                    .map(|ix| timetable.trips[*ix as usize].start)
                    .collect()
            })
            .collect();
        Self {
            longest: timetable.longest_trip(),
            days_back: (timetable.latest_arrival() / 86_400) as i32,
            timetable,
            plans: Vec::new(),
            calls,
            pattern_trips,
            pattern_starts,
        }
    }

    pub fn timetable(&self) -> &Timetable {
        &self.timetable
    }

    /// The index of `day`'s plan, made once and kept while it is near.
    fn plan_ix(&mut self, day: Day) -> usize {
        if let Some(ix) = self.plans.iter().position(|plan| plan.day == day) {
            return ix;
        }
        let runs: Vec<bool> = self
            .timetable
            .calendars
            .iter()
            .map(|calendar| calendar.runs_on(day))
            .collect();
        let trips_of = &self.timetable.trips;
        let mut trips: Vec<u32> = (0..trips_of.len() as u32)
            .filter(|ix| runs[trips_of[*ix as usize].calendar as usize])
            .collect();
        trips.sort_by_key(|ix| trips_of[*ix as usize].start);
        let starts = trips
            .iter()
            .map(|ix| trips_of[*ix as usize].start)
            .collect();
        if self.plans.len() >= PLANS_KEPT {
            self.plans.remove(0);
        }
        self.plans.push(DayPlan {
            day,
            origin_ms: day.origin_ms(),
            runs,
            trips,
            starts,
        });
        self.plans.len() - 1
    }

    /// The trips that run on the service day `day`.
    pub fn trips_on(&mut self, day: Day) -> usize {
        let ix = self.plan_ix(day);
        self.plans[ix].trips.len()
    }

    /// Every trip under way at `unix_ms`, into `out`.
    pub fn runs_at(&mut self, unix_ms: f64, out: &mut Vec<Run>) {
        let today = Day::in_munich(unix_ms);
        let longest = f64::from(self.longest);
        for back in -1..=self.days_back {
            let day = Day::from_days(today.days() - back);
            let ix = self.plan_ix(day);
            let plan = &self.plans[ix];
            let seconds = (unix_ms - plan.origin_ms) / 1e3;
            if seconds < 0. {
                continue;
            }
            let first = plan
                .starts
                .partition_point(|&start| f64::from(start) <= seconds - longest);
            let last = plan
                .starts
                .partition_point(|&start| f64::from(start) <= seconds);
            for &trip in &plan.trips[first..last] {
                let Trip { pattern, start, .. } = self.timetable.trips[trip as usize];
                let elapsed = seconds - f64::from(start);
                if elapsed < f64::from(self.timetable.patterns[pattern as usize].duration()) {
                    out.push(Run {
                        day,
                        trip,
                        elapsed,
                        started_ms: plan.origin_ms + f64::from(start) * 1e3,
                    });
                }
            }
        }
    }

    /// The departures from `station` in the `horizon_s` seconds after
    /// `unix_ms`, soonest first, at most `limit`, into `out`: for each
    /// pattern that calls there, its trips that depart in the window, on
    /// the service days whose trips can (yesterday's past midnight, today's,
    /// tomorrow's before midnight). A trip that calls twice departs once,
    /// at its first call in the window.
    pub fn departures(
        &mut self,
        station: usize,
        unix_ms: f64,
        horizon_s: f64,
        limit: usize,
        out: &mut Vec<Call>,
    ) {
        let Some(calls) = self.calls.get(station) else {
            return;
        };
        if calls.is_empty() {
            return;
        }
        let first = out.len();
        let today = Day::in_munich(unix_ms);
        for back in -1..=self.days_back {
            let day = Day::from_days(today.days() - back);
            let ix = self.plan_ix(day);
            let plan = &self.plans[ix];
            let seconds = (unix_ms - plan.origin_ms) / 1e3;
            for &(pattern, stop) in &self.calls[station] {
                let depart =
                    f64::from(self.timetable.patterns[pattern as usize].depart[stop as usize]);
                let starts = &self.pattern_starts[pattern as usize];
                // Departs after now and within the horizon.
                let from = starts.partition_point(|&start| f64::from(start) + depart <= seconds);
                let to = starts
                    .partition_point(|&start| f64::from(start) + depart <= seconds + horizon_s);
                for &trip in &self.pattern_trips[pattern as usize][from..to] {
                    let Trip {
                        calendar, start, ..
                    } = self.timetable.trips[trip as usize];
                    if plan.runs[calendar as usize] {
                        out.push(Call {
                            day,
                            trip,
                            stop,
                            depart_ms: plan.origin_ms + (f64::from(start) + depart) * 1e3,
                        });
                    }
                }
            }
        }
        let found = &mut out[first..];
        found.sort_by(|a, b| {
            a.depart_ms
                .total_cmp(&b.depart_ms)
                .then(a.key().cmp(&b.key()))
        });
        // A trip that calls here twice (a loop) departs once: its first.
        let mut seen = std::collections::HashSet::new();
        let mut kept = first;
        for ix in first..out.len() {
            if seen.insert(out[ix].key()) {
                out.swap(kept, ix);
                kept += 1;
            }
        }
        out.truncate(kept.min(first + limit));
    }

    /// The pattern of a run.
    #[cfg(test)]
    pub fn pattern(&self, run: &Run) -> &Pattern {
        let trip = &self.timetable.trips[run.trip as usize];
        &self.timetable.patterns[trip.pattern as usize]
    }

    /// Where `run` is.
    #[cfg(test)]
    pub fn position(&self, run: &Run) -> Option<Position> {
        position(self.pattern(run), run)
    }
}

/// Where `run` of `pattern` is.
pub fn position(pattern: &Pattern, run: &Run) -> Option<Position> {
    let leg = pattern.leg_at(run.elapsed.max(0.).floor() as u32)?;
    let depart = f64::from(pattern.depart[leg]);
    let arrive = f64::from(pattern.arrive[leg + 1]);
    let progress = if arrive > depart {
        ((run.elapsed - depart) / (arrive - depart)).clamp(0., 1.)
    } else {
        1.
    };
    Some(Position {
        leg,
        progress,
        next_arrival_ms: run.started_ms + arrive * 1e3,
        stops_left: pattern.stations.len() - 1 - leg,
    })
}

/// The timetable as a [`VehicleSource`]: from the compiled file's bytes,
/// parsed on `load`.
pub struct TimetableSource {
    data: Option<Vec<u8>>,
    schedule: Option<Schedule>,
    /// Per pattern, its legs' labels, made the first time a run shows them.
    legs: Vec<Option<Arc<[Arc<str>]>>>,
    runs: Vec<Run>,
    calls: Vec<Call>,
}

impl TimetableSource {
    /// A source of the compiled timetable in `data`.
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data: Some(data),
            schedule: None,
            legs: Vec::new(),
            runs: Vec::new(),
            calls: Vec::new(),
        }
    }

    /// The label of `pattern`'s leg `leg`: `Marienplatz → Isartor`.
    fn leg_label(&mut self, pattern_ix: usize, leg: usize) -> Arc<str> {
        let Some(schedule) = &self.schedule else {
            return "".into();
        };
        let labels = self.legs[pattern_ix].get_or_insert_with(|| {
            let timetable = schedule.timetable();
            let pattern = &timetable.patterns[pattern_ix];
            pattern
                .stations
                .windows(2)
                .map(|pair| {
                    let name = |ix: u32| timetable.stations[ix as usize].name.as_ref();
                    Arc::from(format!("{} → {}", name(pair[0]), name(pair[1])))
                })
                .collect()
        });
        labels.get(leg).cloned().unwrap_or_else(|| "".into())
    }
}

/// The map's network of `timetable`: every station, and each pair of
/// stations a pattern links, in the first mode that links them. The city
/// is around the stations of Munich's U-Bahn, trams and buses, or of
/// Helsinki's metro and trams.
pub fn network(timetable: &Timetable) -> Network {
    let in_city = |mode: Mode| match timetable.zone {
        Zone::Munich => matches!(mode, Mode::UBahn | Mode::Tram | Mode::Bus),
        Zone::Helsinki => matches!(mode, Mode::Metro | Mode::Tram),
    };
    let mut links: HashMap<(u32, u32), Mode> = HashMap::new();
    let mut city = vec![false; timetable.stations.len()];
    for pattern in &timetable.patterns {
        let mode = pattern.line.mode;
        for pair in pattern.stations.windows(2) {
            let key = (pair[0].min(pair[1]), pair[0].max(pair[1]));
            if key.0 == key.1 {
                continue;
            }
            let link = links.entry(key).or_insert(mode);
            *link = (*link).min(mode);
        }
        if in_city(mode) {
            for station in &pattern.stations {
                city[*station as usize] = true;
            }
        }
    }
    let mut links: Vec<(u32, u32, Mode)> = links
        .into_iter()
        .map(|((a, b), mode)| (a, b, mode))
        .collect();
    // Buses under trams under trains, when the map draws them in order.
    links.sort_unstable_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    let stations: Vec<_> = timetable
        .stations
        .iter()
        .map(|station| station.at)
        .collect();
    Network {
        region: GeoBounds::around(stations.iter().copied()),
        city: GeoBounds::around(
            stations
                .iter()
                .zip(&city)
                .filter(|(_, city)| **city)
                .map(|(at, _)| *at),
        ),
        stations,
        names: timetable
            .stations
            .iter()
            .map(|station| station.name.clone())
            .collect(),
        links,
        lines: timetable.lines.clone(),
    }
}

impl VehicleSource for TimetableSource {
    fn load(&mut self) -> Result<SourceInfo, String> {
        let data = self.data.take().unwrap_or_default();
        let text = std::str::from_utf8(&data).map_err(|_| "the timetable is not UTF-8")?;
        let timetable = Timetable::parse(text)?;
        let feed = &timetable.feed;
        let short = if feed.publisher.contains("(MVV)") {
            "MVV"
        } else {
            &feed.publisher
        };
        let licence = feed.licence.trim_end_matches(" 4.0");
        let info = SourceInfo {
            network: Arc::new(network(&timetable)),
            attribution: format!(
                "Timetable data: {short} ({licence}), feed {}, retrieved {}. Scheduled, not real-time.",
                feed.version, feed.retrieved
            )
            .into(),
            credit: format!(
                "Timetable © {}, {} · feed {} · retrieved {}",
                feed.publisher, feed.licence, feed.version, feed.retrieved
            )
            .into(),
            short: format!("{short} timetable ({licence}), scheduled").into(),
            clock: SourceClock::AnyTime,
            clock_start_ms: None,
            zone: timetable.zone,
            has_board: true,
        };
        self.legs = vec![None; timetable.patterns.len()];
        self.schedule = Some(Schedule::new(timetable));
        Ok(info)
    }

    fn vehicles(&mut self, unix_ms: f64, out: &mut Vec<Vehicle>) {
        let mut runs = std::mem::take(&mut self.runs);
        runs.clear();
        if let Some(schedule) = self.schedule.as_mut() {
            schedule.runs_at(unix_ms, &mut runs);
        }
        for run in &runs {
            let Some(schedule) = self.schedule.as_ref() else {
                break;
            };
            let pattern_ix = schedule.timetable().trips[run.trip as usize].pattern as usize;
            let pattern = &schedule.timetable().patterns[pattern_ix];
            let Some(position) = position(pattern, run) else {
                continue;
            };
            let (line, headsign, path) = (
                pattern.line.clone(),
                pattern.headsign.clone(),
                pattern.path.clone(),
            );
            let at = path[position.leg].toward(path[position.leg + 1], position.progress);
            let leg = self.leg_label(pattern_ix, position.leg);
            out.push(Vehicle {
                key: run.key(),
                line,
                headsign,
                leg,
                progress: position.progress as f32,
                next_arrival_ms: position.next_arrival_ms,
                stops_left: position.stops_left.min(usize::from(u16::MAX)) as u16,
                at,
                path,
                leg_ix: position.leg as u16,
                live: None,
            });
        }
        self.runs = runs;
    }

    fn departures(&mut self, station: u32, unix_ms: f64, limit: usize, out: &mut Vec<Departure>) {
        let Some(schedule) = self.schedule.as_mut() else {
            return;
        };
        let mut calls = std::mem::take(&mut self.calls);
        calls.clear();
        schedule.departures(
            station as usize,
            unix_ms,
            BOARD_HORIZON_S,
            limit,
            &mut calls,
        );
        let timetable = schedule.timetable();
        out.extend(calls.iter().map(|call| {
            let pattern = &timetable.patterns[timetable.trips[call.trip as usize].pattern as usize];
            let stop = usize::from(call.stop);
            let next = pattern.stations[stop + 1] as usize;
            Departure {
                key: call.key(),
                line: pattern.line.clone(),
                headsign: pattern.headsign.clone(),
                platform: pattern
                    .platforms
                    .as_ref()
                    .map_or_else(|| "".into(), |platforms| platforms[stop].clone()),
                depart_ms: call.depart_ms,
                next_stop: timetable.stations[next].name.clone(),
                to_next_s: pattern.arrive[stop + 1].saturating_sub(pattern.depart[stop]),
            }
        }));
        self.calls = calls;
    }

    fn trips_on_day(&mut self, unix_ms: f64) -> Option<u32> {
        let schedule = self.schedule.as_mut()?;
        Some(schedule.trips_on(Day::in_munich(unix_ms)) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        timetable::FIXTURE,
        zone::{self, Day},
    };
    use super::*;

    fn schedule() -> Schedule {
        Schedule::new(Timetable::parse(FIXTURE).expect("the fixture"))
    }

    /// Unix ms of `hh:mm:ss` on the wall clock in Munich on `day`.
    fn at(day: Day, clock: &str) -> f64 {
        zone::parse_munich(&format!("{}T{clock}", day.iso()), day).expect("a time")
    }

    /// The lines of the trips under way.
    fn lines_at(schedule: &mut Schedule, unix_ms: f64) -> Vec<String> {
        let mut runs = Vec::new();
        schedule.runs_at(unix_ms, &mut runs);
        let mut lines: Vec<String> = runs
            .iter()
            .map(|run| schedule.pattern(run).line.name.to_string())
            .collect();
        lines.sort();
        lines
    }

    #[test]
    fn test_calendar_decides_the_trips_of_a_day() {
        let mut schedule = schedule();
        let tuesday = Day::from_ymd(2026, 11, 10);
        assert_eq!(lines_at(&mut schedule, at(tuesday, "06:01:00")), ["U9"]);
        assert_eq!(lines_at(&mut schedule, at(tuesday, "06:11:00")), ["U9"]);
        // Both U9 trips and the tram run on a Tuesday.
        assert_eq!(schedule.trips_on(tuesday), 3);
        let saturday = Day::from_ymd(2026, 11, 14);
        assert!(lines_at(&mut schedule, at(saturday, "06:01:00")).is_empty());
        let removed = Day::from_ymd(2026, 11, 11);
        assert!(lines_at(&mut schedule, at(removed, "06:01:00")).is_empty());
        let added = Day::from_ymd(2026, 11, 15);
        assert_eq!(lines_at(&mut schedule, at(added, "06:01:00")), ["U9"]);
    }

    #[test]
    fn test_a_trip_runs_across_midnight_on_its_own_service_day() {
        let mut schedule = schedule();
        let tuesday = Day::from_ymd(2026, 11, 10);
        let wednesday = tuesday.next();
        assert!(lines_at(&mut schedule, at(tuesday, "23:54:59")).is_empty());
        assert_eq!(lines_at(&mut schedule, at(tuesday, "23:56:00")), ["21"]);
        let after_midnight = at(wednesday, "00:02:00");
        let mut runs = Vec::new();
        schedule.runs_at(after_midnight, &mut runs);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].day, tuesday, "Tuesday's trip, past 24:00");
        assert_eq!(runs[0].elapsed, 420.);
        assert!(lines_at(&mut schedule, at(wednesday, "00:05:00")).is_empty());
    }

    #[test]
    fn test_times_count_from_noon_minus_twelve_hours_when_the_clocks_change() {
        let mut schedule = schedule();
        let autumn = Day::from_ymd(2026, 10, 25);
        let utc = |hours: f64| f64::from(autumn.days()) * 86_400_000. + hours * 3_600_000.;
        // "01:30:00" is 01:30 after the origin, 23:00 UTC the day before:
        // 00:30 UTC, 02:30 summer time.
        assert!(lines_at(&mut schedule, utc(0.49)).is_empty());
        assert_eq!(lines_at(&mut schedule, utc(0.51)), ["N40"]);
        // "08:00:00" is 07:00 UTC, 08:00 on the winter clock.
        assert!(lines_at(&mut schedule, utc(6.01)).is_empty());
        assert_eq!(lines_at(&mut schedule, utc(7.01)), ["N40"]);
        assert_eq!(lines_at(&mut schedule, at(autumn, "08:01:00")), ["N40"]);
    }

    #[test]
    fn test_progress_and_countdown() {
        let mut schedule = schedule();
        let tuesday = Day::from_ymd(2026, 11, 10);
        let now = at(tuesday, "06:01:00");
        let mut runs = Vec::new();
        schedule.runs_at(now, &mut runs);
        let position = schedule.position(&runs[0]).expect("under way");
        assert_eq!(position.leg, 0);
        assert_eq!(position.progress, 0.5);
        assert_eq!(position.next_arrival_ms - now, 60_000.);
        assert_eq!(position.stops_left, 2);
        // Waiting at Beta: no progress, Gamma next.
        let waiting = at(tuesday, "06:02:15");
        runs.clear();
        schedule.runs_at(waiting, &mut runs);
        let position = schedule.position(&runs[0]).expect("under way");
        assert_eq!(position.leg, 1);
        assert_eq!(position.progress, 0.);
        assert_eq!(position.next_arrival_ms - waiting, 135_000.);
        assert_eq!(position.stops_left, 1);
    }

    /// The departures from `station` at `unix_ms`: Munich clock and line.
    fn board(schedule: &mut Schedule, station: usize, unix_ms: f64) -> Vec<(String, String)> {
        let mut calls = Vec::new();
        schedule.departures(station, unix_ms, BOARD_HORIZON_S, 10, &mut calls);
        calls
            .iter()
            .map(|call| {
                let mut clock = String::new();
                zone::write_clock(call.depart_ms, &mut clock);
                let trip = &schedule.timetable().trips[call.trip as usize];
                let line = &schedule.timetable().patterns[trip.pattern as usize].line;
                (clock, line.name.to_string())
            })
            .collect()
    }

    fn pair(clock: &str, line: &str) -> (String, String) {
        (clock.into(), line.into())
    }

    #[test]
    fn test_board_lists_the_next_hour_and_drops_what_departed() {
        let mut schedule = schedule();
        let tuesday = Day::from_ymd(2026, 11, 10);
        let alpha = 0;
        assert_eq!(
            board(&mut schedule, alpha, at(tuesday, "05:55:00")),
            [pair("06:00:00", "U9"), pair("06:10:00", "U9")]
        );
        // The 06:00 has left at 06:00: only the 06:10 is still to come.
        assert_eq!(
            board(&mut schedule, alpha, at(tuesday, "06:00:00")),
            [pair("06:10:00", "U9")]
        );
        // Beta: after the 30 s stop.
        assert_eq!(
            board(&mut schedule, 1, at(tuesday, "06:02:00")),
            [pair("06:02:30", "U9"), pair("06:12:30", "U9")]
        );
        // Gamma is the U9's terminus: it departs from there with the tram
        // only, and only within the hour.
        assert!(board(&mut schedule, 2, at(tuesday, "06:01:00")).is_empty());
        assert_eq!(
            board(&mut schedule, 2, at(tuesday, "23:00:00")),
            [pair("23:55:00", "21")]
        );
        // Not on a Saturday, and not past the horizon.
        let saturday = Day::from_ymd(2026, 11, 14);
        assert!(board(&mut schedule, alpha, at(saturday, "05:55:00")).is_empty());
        assert!(board(&mut schedule, alpha, at(tuesday, "04:59:00")).is_empty());
    }

    #[test]
    fn test_board_reaches_across_midnight_and_the_clock_change() {
        let mut schedule = schedule();
        // Saturday 24 Oct, 23:59 CEST: the night bus of Sunday's service day
        // leaves Delta at 01:30 "after noon minus 12 hours", 02:30 on the
        // summer clock, past the hour; at 01:45 it is within it.
        let saturday = Day::from_ymd(2026, 10, 24);
        assert!(board(&mut schedule, 3, at(saturday, "23:59:00")).is_empty());
        let sunday = Day::from_ymd(2026, 10, 25);
        assert_eq!(
            board(&mut schedule, 3, at(sunday, "01:45:00")),
            [pair("02:30:00", "N40")]
        );
    }

    #[test]
    fn test_source_departures_name_platform_and_next_stop() {
        let mut source = TimetableSource::new(FIXTURE.as_bytes().to_vec());
        let info = source.load().expect("the fixture");
        assert!(info.has_board);
        let tuesday = Day::from_ymd(2026, 11, 10);
        let now = at(tuesday, "06:01:00");
        let mut departures = Vec::new();
        source.departures(1, now, 10, &mut departures);
        assert_eq!(departures.len(), 2);
        let first = &departures[0];
        assert_eq!(first.platform.as_ref(), "2/3");
        assert_eq!(first.next_stop.as_ref(), "Gamma");
        assert_eq!(first.to_next_s, 120);
        assert_eq!(first.depart_ms - now, 90_000.);
        // The key of the departing run is its vehicle's.
        let mut vehicles = Vec::new();
        source.vehicles(now, &mut vehicles);
        assert_eq!(vehicles[0].key, first.key);
        // The tram's stations have no platforms; one at a time.
        departures.clear();
        source.departures(2, at(tuesday, "23:00:00"), 1, &mut departures);
        assert_eq!(departures.len(), 1);
        assert_eq!(departures[0].platform.as_ref(), "");
        assert_eq!(info.network.names[2].as_ref(), "Gamma");
    }

    #[test]
    fn test_source_reports_vehicles_with_their_legs() {
        let mut source = TimetableSource::new(FIXTURE.as_bytes().to_vec());
        let info = source.load().expect("the fixture");
        assert_eq!(
            info.attribution.as_ref(),
            "Timetable data: Test publisher (CC BY), feed 20261102, retrieved 2026-11-07. Scheduled, not real-time."
        );
        assert_eq!(info.network.stations.len(), 5);
        assert_eq!(info.network.links.len(), 4);
        let tuesday = Day::from_ymd(2026, 11, 10);
        let mut vehicles = Vec::new();
        source.vehicles(at(tuesday, "06:01:00"), &mut vehicles);
        assert_eq!(vehicles.len(), 1);
        let vehicle = &vehicles[0];
        assert_eq!(vehicle.leg.as_ref(), "Alpha → Beta");
        assert_eq!(vehicle.headsign.as_ref(), "Gamma");
        assert_eq!(vehicle.stops_left, 2);
        assert!((vehicle.at.lat - 48.135).abs() < 1e-9);
        // The same leg's label is shared, not made again.
        let mut again = Vec::new();
        source.vehicles(at(tuesday, "06:01:30"), &mut again);
        assert!(Arc::ptr_eq(&vehicle.leg, &again[0].leg));
        assert_eq!(source.trips_on_day(at(tuesday, "12:00:00")), Some(3));
    }
}
