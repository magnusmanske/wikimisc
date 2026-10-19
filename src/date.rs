//! Useful functions to handle dates.
//!
//! # Range restrictions
//!
//! [`Date::from_str`] is intentionally conservative about what it accepts:
//!
//! - **Future years are rejected.** Any year strictly greater than the current
//!   UTC year fails to parse. This catches typos like `3000` when the input
//!   should have been `2000`. If you need to represent future dates, parse
//!   them yourself.
//! - **BCE years are not supported.** The Wikibase-style patterns accept an
//!   optional leading `+` (as in Wikidata's canonical `+1776-07-04T00:00:00Z/11`)
//!   but no `-`, so explicitly negative inputs like `-0500-01-01T00:00:00Z/11`
//!   are not recognised and fail.
//! - **Two-digit years are rejected.** The patterns require `\d{3,}`, so a
//!   bare `99` will not parse. This avoids ambiguity between, e.g., 1999 and 99.
//! - **Impossible dates are rejected.** A month of `00` is accepted only at
//!   year precision (9), and at day precision (11) the day must exist in its
//!   month in the proleptic Gregorian calendar: `1999-02-29` and `1999-04-31`
//!   fail, `2000-02-29` parses.

use chrono::Datelike;
use regex::Regex;
use std::num::ParseIntError;
use std::str::FromStr;
use std::sync::LazyLock;
use thiserror::Error;

/// Failure modes of [`Date::from_str`].
#[derive(Debug, Error)]
pub enum DateError {
    /// The string does not look like any date this module recognises, or it
    /// does but the resulting date is not a valid one (month 13, day 32, a year
    /// in the future, …).
    #[error("could not parse '{0}' into a date")]
    Unparsable(String),

    /// A year/month/day component matched but is not a number.
    #[error("could not parse the numeric components of date '{input}'")]
    NotANumber {
        input: String,
        #[source]
        source: ParseIntError,
    },
}

/// Pattern / replacement / precision triples, tried in order by [`Date::from_str`].
///
/// The patterns are literals, so `Regex::new` cannot fail in practice. Rather
/// than panic if one somehow did, that entry is skipped — `from_str` already
/// returns [`DateError::Unparsable`] for input it does not recognise, so the
/// failure surfaces as an ordinary error instead of taking the caller down.
/// `test_all_date_patterns_compile` makes sure CI catches a broken literal.
static DATES: LazyLock<Vec<(Regex, String, u64)>> = LazyLock::new(|| {
    // NOTE: The pattern always needs to cover the whole string, so use ^$
    const PATTERNS: &[(&str, &str, u64)] = &[
        (r"^(\d{3,})$", "+${1}-00-00T00:00:00Z", 9),
        (r"^(\d{3,})-(\d{2})$", "+${1}-${2}-00T00:00:00Z", 10),
        (
            r"^(\d{3,})-(\d{2})-(\d{2})$",
            "+${1}-${2}-${3}T00:00:00Z",
            11,
        ),
        // Why not?
        (
            r"^https?://data.bnf.fr/date/(\d+)/?$",
            "+${1}-00-00T00:00:00Z",
            9,
        ),
        (
            r"^\+?(\d{3,})-(\d{2})-(\d{2})T\d{2}:\d{2}:\d{2}Z/11$",
            "+${1}-${2}-${3}T00:00:00Z",
            11,
        ),
        (
            r"^\+?(\d{3,})-(\d{2})-0[01]T\d{2}:\d{2}:\d{2}Z/10$",
            "+${1}-${2}-00T00:00:00Z",
            10,
        ),
        (
            r"^\+?(\d{3,})-0[01]-0[01]T\d{2}:\d{2}:\d{2}Z/9$",
            "+${1}-00-00T00:00:00Z",
            9,
        ),
    ];

    PATTERNS
        .iter()
        .filter_map(|(pattern, replacement, precision)| {
            Some((
                Regex::new(pattern).ok()?,
                replacement.to_string(),
                *precision,
            ))
        })
        .collect()
});

/// Number of entries [`DATES`] is expected to hold; asserted by the tests so a
/// pattern that stops compiling fails the build rather than degrading silently.
#[cfg(test)]
const DATE_PATTERN_COUNT: usize = 7;

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
    type Err = DateError;

    /// Parses a date from a string. Returns None if the string is not a recognized or valid date.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let unparsable = || DateError::Unparsable(s.to_string());
        let not_a_number = |source| DateError::NotANumber {
            input: s.to_string(),
            source,
        };

        let (time, precision) = DATES
            .iter()
            .find_map(|e| {
                let replaced = e.0.replace_all(s, &e.1);
                (replaced != s).then(|| (replaced.into_owned(), e.2))
            })
            .ok_or_else(unparsable)?;

        let parts: Vec<&str> = time.split('-').collect();
        let year = parts
            .first()
            .ok_or_else(unparsable)?
            .parse::<i32>()
            .map_err(not_a_number)?;
        let current_year = chrono::Utc::now().year();
        if year > current_year {
            return Err(unparsable());
        }

        // Month precision (10) and finer need a month; year precision (9)
        // keeps Wikidata's `00` placeholder.
        let month = parts
            .get(1)
            .ok_or_else(unparsable)?
            .parse::<u32>()
            .map_err(not_a_number)?;
        if month > 12 || (month == 0 && precision >= 10) {
            return Err(unparsable());
        }

        let day_part = parts.get(2).ok_or_else(unparsable)?;
        let day = day_part
            .split('T')
            .next()
            .ok_or_else(unparsable)?
            .parse::<u32>()
            .map_err(not_a_number)?;
        // Day precision (11) needs a day that exists in that month, leap years
        // included (proleptic Gregorian, as chrono and Wikidata's default
        // calendar count; years here are never negative).
        if precision >= 11 && chrono::NaiveDate::from_ymd_opt(year, month, day).is_none() {
            return Err(unparsable());
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
    fn test_month_00_rejected_at_month_precision() {
        assert!(Date::from_str("1999-00").is_err());
        assert!(Date::from_str("+1999-00-00T00:00:00Z/10").is_err());
        assert!(Date::from_str("1999-00-00T00:00:00Z/10").is_err());
        // Year precision keeps its placeholder month.
        assert_eq!(
            Date::from_str("+1999-00-00T00:00:00Z/9").unwrap().as_qs(),
            "+1999-00-00T00:00:00Z/9"
        );
    }

    #[test]
    fn test_day_must_exist_in_its_month() {
        for impossible in [
            "1999-02-29",
            "1999-02-30",
            "1999-04-31",
            "1900-02-29",
            "+1999-06-31T00:00:00Z/11",
        ] {
            assert!(Date::from_str(impossible).is_err(), "{impossible}");
        }
        for possible in ["2000-02-29", "1996-02-29", "1999-04-30", "1999-12-31"] {
            assert_eq!(
                Date::from_str(possible).unwrap().precision(),
                11,
                "{possible}"
            );
        }
        // Coarser precisions do not look at the day.
        assert_eq!(
            Date::from_str("1999-02-00T00:00:00Z/10").unwrap().time(),
            "+1999-02-00T00:00:00Z"
        );
    }

    #[test]
    fn test_invalid_day() {
        assert!(Date::from_str("1234-05-00").is_err());
        assert!(Date::from_str("1234-05-32").is_err());
        assert!(Date::from_str("1234-05-99").is_err());
    }

    #[test]
    fn test_future_year_rejected() {
        // Years strictly after the current year must be rejected
        let next_year = (chrono::Utc::now().year() + 1).to_string();
        assert!(Date::from_str(&next_year).is_err());
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

    #[test]
    fn test_current_year_accepted() {
        // The current year must always be accepted
        let current_year = chrono::Utc::now().year().to_string();
        let d = Date::from_str(&current_year).unwrap();
        assert_eq!(d.precision(), 9);
        assert!(d.time().contains(&current_year));
    }

    #[test]
    fn test_current_year_full_date_accepted() {
        let current_year = chrono::Utc::now().year();
        let s = format!("{current_year}-06-15");
        let d = Date::from_str(&s).unwrap();
        assert_eq!(d.time(), format!("+{current_year}-06-15T00:00:00Z"));
        assert_eq!(d.precision(), 11);
    }

    #[test]
    fn test_bce_year_not_supported() {
        // The Wikibase-style patterns accept a leading '+' but not '-'.
        // BCE years are not supported by Date::from_str.
        for s in [
            "-0500-01-01T00:00:00Z/11",
            "-0500-01-00T00:00:00Z/10",
            "-1000-01-01T00:00:00Z/9",
        ] {
            assert!(
                matches!(Date::from_str(s), Err(DateError::Unparsable(_))),
                "{s}"
            );
        }
    }

    #[test]
    fn test_positive_signed_year_via_wikibase_format() {
        // Regression: Wikidata's canonical form carries an explicit '+', which
        // the replacement used to double into '++1776-…'.
        let d = Date::from_str("+1776-07-04T00:00:00Z/11").unwrap();
        assert_eq!(d.time(), "+1776-07-04T00:00:00Z");
        assert_eq!(d.precision(), 11);
        let d = Date::from_str("+1776-07-00T00:00:00Z/10").unwrap();
        assert_eq!(d.time(), "+1776-07-00T00:00:00Z");
        assert_eq!(d.precision(), 10);
        let d = Date::from_str("+1776-00-00T00:00:00Z/9").unwrap();
        assert_eq!(d.time(), "+1776-00-00T00:00:00Z");
        assert_eq!(d.precision(), 9);
        // The unsigned form keeps working.
        let d = Date::from_str("1776-07-04T00:00:00Z/11").unwrap();
        assert_eq!(d.time(), "+1776-07-04T00:00:00Z");
        // One sign only.
        assert!(Date::from_str("++1776-07-04T00:00:00Z/11").is_err());
    }

    #[test]
    fn test_three_digit_year() {
        let d = Date::from_str("800").unwrap();
        assert_eq!(d.time(), "+800-00-00T00:00:00Z");
        assert_eq!(d.precision(), 9);
    }

    #[test]
    fn test_three_digit_year_with_month() {
        let d = Date::from_str("800-03").unwrap();
        assert_eq!(d.time(), "+800-03-00T00:00:00Z");
        assert_eq!(d.precision(), 10);
    }

    #[test]
    fn test_two_digit_year_rejected() {
        // Regex requires \d{3,} so two-digit years are not matched
        assert!(Date::from_str("99").is_err());
        assert!(Date::from_str("99-01").is_err());
    }

    #[test]
    fn test_bnf_url_with_three_digit_year() {
        let d = Date::from_str("https://data.bnf.fr/date/800/").unwrap();
        assert_eq!(d.time(), "+800-00-00T00:00:00Z");
        assert_eq!(d.precision(), 9);
    }

    #[test]
    fn test_day_31_accepted_for_precision_11() {
        // Day 31 is at the boundary of !(1..=31)
        let d = Date::from_str("2000-01-31T00:00:00Z/11").unwrap();
        assert_eq!(d.precision(), 11);
        assert!(d.time().contains("-31T"));
    }

    #[test]
    fn test_month_12_accepted() {
        let d = Date::from_str("1999-12").unwrap();
        assert_eq!(d.time(), "+1999-12-00T00:00:00Z");
        assert_eq!(d.precision(), 10);
    }
    #[test]
    fn test_all_date_patterns_compile() {
        // The patterns are skipped rather than panicked on if they fail to
        // compile, so assert the expected count here: a broken literal must fail
        // the build instead of silently disabling a date format.
        assert_eq!(DATES.len(), DATE_PATTERN_COUNT);
    }
}
