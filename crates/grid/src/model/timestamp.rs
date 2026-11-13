//! Points in time (`chrono::DateTime<Utc>`, [`CellValue::Timestamp`]):
//! their text in a cell without `core::fmt`, and their serialized form.
//!
//! Only types and UTC text: the crate never reads a clock here (no
//! `Utc::now()`, no `Local`); the store's clock is the grid's only clock.
//!
//! [`CellValue::Timestamp`]: super::CellValue::Timestamp

use std::fmt;

use chrono::{DateTime, Datelike as _, SecondsFormat, Timelike as _, Utc};
use serde::{Serialize, Serializer, de};

use super::float_text::DIGIT_PAIRS;

/// The length of `2026-11-04 12:34:56`.
const TEXT_LEN: usize = 19;

/// Appends `value` in UTC as `2026-11-04 12:34:56`, the text of a
/// timestamp cell: 19 ASCII characters for the years 0 to 9999, so the
/// text sorts in time order. A year outside them keeps its sign and digits
/// (`+10000-01-01 00:00:00`), as RFC 3339 writes it. A leap second shows
/// as `:60`, as `chrono` writes it.
///
/// Writes digit pairs on the stack and pushes them once, without
/// `core::fmt`: no allocation beyond `out` growing. A frame formats every
/// visible timestamp cell.
pub(crate) fn push_timestamp(value: DateTime<Utc>, out: &mut String) {
    let time = value.naive_utc();
    let year = time.year();
    let four_digits = (0..=9999).contains(&year);
    let mut text = [0; TEXT_LEN];
    let pair = |text: &mut [u8; TEXT_LEN], at: usize, value: u32| {
        let ix = value as usize * 2;
        text[at..at + 2].copy_from_slice(&DIGIT_PAIRS[ix..ix + 2]);
    };
    if four_digits {
        pair(&mut text, 0, year as u32 / 100);
        pair(&mut text, 2, year as u32 % 100);
    }
    text[4] = b'-';
    pair(&mut text, 5, time.month());
    text[7] = b'-';
    pair(&mut text, 8, time.day());
    text[10] = b' ';
    pair(&mut text, 11, time.hour());
    text[13] = b':';
    pair(&mut text, 14, time.minute());
    text[16] = b':';
    // `chrono` keeps a leap second as second 59 with a nanosecond past 1e9.
    let leap = u32::from(time.nanosecond() >= 1_000_000_000);
    pair(&mut text, 17, time.second() + leap);
    if four_digits {
        // ASCII digits and separators only.
        out.push_str(std::str::from_utf8(&text).unwrap_or_default());
    } else {
        use std::fmt::Write as _;
        let _ = write!(out, "{year:+05}");
        out.push_str(std::str::from_utf8(&text[4..]).unwrap_or_default());
    }
}

/// A timestamp serialized as RFC 3339 text in UTC, with as many fraction
/// digits as it has (none, 3, 6 or 9), so it reads back exactly:
/// `"2026-11-04T12:34:56Z"`, `"2026-11-04T12:34:56.250Z"`.
pub(super) struct TimestampText(pub(super) DateTime<Utc>);

impl Serialize for TimestampText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let year = self.0.year();
        if !(0..=9999).contains(&year) {
            // RFC 3339 has four-digit years only: the text would not read back.
            return Err(serde::ser::Error::custom(format_args!(
                "the year {year} is outside RFC 3339"
            )));
        }
        serializer.serialize_str(&self.0.to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }
}

/// Reads RFC 3339 text with any offset, as the point in time it names:
/// `2026-11-04T15:34:56+03:00` is `2026-11-04T12:34:56Z`.
pub(super) fn read_timestamp<E: de::Error>(text: &str) -> Result<DateTime<Utc>, E> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| E::invalid_value(de::Unexpected::Str(text), &Rfc3339))
}

struct Rfc3339;

impl de::Expected for Rfc3339 {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an RFC 3339 date and time")
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone as _};

    use super::*;

    fn text(value: DateTime<Utc>) -> String {
        let mut out = String::new();
        push_timestamp(value, &mut out);
        out
    }

    #[test]
    fn test_push_timestamp_matches_chrono() {
        // The fast path writes what `chrono` writes, on random points over
        // the four-digit years, fractions and leap seconds included.
        let mut rng = fastrand::Rng::with_seed(0x7157);
        let first = Utc.with_ymd_and_hms(0, 1, 1, 0, 0, 0).unwrap().timestamp();
        let last = Utc
            .with_ymd_and_hms(9999, 12, 31, 23, 59, 59)
            .unwrap()
            .timestamp();
        for _ in 0..20_000 {
            let nanos = match rng.u8(0..3) {
                0 => 0,
                1 => rng.u32(..1_000_000_000),
                _ => rng.u32(1_000_000_000..2_000_000_000),
            };
            let Some(value) = DateTime::from_timestamp(rng.i64(first..=last), nanos) else {
                continue;
            };
            let expected = value.format("%Y-%m-%d %H:%M:%S").to_string();
            assert_eq!(text(value), expected, "{value:?}");
        }
        // A leap second, the last of 2016.
        let leap = DateTime::from_timestamp(1_483_228_799, 1_500_000_000).unwrap();
        assert_eq!(text(leap), "2016-12-31 23:59:60");
        assert_eq!(text(leap), leap.format("%Y-%m-%d %H:%M:%S").to_string());
        // Outside the four-digit years the sign shows, as RFC 3339 writes it.
        let far = NaiveDate::from_ymd_opt(10_000, 1, 2)
            .unwrap()
            .and_hms_opt(3, 4, 5)
            .unwrap()
            .and_utc();
        assert_eq!(text(far), "+10000-01-02 03:04:05");
        let before = NaiveDate::from_ymd_opt(-1, 12, 31)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        assert_eq!(text(before), "-0001-12-31 00:00:00");
        // Appends: what is already there stays.
        let mut out = String::from("at ");
        push_timestamp(DateTime::UNIX_EPOCH, &mut out);
        assert_eq!(out, "at 1970-01-01 00:00:00");
    }
}
