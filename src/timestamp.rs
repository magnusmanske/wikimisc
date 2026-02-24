//! TimeStamp convertes from and to YYYYMMDDHHMMSS format

use chrono::{DateTime, NaiveDateTime, Utc};

#[derive(Debug, Clone, Default)]
pub struct TimeStamp {}

impl TimeStamp {
    /// Returns the current UTF time as a timestamp, 14 char format
    pub fn now() -> String {
        Utc::now().format("%Y%m%d%H%M%S").to_string()
    }

    /// Returns the given UTF time as a timestamp, 14 char format
    pub fn datetime(utc: &DateTime<Utc>) -> String {
        utc.format("%Y%m%d%H%M%S").to_string()
    }

    pub fn str2naive(ts: &str) -> Option<NaiveDateTime> {
        NaiveDateTime::parse_from_str(ts, "%Y%m%d%H%M%S").ok()
    }

    pub fn str2utc(ts: &str) -> Option<DateTime<Utc>> {
        match NaiveDateTime::parse_from_str(ts, "%Y%m%d%H%M%S")
            .ok()?
            .and_local_timezone(Utc)
        {
            chrono::offset::LocalResult::Single(d) => Some(d),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
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
}
