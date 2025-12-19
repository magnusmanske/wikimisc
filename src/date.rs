//! Useful functions to handle dates.

use anyhow::{anyhow, Result};
use regex::Regex;
use std::str::FromStr;

lazy_static! {
    static ref DATES : Vec<(Regex,String,u64)> = {
        // NOTE: The pattern always needs to cover the whole string, so use ^$
        vec![
            (Regex::new(r"^(\d{3,})$").unwrap(),"+${1}-00-00T00:00:00Z".to_string(),9),
            (Regex::new(r"^(\d{3,})-(\d{2})$").unwrap(),"+${1}-${2}-00T00:00:00Z".to_string(),10),
            (Regex::new(r"^(\d{3,})-(\d{2})-(\d{2})$").unwrap(),"+${1}-${2}-${3}T00:00:00Z".to_string(),11),
            (Regex::new(r"^https?://data.bnf.fr/date/(\d+)/?$").unwrap(),"+${1}-00-00T00:00:00Z".to_string(),9), // Why not?
            (Regex::new(r"^([+-]?\d{3,})-(\d{2})-(\d{2})T\d{2}:\d{2}:\d{2}Z/11$").unwrap(),"+${1}-${2}-${3}T00:00:00Z".to_string(),11),
            (Regex::new(r"^([+-]?\d{3,})-(\d{2})-0[01]T\d{2}:\d{2}:\d{2}Z/10$").unwrap(),"+${1}-${2}-00T00:00:00Z".to_string(),10),
            (Regex::new(r"^([+-]?\d{3,})-0[01]-0[01]T\d{2}:\d{2}:\d{2}Z/9$").unwrap(),"+${1}-00-00T00:00:00Z".to_string(),9),
        ]
    };
}

pub struct Date {
    time: String,
    precision: u64,
}

impl Date {
    /// Returns the date as a QuickStatements-compatible string.
    pub fn as_qs(&self) -> String {
        format!("{}/{}", self.time, self.precision)
    }

    /// Returns the date as a wikibase timevalue-compatible string.
    pub fn time(&self) -> &str {
        &self.time
    }

    /// Returns the precision of the date.
    pub fn precision(&self) -> u64 {
        self.precision
    }
}

impl FromStr for Date {
    type Err = anyhow::Error;

    /// Parses a date from a string. Returns None if the string is not a recognized or valid date.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (time, precision) = DATES
            .iter()
            .find_map(|e| {
                let replaced = e.0.replace_all(s, &e.1);
                (replaced != s).then(|| (replaced.to_string(), e.2))
            })
            .ok_or_else(|| anyhow!("Could not parse '{s}' into date"))?;

        let parts: Vec<&str> = time.split('-').collect();
        let year = parts
            .first()
            .ok_or_else(|| anyhow!("Could not parse '{s}' into date"))?
            .parse::<i16>()?;
        if year >= 2025 {
            return Err(anyhow!("Could not parse '{s}' into date"));
        }

        let month = parts
            .get(1)
            .ok_or_else(|| anyhow!("Could not parse '{s}' into date"))?
            .parse::<u8>()?;
        if month > 12 || (month == 0 && precision >= 11) {
            return Err(anyhow!("Could not parse '{s}' into date"));
        }

        let day_part = parts
            .get(2)
            .ok_or_else(|| anyhow!("Could not parse '{s}' into date"))?;
        let day = day_part
            .split('T')
            .next()
            .ok_or_else(|| anyhow!("Could not parse '{s}' into date"))?
            .parse::<u8>()?;
        if precision >= 11 && !(1..=31).contains(&day) {
            return Err(anyhow!("Could not parse '{s}' into date"));
        }
        Ok(Self { time, precision })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date() {
        assert_eq!(
            Date::from_str("1234").unwrap().time,
            "+1234-00-00T00:00:00Z"
        );
        assert_eq!(
            Date::from_str("1234-05").unwrap().time,
            "+1234-05-00T00:00:00Z"
        );
        assert!(Date::from_str("1234-15").is_err());
        assert_eq!(
            Date::from_str("1234-05-17").unwrap().time,
            "+1234-05-17T00:00:00Z"
        );
        assert_eq!(
            Date::from_str("https://data.bnf.fr/date/1234/")
                .unwrap()
                .time,
            "+1234-00-00T00:00:00Z"
        );
        assert_eq!(
            Date::from_str("https://data.bnf.fr/date/1234")
                .unwrap()
                .time,
            "+1234-00-00T00:00:00Z"
        );
        assert_eq!(
            Date::from_str("https://data.bnf.fr/date/1234/")
                .unwrap()
                .precision,
            9
        );
        assert_eq!(
            Date::from_str("https://data.bnf.fr/date/1234")
                .unwrap()
                .precision,
            9
        );
        assert_eq!(Date::from_str("1234-05-17").unwrap().precision, 11);
        assert_eq!(Date::from_str("1234-05").unwrap().precision, 10);
        assert_eq!(Date::from_str("1234").unwrap().precision, 9);
        assert_eq!(
            Date::from_str("1234-05-17T00:00:00Z/11").unwrap().time,
            "+1234-05-17T00:00:00Z"
        );
        assert_eq!(
            Date::from_str("1234-05-17T00:00:00Z/11").unwrap().precision,
            11
        );
        assert_eq!(
            Date::from_str("1234-05-00T00:00:00Z/10").unwrap().time,
            "+1234-05-00T00:00:00Z"
        );
        assert_eq!(
            Date::from_str("1234-05-01T00:00:00Z/10").unwrap().precision,
            10
        );
        assert_eq!(
            Date::from_str("1234-00-01T00:00:00Z/9").unwrap().precision,
            9
        );
        assert_eq!(
            Date::from_str("1234-01-00T00:00:00Z/9").unwrap().time,
            "+1234-00-00T00:00:00Z"
        );
    }

    #[test]
    fn test_as_qs() {
        assert_eq!(
            Date::from_str("1234").unwrap().as_qs(),
            "+1234-00-00T00:00:00Z/9"
        );
        assert_eq!(
            Date::from_str("1234-05").unwrap().as_qs(),
            "+1234-05-00T00:00:00Z/10"
        );
        assert_eq!(
            Date::from_str("1234-05-17").unwrap().as_qs(),
            "+1234-05-17T00:00:00Z/11"
        );
    }

    #[test]
    fn test_time() {
        assert_eq!(
            Date::from_str("1234").unwrap().time(),
            "+1234-00-00T00:00:00Z"
        );
        assert_eq!(
            Date::from_str("1234-05").unwrap().time(),
            "+1234-05-00T00:00:00Z"
        );
        assert_eq!(
            Date::from_str("1234-05-17").unwrap().time(),
            "+1234-05-17T00:00:00Z"
        );
    }

    #[test]
    fn test_precision() {
        assert_eq!(Date::from_str("1234").unwrap().precision(), 9);
        assert_eq!(Date::from_str("1234-05").unwrap().precision(), 10);
        assert_eq!(Date::from_str("1234-05-17").unwrap().precision(), 11);
    }

    #[test]
    fn test_invalid_month() {
        assert!(Date::from_str("1234-13").is_err());
        assert!(Date::from_str("1234-99").is_err());
        // Month 00 is valid for year-only precision (gets converted to precision 9)
    }

    #[test]
    fn test_invalid_day() {
        assert!(Date::from_str("1234-05-00").is_err());
        assert!(Date::from_str("1234-05-32").is_err());
        assert!(Date::from_str("1234-05-99").is_err());
    }

    #[test]
    fn test_future_year_rejected() {
        assert!(Date::from_str("2025").is_err());
        assert!(Date::from_str("2026").is_err());
        assert!(Date::from_str("3000").is_err());
    }

    #[test]
    fn test_partial_precision_formats() {
        // Test that month=00 is allowed with precision 9 (year only)
        let date = Date::from_str("1234-00-01T00:00:00Z/9").unwrap();
        assert_eq!(date.precision(), 9);
        assert_eq!(date.time(), "+1234-00-00T00:00:00Z");

        // Test that day=00 or 01 is allowed with precision 10 (year-month)
        let date = Date::from_str("1234-05-00T00:00:00Z/10").unwrap();
        assert_eq!(date.precision(), 10);
        assert_eq!(date.time(), "+1234-05-00T00:00:00Z");
    }

    #[test]
    fn test_bnf_url_variations() {
        let with_slash = Date::from_str("https://data.bnf.fr/date/1500/").unwrap();
        let without_slash = Date::from_str("https://data.bnf.fr/date/1500").unwrap();
        assert_eq!(with_slash.time(), without_slash.time());
        assert_eq!(with_slash.precision(), 9);
        assert_eq!(without_slash.precision(), 9);
    }

    #[test]
    fn test_invalid_format() {
        assert!(Date::from_str("not-a-date").is_err());
        assert!(Date::from_str("").is_err());
        assert!(Date::from_str("abc-def-ghi").is_err());
    }
}
