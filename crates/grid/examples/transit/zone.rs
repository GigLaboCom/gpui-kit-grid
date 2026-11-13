//! Days and the timetable's zone, Europe/Berlin, without a time zone
//! database: the EU rule (summer time from 01:00 UTC on the last Sunday of
//! March to 01:00 UTC on the last Sunday of October) is the only one
//! Munich has had since 1996, and the example needs no other. Helsinki's
//! clock ([`Zone::Helsinki`]) is an hour ahead by the same rule. No GPUI
//! types.
//!
//! A GTFS service day starts at "noon minus 12 hours" in the zone
//! ([`Day::origin_ms`]): local midnight on most days, 23:00 or 01:00 of the
//! wall clock on the two days a year the clocks change, so a trip's times
//! stay plain seconds from that origin, past 24:00 after midnight.

use std::fmt::Write as _;

const DAY_MS: f64 = 86_400_000.;
const HOUR_MS: f64 = 3_600_000.;

/// A calendar day, as days since 1970-01-01.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(i32);

impl Day {
    pub const fn from_days(days: i32) -> Self {
        Self(days)
    }

    /// The day of `year`, `month` (1–12) and `day` (1–31), in the
    /// proleptic Gregorian calendar.
    pub fn from_ymd(year: i32, month: u32, day: u32) -> Self {
        // Howard Hinnant's `days_from_civil`.
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let month = month as i32;
        let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        Self(era * 146_097 + day_of_era - 719_468)
    }

    /// `20261110` as GTFS writes days.
    pub fn parse_compact(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() != 8 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let year = text[..4].parse().ok()?;
        let month = text[4..6].parse().ok()?;
        let day = text[6..].parse().ok()?;
        valid(year, month, day).then(|| Self::from_ymd(year, month, day))
    }

    /// `2026-11-10`.
    pub fn parse_iso(text: &str) -> Option<Self> {
        let mut parts = text.trim().splitn(3, '-');
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        valid(year, month, day).then(|| Self::from_ymd(year, month, day))
    }

    pub const fn days(self) -> i32 {
        self.0
    }

    /// The year, month and day.
    pub fn ymd(self) -> (i32, u32, u32) {
        let days = self.0 + 719_468;
        let era = days.div_euclid(146_097);
        let day_of_era = days - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_index = (5 * day_of_year + 2) / 153;
        let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
        let month = if month_index < 10 {
            month_index + 3
        } else {
            month_index - 9
        } as u32;
        let year = year_of_era + era * 400 + i32::from(month <= 2);
        (year, month, day)
    }

    /// 0 for Monday through 6 for Sunday, as GTFS lists the days.
    pub fn weekday(self) -> u8 {
        // 1970-01-01 was a Thursday.
        (self.0 + 3).rem_euclid(7) as u8
    }

    #[cfg(test)]
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }

    pub fn previous(self) -> Self {
        Self(self.0 - 1)
    }

    /// Midnight UTC of the day, Unix ms.
    fn utc_ms(self) -> f64 {
        f64::from(self.0) * DAY_MS
    }

    /// The day's GTFS origin, noon minus 12 hours in Munich, Unix ms: the
    /// instant a trip's times count from.
    pub fn origin_ms(self) -> f64 {
        // Noon is never near a change of the clocks (01:00 UTC), so the
        // offset at 11:00 UTC is the offset at noon in Munich.
        self.utc_ms() - offset_ms(self.utc_ms() + 11. * HOUR_MS)
    }

    /// The day in Munich at `unix_ms`.
    pub fn in_munich(unix_ms: f64) -> Self {
        Self((munich_ms(unix_ms) / DAY_MS).floor() as i32)
    }

    /// `Tue 10 Nov`.
    pub fn write_short(self, out: &mut String) {
        const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let (_, month, day) = self.ymd();
        let _ = write!(
            out,
            "{} {day} {}",
            WEEKDAYS[usize::from(self.weekday())],
            MONTHS[month as usize - 1]
        );
    }

    /// `2026-11-10`.
    #[cfg(test)]
    pub fn iso(self) -> String {
        let (year, month, day) = self.ymd();
        format!("{year:04}-{month:02}-{day:02}")
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn valid(year: i32, month: u32, day: u32) -> bool {
    (1..=12).contains(&month) && (1..=31).contains(&day) && (1900..=2200).contains(&year)
}

/// The last Sunday of `month` in `year`.
fn last_sunday(year: i32, month: u32) -> Day {
    let next = if month == 12 {
        Day::from_ymd(year + 1, 1, 1)
    } else {
        Day::from_ymd(year, month + 1, 1)
    };
    let last = next.previous();
    Day(last.0 - i32::from(last.weekday() + 1) % 7)
}

/// Munich's offset from UTC at `unix_ms`, in ms: one hour, two in summer.
pub fn offset_ms(unix_ms: f64) -> f64 {
    let (year, _, _) = Day((unix_ms / DAY_MS).floor() as i32).ymd();
    let start = last_sunday(year, 3).utc_ms() + HOUR_MS;
    let end = last_sunday(year, 10).utc_ms() + HOUR_MS;
    if (start..end).contains(&unix_ms) {
        2. * HOUR_MS
    } else {
        HOUR_MS
    }
}

/// Is `unix_ms` summer time in Munich?
pub fn is_summer(unix_ms: f64) -> bool {
    offset_ms(unix_ms) > HOUR_MS
}

/// The wall clock in Munich at `unix_ms`, as Unix ms of that clock.
pub fn munich_ms(unix_ms: f64) -> f64 {
    unix_ms + offset_ms(unix_ms)
}

/// The instant the wall clock in Munich shows `seconds` after midnight of
/// `day`: the earlier of two when the clocks go back, an hour later than
/// written in the hour they skip in spring.
pub fn from_munich(day: Day, seconds: f64) -> f64 {
    let wall = day.utc_ms() + seconds * 1e3;
    for offset in [2. * HOUR_MS, HOUR_MS] {
        if offset_ms(wall - offset) == offset {
            return wall - offset;
        }
    }
    wall - HOUR_MS
}

/// `07:45:12`, the wall clock in Munich.
#[cfg(test)]
pub fn write_clock(unix_ms: f64, out: &mut String) {
    let seconds = (munich_ms(unix_ms) / 1e3).floor() as i64;
    let of_day = seconds.rem_euclid(86_400);
    let _ = write!(
        out,
        "{:02}:{:02}:{:02}",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    );
}

/// `Tue 10 Nov, 07:45:12 CET` (CEST in summer).
#[cfg(test)]
pub fn format_munich(unix_ms: f64) -> String {
    let mut out = String::new();
    Day::in_munich(unix_ms).write_short(&mut out);
    out.push_str(", ");
    write_clock(unix_ms, &mut out);
    out.push_str(if is_summer(unix_ms) { " CEST" } else { " CET" });
    out
}

/// Where the clock shows the time: the city of the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Zone {
    /// Europe/Berlin: CET, CEST in summer.
    #[default]
    Munich,
    /// Europe/Helsinki: EET, EEST in summer, an hour ahead of Munich.
    Helsinki,
}

impl Zone {
    /// The zone a compiled timetable names.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Europe/Berlin" => Some(Self::Munich),
            "Europe/Helsinki" => Some(Self::Helsinki),
            _ => None,
        }
    }

    /// The offset from UTC at `unix_ms`, ms.
    pub fn offset_ms(self, unix_ms: f64) -> f64 {
        match self {
            Self::Munich => offset_ms(unix_ms),
            Self::Helsinki => offset_ms(unix_ms) + HOUR_MS,
        }
    }

    /// `Tue 10 Nov, 07:45:12 CET` (EET, CEST, EEST) at `unix_ms`.
    pub fn format(self, unix_ms: f64) -> String {
        let wall = unix_ms + self.offset_ms(unix_ms);
        let mut out = String::new();
        Day((wall / DAY_MS).floor() as i32).write_short(&mut out);
        out.push_str(", ");
        let seconds = (wall / 1e3).floor() as i64;
        let of_day = seconds.rem_euclid(86_400);
        let _ = write!(
            out,
            "{:02}:{:02}:{:02} ",
            of_day / 3600,
            of_day / 60 % 60,
            of_day % 60
        );
        out.push_str(match (self, is_summer(unix_ms)) {
            (Self::Munich, false) => "CET",
            (Self::Munich, true) => "CEST",
            (Self::Helsinki, false) => "EET",
            (Self::Helsinki, true) => "EEST",
        });
        out
    }

    /// The year at `unix_ms` there: what "© HSL <year>" names.
    pub fn year(self, unix_ms: f64) -> i32 {
        let wall = unix_ms + self.offset_ms(unix_ms);
        Day((wall / DAY_MS).floor() as i32).ymd().0
    }
}

/// A start time as the example takes it (`TRANSIT_TIME`, `?time=`), in
/// Munich: `07:45` or `07:45:30` on the day `today` (the Munich day of the
/// real clock), or `2026-11-10T07:45` with a date (a space works for the
/// `T`); with a `Z` at the end the time is UTC (`2026-11-06T17:30:00Z`).
/// Unix ms, or `None` for anything else.
pub fn parse_munich(text: &str, today: Day) -> Option<f64> {
    let text = text.trim();
    let (text, utc) = match text.strip_suffix(['Z', 'z']) {
        Some(text) => (text, true),
        None => (text, false),
    };
    let (day, clock) = match text.split_once(['T', ' ']) {
        Some((date, clock)) => (Day::parse_iso(date)?, clock),
        None => (today, text),
    };
    let mut parts = clock.split(':');
    let hours: u32 = parts.next()?.trim().parse().ok()?;
    let minutes: u32 = parts.next()?.trim().parse().ok()?;
    let seconds: u32 = match parts.next() {
        Some(seconds) => seconds.trim().parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    let seconds = f64::from(hours * 3600 + minutes * 60 + seconds);
    Some(if utc {
        day.utc_ms() + seconds * 1e3
    } else {
        from_munich(day, seconds)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(day: Day, hours: f64) -> f64 {
        day.utc_ms() + hours * HOUR_MS
    }

    #[test]
    fn test_days_round_trip_and_weekdays() {
        assert_eq!(Day::from_ymd(1970, 1, 1), Day(0));
        let day = Day::from_ymd(2026, 11, 10);
        assert_eq!(day.ymd(), (2026, 11, 10));
        assert_eq!(day.weekday(), 1, "a Tuesday");
        assert_eq!(Day::from_ymd(2026, 11, 8).weekday(), 6, "a Sunday");
        assert_eq!(Day::parse_compact("20261110"), Some(day));
        assert_eq!(Day::parse_iso("2026-11-10"), Some(day));
        assert_eq!(Day::parse_compact("2026111"), None);
        assert_eq!(Day::parse_compact("20261310"), None);
        assert_eq!(Day::from_ymd(2028, 2, 29).next().ymd(), (2028, 3, 1));
        assert_eq!(Day::from_ymd(2027, 1, 1).previous().ymd(), (2026, 12, 31));
        assert_eq!(day.iso(), "2026-11-10");
        let mut short = String::new();
        day.write_short(&mut short);
        assert_eq!(short, "Tue 10 Nov");
    }

    #[test]
    fn test_summer_time_follows_the_eu_rule() {
        assert_eq!(last_sunday(2026, 10), Day::from_ymd(2026, 10, 25));
        assert_eq!(last_sunday(2027, 3), Day::from_ymd(2027, 3, 28));
        let autumn = Day::from_ymd(2026, 10, 25);
        assert!(is_summer(utc(autumn, 0.99)));
        assert!(!is_summer(utc(autumn, 1.)));
        let spring = Day::from_ymd(2027, 3, 28);
        assert!(!is_summer(utc(spring, 0.99)));
        assert!(is_summer(utc(spring, 1.)));
        assert_eq!(offset_ms(utc(Day::from_ymd(2026, 7, 1), 12.)), 2. * HOUR_MS);
        assert_eq!(offset_ms(utc(Day::from_ymd(2026, 11, 10), 12.)), HOUR_MS);
    }

    #[test]
    fn test_service_day_origin_is_noon_minus_twelve_hours() {
        // An ordinary winter day: local midnight, 23:00 UTC the day before.
        let winter = Day::from_ymd(2026, 11, 10);
        assert_eq!(winter.origin_ms(), utc(winter, -1.));
        // The day the clocks go back: noon is CET, so the origin is 23:00
        // UTC, which the wall clock showed as 01:00 CEST.
        let autumn = Day::from_ymd(2026, 10, 25);
        assert_eq!(autumn.origin_ms(), utc(autumn, -1.));
        assert_eq!(munich_ms(autumn.origin_ms()), utc(autumn, 1.));
        // The day they go forward: noon is CEST, the origin 22:00 UTC,
        // 23:00 CET of the day before on the wall clock.
        let spring = Day::from_ymd(2027, 3, 28);
        assert_eq!(spring.origin_ms(), utc(spring, -2.));
        assert_eq!(munich_ms(spring.origin_ms()), utc(spring, -1.));
    }

    #[test]
    fn test_munich_clock_parses_and_formats() {
        let today = Day::from_ymd(2026, 11, 10);
        let at = parse_munich("07:45", today).expect("a time");
        assert_eq!(at, utc(today, 6.75));
        assert_eq!(
            parse_munich("2026-11-10T07:45:30", today),
            Some(at + 30_000.)
        );
        assert_eq!(
            parse_munich("2026-07-01 12:00", today),
            Some(utc(Day::from_ymd(2026, 7, 1), 10.))
        );
        assert_eq!(parse_munich("2026-11-10T06:45Z", today), Some(at));
        assert_eq!(parse_munich("25:00", today), None);
        assert_eq!(parse_munich("soon", today), None);
        assert_eq!(format_munich(at), "Tue 10 Nov, 07:45:00 CET");
        assert_eq!(Day::in_munich(utc(today, 23.5)), today.next());
        // The skipped hour in spring is read an hour later.
        let spring = Day::from_ymd(2027, 3, 28);
        assert_eq!(from_munich(spring, 2.5 * 3600.), utc(spring, 1.5));
    }

    #[test]
    fn test_zones_format_their_own_clock() {
        let today = Day::from_ymd(2026, 11, 10);
        let at = utc(today, 6.75);
        assert_eq!(Zone::Munich.format(at), format_munich(at));
        assert_eq!(Zone::Helsinki.format(at), "Tue 10 Nov, 08:45:00 EET");
        let summer = utc(Day::from_ymd(2026, 7, 1), 22.);
        assert_eq!(Zone::Helsinki.format(summer), "Thu 2 Jul, 01:00:00 EEST");
        let new_year = utc(Day::from_ymd(2026, 12, 31), 22.5);
        assert_eq!(Zone::Helsinki.year(new_year), 2027);
        assert_eq!(Zone::Munich.year(new_year), 2026);
        assert_eq!(Zone::from_name("Europe/Helsinki"), Some(Zone::Helsinki));
        assert_eq!(Zone::from_name("Europe/Paris"), None);
    }
}
