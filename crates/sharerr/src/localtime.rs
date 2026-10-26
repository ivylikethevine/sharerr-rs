//! The container's local time — `TZ`, else `/etc/localtime`, with the zone
//! database bundled — for quiet hours and share-until dates. UTC when
//! neither names a zone.

use jiff::civil::Date;
use jiff::tz::TimeZone;

/// Minutes past local midnight, now.
pub fn minute_of_day() -> u16 {
    let now = jiff::Zoned::now();
    u16::try_from(now.hour()).unwrap_or(0) * 60 + u16::try_from(now.minute()).unwrap_or(0)
}

/// The Unix second local day `date` (`YYYY-MM-DD`) starts at.
pub fn start_of_day(date: &str) -> Result<i64, String> {
    start_of_day_in(date, &TimeZone::system())
}

/// `timestamp` as a local `YYYY-MM-DD`.
pub fn date_of(timestamp: i64) -> String {
    date_of_in(timestamp, &TimeZone::system())
}

fn start_of_day_in(date: &str, zone: &TimeZone) -> Result<i64, String> {
    let date: Date = date
        .trim()
        .parse()
        .map_err(|_| format!("{date:?} is not a date (YYYY-MM-DD)"))?;
    date.to_zoned(zone.clone())
        .map(|start| start.timestamp().as_second())
        .map_err(|err| err.to_string())
}

fn date_of_in(timestamp: i64, zone: &TimeZone) -> String {
    jiff::Timestamp::from_second(timestamp)
        .map(|ts| ts.to_zoned(zone.clone()).date().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_date_starts_at_local_midnight_and_reads_back() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        // 2026-11-01 00:00 CET is 2026-10-31 23:00 UTC.
        let start = start_of_day_in("2026-11-01", &berlin).unwrap();
        assert_eq!(start, 1_793_487_600);
        assert_eq!(date_of_in(start, &berlin), "2026-11-01");
        assert_eq!(date_of_in(start, &TimeZone::UTC), "2026-10-31");
        assert!(start_of_day_in("next tuesday", &berlin).is_err());
    }

    #[test]
    fn the_system_zone_answers_in_range() {
        assert!(minute_of_day() < 24 * 60);
        assert_eq!(date_of(start_of_day("2026-03-15").unwrap()), "2026-03-15");
    }
}
