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

    pub fn from_str(ts: &str) -> Option<DateTime<Utc>> {
        match NaiveDateTime::parse_from_str(ts, "%Y%m%d%H%M%S")
            .ok()?
            .and_local_timezone(Utc)
        {
            chrono::offset::LocalResult::Single(d) => Some(d),
            _ => None,
        }
    }
}
