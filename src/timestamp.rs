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
}
