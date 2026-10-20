//! TimeStamp converts from and to the 14-digit `YYYYMMDDHHMMSS` format
//! MediaWiki uses in its database and API.

use chrono::{DateTime, NaiveDateTime, TimeDelta, Timelike, Utc};
use std::time::Duration;

const FORMAT: &str = "%Y%m%d%H%M%S";

#[derive(Debug, Clone, Default)]
pub struct TimeStamp {}

impl TimeStamp {
    /// Returns the current UTF time as a timestamp, 14 char format
    pub fn now() -> String {
        Utc::now().format(FORMAT).to_string()
    }

    /// Returns the given UTF time as a timestamp, 14 char format
    pub fn datetime(utc: &DateTime<Utc>) -> String {
        utc.format(FORMAT).to_string()
    }

    /// Returns the given naive (UTC) time as a timestamp, 14 char format.
    pub fn naive(naive: &NaiveDateTime) -> String {
        naive.format(FORMAT).to_string()
    }

    /// The timestamp `age` ago. Saturates at the earliest time chrono can
    /// represent rather than failing.
    pub fn ago(age: Duration) -> String {
        let then = TimeDelta::from_std(age)
            .ok()
            .and_then(|age| Utc::now().checked_sub_signed(age))
            .unwrap_or(DateTime::<Utc>::MIN_UTC);
        Self::datetime(&then)
    }

    /// The timestamp `days` days ago, at the current time of day.
    pub fn days_ago(days: u32) -> String {
        Self::ago(Duration::from_secs(u64::from(days) * 86_400))
    }

    /// Midnight UTC at the start of today, `YYYYMMDD000000`.
    pub fn start_of_today() -> String {
        Self::start_of_day_ago(0)
    }

    /// Midnight UTC at the start of the day `days` days ago,
    /// `YYYYMMDD000000` (`0` is today).
    pub fn start_of_day_ago(days: u32) -> String {
        Utc::now()
            .date_naive()
            .checked_sub_days(chrono::Days::new(u64::from(days)))
            .unwrap_or(chrono::NaiveDate::MIN)
            .format("%Y%m%d000000")
            .to_string()
    }

    /// Converts a 14-digit timestamp to RFC 3339 / ISO 8601 in UTC
    /// (`2023-09-01T12:34:56Z`), as Atom feeds and JSON APIs want it; `None`
    /// unless it is a valid timestamp.
    pub fn rfc3339(ts: &str) -> Option<String> {
        Some(
            Self::str2naive(ts)?
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string(),
        )
    }

    /// Parses a 14-digit `YYYYMMDDHHMMSS` timestamp; `None` unless it names an
    /// existing date and time.
    pub fn str2naive(ts: &str) -> Option<NaiveDateTime> {
        // chrono would also take a field with fewer or (the year) more digits.
        if ts.len() != 14 || !ts.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let naive = NaiveDateTime::parse_from_str(ts, FORMAT).ok()?;
        // chrono reads a second of 60 as a leap second, at any minute of any
        // day; a MediaWiki timestamp never has one.
        (naive.nanosecond() < 1_000_000_000).then_some(naive)
    }

    /// As [`Self::str2naive`], as a UTC time.
    pub fn str2utc(ts: &str) -> Option<DateTime<Utc>> {
        Some(Self::str2naive(ts)?.and_utc())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_start_of_day_ago() {
        assert_eq!(TimeStamp::start_of_day_ago(0), TimeStamp::start_of_today());
        let yesterday = (Utc::now().date_naive() - chrono::Days::new(1))
            .format("%Y%m%d000000")
            .to_string();
        assert_eq!(TimeStamp::start_of_day_ago(1), yesterday);
        assert_eq!(TimeStamp::start_of_day_ago(1).len(), 14);
    }
    use super::*;

    #[test]
    fn test_now() {
        let ts = TimeStamp::now();
        assert_eq!(ts.len(), 14);
        assert_eq!(ts.chars().filter(|c| c.is_numeric()).count(), 14);
    }

    #[test]
    fn test_str2naive() {
        let dt: NaiveDateTime = chrono::NaiveDate::from_ymd_opt(2023, 9, 1)
            .unwrap()
            .and_hms_opt(12, 34, 56)
            .unwrap();
        let ts = "20230901123456";
        assert_eq!(TimeStamp::str2naive(ts).unwrap(), dt);
    }

    #[test]
    fn test_str2naive_invalid() {
        assert!(TimeStamp::str2naive("not-a-timestamp").is_none());
        assert!(TimeStamp::str2naive("").is_none());
        // Too short — only a year, not a full 14-char stamp
        assert!(TimeStamp::str2naive("2023").is_none());
        // Wrong separator style
        assert!(TimeStamp::str2naive("2023-09-01 12:34:56").is_none());
    }

    #[test]
    fn test_datetime() {
        use chrono::TimeZone;
        let dt = chrono::Utc
            .with_ymd_and_hms(2023, 9, 1, 12, 34, 56)
            .unwrap();
        assert_eq!(TimeStamp::datetime(&dt), "20230901123456");
    }

    #[test]
    fn test_datetime_midnight() {
        use chrono::TimeZone;
        let dt = chrono::Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(TimeStamp::datetime(&dt), "20000101000000");
    }

    #[test]
    fn test_str2utc() {
        use chrono::TimeZone;
        let expected = chrono::Utc
            .with_ymd_and_hms(2023, 9, 1, 12, 34, 56)
            .unwrap();
        let result = TimeStamp::str2utc("20230901123456").unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_str2utc_roundtrip() {
        // A datetime serialised with TimeStamp::datetime must round-trip through str2utc.
        use chrono::TimeZone;
        let original = chrono::Utc
            .with_ymd_and_hms(1999, 12, 31, 23, 59, 59)
            .unwrap();
        let ts = TimeStamp::datetime(&original);
        let recovered = TimeStamp::str2utc(&ts).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn test_str2utc_invalid() {
        assert!(TimeStamp::str2utc("not-a-timestamp").is_none());
        assert!(TimeStamp::str2utc("").is_none());
        assert!(TimeStamp::str2utc("20231399000000").is_none()); // month 13 is invalid
    }

    #[test]
    fn test_impossible_dates_and_times_rejected() {
        for impossible in [
            "20230229000000", // not a leap year
            "21000229000000", // not a leap year either
            "20230230000000",
            "20230431000000",
            "20230100000000",
            "20230101240000",
            "20230101006000",
            "20230101000060",
            "2023010100000",   // too short
            "202301010000000", // too long
        ] {
            assert!(TimeStamp::str2naive(impossible).is_none(), "{impossible}");
            assert!(TimeStamp::str2utc(impossible).is_none(), "{impossible}");
        }
        assert!(TimeStamp::str2utc("20240229235959").is_some());
        assert!(TimeStamp::str2utc("20000229000000").is_some());
    }

    #[test]
    fn test_naive() {
        let naive = chrono::NaiveDate::from_ymd_opt(2023, 9, 1)
            .unwrap()
            .and_hms_opt(1, 2, 3)
            .unwrap();
        assert_eq!(TimeStamp::naive(&naive), "20230901010203");
        assert_eq!(TimeStamp::str2naive(&TimeStamp::naive(&naive)), Some(naive));
    }

    #[test]
    fn test_ago() {
        let expected = TimeStamp::datetime(&(Utc::now() - TimeDelta::days(7)));
        let got = TimeStamp::days_ago(7);
        // Allow for the clock ticking between the two readings.
        let diff = TimeStamp::str2utc(&expected).unwrap() - TimeStamp::str2utc(&got).unwrap();
        assert!(diff.num_seconds().abs() <= 2, "{expected} vs {got}");
        assert_eq!(TimeStamp::days_ago(0).len(), 14);
        // Saturates instead of failing.
        assert_eq!(
            TimeStamp::ago(Duration::MAX).len(),
            TimeStamp::datetime(&DateTime::<Utc>::MIN_UTC).len()
        );
    }

    #[test]
    fn test_start_of_today() {
        let ts = TimeStamp::start_of_today();
        assert!(ts.ends_with("000000"));
        assert_eq!(&ts[..8], &TimeStamp::now()[..8]);
    }

    #[test]
    fn test_rfc3339() {
        assert_eq!(
            TimeStamp::rfc3339("20230901123456").as_deref(),
            Some("2023-09-01T12:34:56Z")
        );
        assert_eq!(TimeStamp::rfc3339("20230230000000"), None);
        assert_eq!(TimeStamp::rfc3339("garbage"), None);
    }
}
