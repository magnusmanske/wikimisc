//! Wikibase dates: a time string such as `+1952-03-11T00:00:00Z`, a precision
//! (9 = year, 10 = month, 11 = day, coarser down to 0 = billion years) and a
//! calendar model.
//!
//! There are several ways to get a [`Date`]:
//!
//! - [`Date::from_str`] reads the loose formats found in external data
//!   (`1952`, `1952-03`, `1952-03-11`, BnF date URLs, QuickStatements-style
//!   `+1952-03-11T00:00:00Z/11`), with the restrictions below;
//! - [`Date::new`] / [`Date::new_in`] build one from a year and an optional
//!   month and day;
//! - [`Date::from_wikibase`] reads a time string and precision as Wikibase
//!   stores them, BCE and years beyond four digits included;
//! - [`Date::from_qs`] reads a QuickStatements time value
//!   (`+1952-03-11T00:00:00Z/11/J`);
//! - [`Date::today`].
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

/// Failure modes of the [`Date`] parsers and constructors.
#[derive(Debug, Error)]
#[non_exhaustive]
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

    /// The components do not form a date: year 0, month 13, 30 February, a
    /// day without a month, or a precision this module cannot represent.
    #[error("'{0}' is not a valid date")]
    Invalid(String),
}

/// The concept URI of the proleptic Gregorian calendar model.
pub const GREGORIAN_URI: &str = "http://www.wikidata.org/entity/Q1985727";

/// The concept URI of the proleptic Julian calendar model.
pub const JULIAN_URI: &str = "http://www.wikidata.org/entity/Q1985786";

/// The calendar model of a [`Date`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Calendar {
    #[default]
    Gregorian,
    Julian,
}

impl Calendar {
    /// The concept URI Wikibase uses as `calendarmodel`.
    pub const fn uri(self) -> &'static str {
        match self {
            Self::Gregorian => GREGORIAN_URI,
            Self::Julian => JULIAN_URI,
        }
    }

    /// The number of the calendar's Wikidata item (`1985727` for Q1985727).
    pub const fn item_number(self) -> u64 {
        match self {
            Self::Gregorian => 1_985_727,
            Self::Julian => 1_985_786,
        }
    }

    /// The calendar with the given concept URI (`http` or `https`).
    pub fn from_uri(uri: &str) -> Option<Self> {
        let uri = uri.replacen("https://", "http://", 1);
        [Self::Gregorian, Self::Julian]
            .into_iter()
            .find(|calendar| calendar.uri() == uri)
    }

    /// Whether `year` (Wikibase numbering: no year 0, `-1` is 1 BCE) is a
    /// leap year in this calendar.
    pub const fn is_leap_year(self, year: i64) -> bool {
        // 1 BCE is astronomical year 0; the leap-year rules count that way.
        let year = if year < 0 { year + 1 } else { year };
        match self {
            Self::Gregorian => year % 4 == 0 && (year % 100 != 0 || year % 400 == 0),
            Self::Julian => year % 4 == 0,
        }
    }

    /// The number of days of `month` (1–12) in `year`; 0 for another month.
    pub const fn days_in_month(self, year: i64, month: u8) -> u8 {
        match month {
            2 if self.is_leap_year(year) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            1..=12 => 31,
            _ => 0,
        }
    }
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

/// Year precision.
pub const PRECISION_YEAR: u64 = 9;
/// Month precision.
pub const PRECISION_MONTH: u64 = 10;
/// Day precision.
pub const PRECISION_DAY: u64 = 11;

/// A Wikibase date: year, optional month and day, precision and calendar.
///
/// Times of day are not represented; precisions finer than a day are read as
/// day precision. Two dates are equal when their components, precision and
/// calendar are equal, whatever the spelling of their time strings.
#[derive(Debug, Clone)]
pub struct Date {
    year: i64,
    /// 0 when unknown or below the precision.
    month: u8,
    /// 0 when unknown or below the precision.
    day: u8,
    precision: u64,
    calendar: Calendar,
    time: String,
}

impl PartialEq for Date {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for Date {}

impl std::hash::Hash for Date {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl Date {
    /// A date of year, month or day precision, depending on which parts are
    /// given, in the Gregorian calendar.
    ///
    /// Fails on year 0, a month outside 1–12, a day that does not exist in
    /// its month, or a day without a month. Years before the common era are
    /// negative (`-500` is 500 BCE).
    pub fn new(year: i64, month: Option<u8>, day: Option<u8>) -> Result<Self, DateError> {
        Self::new_in(Calendar::Gregorian, year, month, day)
    }

    /// As [`Self::new`], in the given calendar (which decides whether
    /// 29 February exists).
    pub fn new_in(
        calendar: Calendar,
        year: i64,
        month: Option<u8>,
        day: Option<u8>,
    ) -> Result<Self, DateError> {
        let invalid = || DateError::Invalid(describe_parts(year, month, day));
        let precision = match (month, day) {
            (None, None) => PRECISION_YEAR,
            (Some(_), None) => PRECISION_MONTH,
            (Some(_), Some(_)) => PRECISION_DAY,
            (None, Some(_)) => return Err(invalid()),
        };
        let month = month.unwrap_or(0);
        let day = day.unwrap_or(0);
        if !parts_are_valid(calendar, year, month, day, precision) {
            return Err(invalid());
        }
        Ok(Self::from_parts(year, month, day, precision, calendar))
    }

    /// Reads a time string and precision as Wikibase stores them
    /// (`+1952-03-11T00:00:00Z`, `-0500-00-00T00:00:00Z`,
    /// `+13798000000-00-00T00:00:00Z`).
    ///
    /// The sign is optional, leading zeros of the year are ignored and the
    /// time of day is not looked at. Parts below the precision are dropped
    /// (`+1952-03-11T00:00:00Z` at precision 9 is the year 1952); parts the
    /// precision needs must be valid. Precisions finer than a day (12–14)
    /// become day precision. The calendar is Gregorian; see
    /// [`Self::with_calendar`].
    pub fn from_wikibase(time: &str, precision: u64) -> Result<Self, DateError> {
        let unparsable = || DateError::Unparsable(format!("{time}/{precision}"));
        if precision > 14 {
            return Err(unparsable());
        }
        let precision = precision.min(PRECISION_DAY);
        let (year, month, day) = split_wikibase_time(time).ok_or_else(unparsable)?;
        let month = if precision >= PRECISION_MONTH {
            month
        } else {
            0
        };
        let day = if precision >= PRECISION_DAY { day } else { 0 };
        // The calendar is not known here, so 29 February passes in any year
        // that is a leap year in either calendar.
        if !parts_are_valid(Calendar::Julian, year, month, day, precision) {
            return Err(unparsable());
        }
        Ok(Self::from_parts(
            year,
            month,
            day,
            precision,
            Calendar::Gregorian,
        ))
    }

    /// Reads a QuickStatements time value: `[+-]YYYY[-MM[-DD[Thh:mm:ssZ]]]`,
    /// optionally followed by `/precision` (default 9) and `/J` for the
    /// Julian calendar.
    ///
    /// As QuickStatements does, a precision finer than a day becomes day
    /// precision, and a `00` day or month lowers the precision to what is
    /// known (`+1952-03-00T00:00:00Z/11` is March 1952). Unlike
    /// QuickStatements, parts below the precision are dropped from the time
    /// string, and the year is written with at least four digits.
    pub fn from_qs(s: &str) -> Result<Self, DateError> {
        let unparsable = || DateError::Unparsable(s.to_string());
        let (value, calendar) = match s.strip_suffix("/J") {
            Some(value) => (value, Calendar::Julian),
            None => (s, Calendar::Gregorian),
        };
        let (value, precision) = match value.rsplit_once('/') {
            Some((value, precision)) if is_digits(precision) => {
                (value, precision.parse::<u64>().map_err(|_| unparsable())?)
            }
            Some(_) => return Err(unparsable()),
            None => (value, PRECISION_YEAR),
        };
        let (negative, unsigned) = split_sign(value);
        let mut fields = unsigned
            .trim_end_matches('Z')
            .split(['-', 'T', ':'])
            .map(|field| {
                is_digits(field)
                    .then(|| field.parse::<u64>().ok())
                    .flatten()
            });
        let year = fields.next().flatten().ok_or_else(unparsable)?;
        let mut next_or = |default: u64| fields.next().unwrap_or(Some(default));
        let month = next_or(1).ok_or_else(unparsable)?;
        let day = next_or(1).ok_or_else(unparsable)?;
        // Hours, minutes and seconds must be numbers but are not used.
        for _ in 0..3 {
            next_or(0).ok_or_else(unparsable)?;
        }
        if fields.next().is_some() {
            return Err(unparsable());
        }

        let mut precision = precision.min(PRECISION_DAY);
        if day == 0 && precision >= PRECISION_DAY {
            precision = PRECISION_MONTH;
        }
        if month == 0 && precision >= PRECISION_MONTH {
            precision = PRECISION_YEAR;
        }
        let year = i64::try_from(year).map_err(|_| unparsable())?;
        let year = if negative { -year } else { year };
        let month = u8::try_from(month).map_err(|_| unparsable())?;
        let day = u8::try_from(day).map_err(|_| unparsable())?;
        let month = if precision >= PRECISION_MONTH {
            month
        } else {
            0
        };
        let day = if precision >= PRECISION_DAY { day } else { 0 };
        if !parts_are_valid(calendar, year, month, day, precision) {
            return Err(unparsable());
        }
        Ok(Self::from_parts(year, month, day, precision, calendar))
    }

    /// Today's date (UTC), day precision, Gregorian.
    pub fn today() -> Self {
        let today = chrono::Utc::now().date_naive();
        // chrono's month and day always fit.
        let month = u8::try_from(today.month()).unwrap_or(0);
        let day = u8::try_from(today.day()).unwrap_or(0);
        Self::from_parts(
            i64::from(today.year()),
            month,
            day,
            PRECISION_DAY,
            Calendar::Gregorian,
        )
    }

    /// The same date in another calendar model. The components are kept, not
    /// converted: this says which calendar they were counted in.
    #[must_use]
    pub fn with_calendar(mut self, calendar: Calendar) -> Self {
        self.calendar = calendar;
        self
    }

    /// The same date at another precision. A coarser precision drops parts
    /// (and rewrites the time string); a finer one fails unless the parts it
    /// needs are known.
    pub fn with_precision(self, precision: u64) -> Result<Self, DateError> {
        let invalid = || DateError::Invalid(format!("{}/{precision}", self.time));
        if precision > PRECISION_DAY
            || (precision >= PRECISION_MONTH && self.month == 0)
            || (precision >= PRECISION_DAY && self.day == 0)
        {
            return Err(invalid());
        }
        let month = if precision >= PRECISION_MONTH {
            self.month
        } else {
            0
        };
        let day = if precision >= PRECISION_DAY {
            self.day
        } else {
            0
        };
        Ok(Self::from_parts(
            self.year,
            month,
            day,
            precision,
            self.calendar,
        ))
    }

    /// Returns the date as a QuickStatements-compatible string
    /// (`+1952-03-11T00:00:00Z/11`, with `/J` for the Julian calendar).
    pub fn as_qs(&self) -> String {
        match self.calendar {
            Calendar::Gregorian => format!("{}/{}", self.time, self.precision),
            Calendar::Julian => format!("{}/{}/J", self.time, self.precision),
        }
    }

    /// Returns the date as a wikibase timevalue-compatible string.
    pub fn time(&self) -> &str {
        &self.time
    }

    /// Returns the precision of the date.
    pub fn precision(&self) -> u64 {
        self.precision
    }

    /// The year; negative before the common era (`-500` is 500 BCE).
    pub fn year(&self) -> i64 {
        self.year
    }

    /// The month (1–12), if the precision includes it.
    pub fn month(&self) -> Option<u8> {
        (self.month > 0).then_some(self.month)
    }

    /// The day of the month, if the precision includes it.
    pub fn day(&self) -> Option<u8> {
        (self.day > 0).then_some(self.day)
    }

    /// The calendar model.
    pub fn calendar(&self) -> Calendar {
        self.calendar
    }

    /// The date in English, as precise as it is: `1952-03-11`, `1952-03`,
    /// `1952`, `1950s`, `20th century`, `2nd millennium`. Coarser precisions
    /// fall back to the year. Years are written with at least four digits
    /// and a `-` before the common era (`-0500`, `-6th century`).
    pub fn describe(&self) -> String {
        let year = format_year(self.year, "");
        match self.precision {
            PRECISION_DAY.. => format!("{year}-{:02}-{:02}", self.month, self.day),
            PRECISION_MONTH => format!("{year}-{:02}", self.month),
            8 => format!("{}0s", self.year / 10),
            7 => format!("{} century", ordinal(nth_unit(self.year, 100))),
            6 => format!("{} millennium", ordinal(nth_unit(self.year, 1000))),
            _ => year,
        }
    }

    /// A date from validated parts, with its canonical time string.
    fn from_parts(year: i64, month: u8, day: u8, precision: u64, calendar: Calendar) -> Self {
        let time = format!("{}-{month:02}-{day:02}T00:00:00Z", format_year(year, "+"));
        Self {
            year,
            month,
            day,
            precision,
            calendar,
            time,
        }
    }

    fn key(&self) -> (i64, u8, u8, u64, Calendar) {
        (
            self.year,
            self.month,
            self.day,
            self.precision,
            self.calendar,
        )
    }
}

/// `year` with at least four digits, `-` before the common era and
/// `positive_sign` otherwise.
fn format_year(year: i64, positive_sign: &str) -> String {
    let sign = if year < 0 { "-" } else { positive_sign };
    format!("{sign}{:04}", year.unsigned_abs())
}

/// The century (`unit` 100) or millennium (1000) a year falls in, counted
/// from 1; negative before the common era.
fn nth_unit(year: i64, unit: i64) -> i64 {
    if year > 0 {
        (year - 1) / unit + 1
    } else {
        year / unit - 1
    }
}

/// `1st`, `2nd`, `3rd`, `4th`, `11th`, `21st`, `-1st`.
fn ordinal(n: i64) -> String {
    let abs = n.unsigned_abs();
    let suffix = match (abs % 100, abs % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Whether the parts form a date at `precision` (0 marks an absent part).
fn parts_are_valid(calendar: Calendar, year: i64, month: u8, day: u8, precision: u64) -> bool {
    if year == 0 || precision > PRECISION_DAY || month > 12 {
        return false;
    }
    if precision >= PRECISION_MONTH && month == 0 {
        return false;
    }
    if day > 0 && month == 0 {
        return false;
    }
    if precision >= PRECISION_DAY && day == 0 {
        return false;
    }
    day <= calendar.days_in_month(year, month) || month == 0
}

fn describe_parts(year: i64, month: Option<u8>, day: Option<u8>) -> String {
    let mut s = year.to_string();
    if let Some(month) = month {
        s.push_str(&format!("-{month:02}"));
    }
    if let Some(day) = day {
        s.push_str(&format!("-{day:02}"));
    }
    s
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Splits off a leading `+` or `-`; `true` for `-`.
fn split_sign(s: &str) -> (bool, &str) {
    match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    }
}

/// Year, month and day of a Wikibase time string; the time of day, if any,
/// is ignored.
fn split_wikibase_time(time: &str) -> Option<(i64, u8, u8)> {
    let (negative, unsigned) = split_sign(time);
    let date = unsigned.split('T').next().unwrap_or(unsigned);
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !is_digits(year) || month.len() != 2 || day.len() != 2 {
        return None;
    }
    if !is_digits(month) || !is_digits(day) {
        return None;
    }
    let year: i64 = year.parse().ok()?;
    Some((
        if negative { -year } else { year },
        month.parse().ok()?,
        day.parse().ok()?,
    ))
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
        // The time string is kept as it always was (a three-digit year stays
        // three digits); the parts follow the precision.
        let keep = |part: u32, needed: u64| {
            if precision >= needed {
                u8::try_from(part).unwrap_or(0)
            } else {
                0
            }
        };
        Ok(Self {
            year: i64::from(year),
            month: keep(month, PRECISION_MONTH),
            day: keep(day, PRECISION_DAY),
            precision,
            calendar: Calendar::Gregorian,
            time,
        })
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

    #[test]
    fn test_from_str_exposes_parts() {
        let d = Date::from_str("1952-03-11").unwrap();
        assert_eq!((d.year(), d.month(), d.day()), (1952, Some(3), Some(11)));
        assert_eq!(d.calendar(), Calendar::Gregorian);
        let d = Date::from_str("1234-05-01T00:00:00Z/10").unwrap();
        assert_eq!((d.year(), d.month(), d.day()), (1234, Some(5), None));
        let d = Date::from_str("800").unwrap();
        assert_eq!((d.year(), d.month(), d.day()), (800, None, None));
    }

    #[test]
    fn test_new() {
        let d = Date::new(1952, Some(3), Some(11)).unwrap();
        assert_eq!(d.as_qs(), "+1952-03-11T00:00:00Z/11");
        let d = Date::new(1952, Some(3), None).unwrap();
        assert_eq!(d.as_qs(), "+1952-03-00T00:00:00Z/10");
        let d = Date::new(800, None, None).unwrap();
        assert_eq!(d.as_qs(), "+0800-00-00T00:00:00Z/9");
        let d = Date::new(-500, None, None).unwrap();
        assert_eq!(d.time(), "-0500-00-00T00:00:00Z");
        assert_eq!(d.year(), -500);
        let d = Date::new(13_798_000_000, None, None).unwrap();
        assert_eq!(d.time(), "+13798000000-00-00T00:00:00Z");
        // Future dates are fine here; only `from_str` refuses them.
        assert!(Date::new(3000, Some(1), Some(1)).is_ok());
    }

    #[test]
    fn test_new_rejects_impossible_dates() {
        assert!(matches!(
            Date::new(0, None, None),
            Err(DateError::Invalid(_))
        ));
        assert!(Date::new(1952, Some(13), None).is_err());
        assert!(Date::new(1952, Some(0), None).is_err());
        assert!(Date::new(1952, None, Some(3)).is_err());
        assert!(Date::new(1952, Some(4), Some(31)).is_err());
        assert!(Date::new(1952, Some(4), Some(0)).is_err());
        assert!(Date::new(1900, Some(2), Some(29)).is_err());
        assert!(Date::new(2000, Some(2), Some(29)).is_ok());
    }

    #[test]
    fn test_julian_leap_years() {
        assert!(Date::new_in(Calendar::Julian, 1900, Some(2), Some(29)).is_ok());
        let d = Date::new_in(Calendar::Julian, 1582, Some(10), Some(4)).unwrap();
        assert_eq!(d.as_qs(), "+1582-10-04T00:00:00Z/11/J");
        // 1 BCE is a leap year (astronomical year 0), 2 BCE is not.
        assert!(Date::new_in(Calendar::Julian, -1, Some(2), Some(29)).is_ok());
        assert!(Date::new_in(Calendar::Julian, -2, Some(2), Some(29)).is_err());
        assert!(Calendar::Gregorian.is_leap_year(-401));
        assert!(!Calendar::Gregorian.is_leap_year(-101));
    }

    #[test]
    fn test_calendar() {
        assert_eq!(Calendar::Julian.uri(), JULIAN_URI);
        assert_eq!(Calendar::Gregorian.item_number(), 1_985_727);
        assert_eq!(Calendar::from_uri(GREGORIAN_URI), Some(Calendar::Gregorian));
        assert_eq!(
            Calendar::from_uri("https://www.wikidata.org/entity/Q1985786"),
            Some(Calendar::Julian)
        );
        assert_eq!(
            Calendar::from_uri("http://www.wikidata.org/entity/Q1"),
            None
        );
        assert_eq!(Calendar::Gregorian.days_in_month(2023, 2), 28);
        assert_eq!(Calendar::Gregorian.days_in_month(2023, 13), 0);
    }

    #[test]
    fn test_from_wikibase() {
        let d = Date::from_wikibase("+1952-03-11T00:00:00Z", 11).unwrap();
        assert_eq!((d.year(), d.month(), d.day()), (1952, Some(3), Some(11)));
        // Parts below the precision are dropped.
        let d = Date::from_wikibase("+1952-01-01T00:00:00Z", 9).unwrap();
        assert_eq!(d.time(), "+1952-00-00T00:00:00Z");
        assert_eq!(d.month(), None);
        // BCE, leading zeros, long years.
        let d = Date::from_wikibase("-0500-00-00T00:00:00Z", 9).unwrap();
        assert_eq!(d.year(), -500);
        let d = Date::from_wikibase("+00000001952-03-00T00:00:00Z", 10).unwrap();
        assert_eq!(d.time(), "+1952-03-00T00:00:00Z");
        let d = Date::from_wikibase("-13798000000-00-00T00:00:00Z", 3).unwrap();
        assert_eq!(d.year(), -13_798_000_000);
        assert_eq!(d.precision(), 3);
        // Finer than a day: day precision; the time of day is ignored.
        let d = Date::from_wikibase("+2001-09-11T12:46:40Z", 14).unwrap();
        assert_eq!(d.precision(), 11);
        assert_eq!(d.time(), "+2001-09-11T00:00:00Z");
        // No sign is fine; QLever-style missing `Z` too.
        assert!(Date::from_wikibase("1952-03-11T00:00:00", 11).is_ok());
        // 29 February 1900 exists in the Julian calendar.
        assert!(Date::from_wikibase("+1900-02-29T00:00:00Z", 11).is_ok());
        for (time, precision) in [
            ("+1952-00-00T00:00:00Z", 10),
            ("+1952-03-00T00:00:00Z", 11),
            ("+1952-02-30T00:00:00Z", 11),
            ("+1952-13-01T00:00:00Z", 10),
            ("+0000-00-00T00:00:00Z", 9),
            ("+1952-03-11T00:00:00Z", 15),
            ("+1952-3-11T00:00:00Z", 11),
            ("1952", 9),
            ("", 9),
            ("+19x2-03-11T00:00:00Z", 11),
        ] {
            assert!(
                Date::from_wikibase(time, precision).is_err(),
                "{time}/{precision}"
            );
        }
    }

    #[test]
    fn test_from_qs() {
        let d = Date::from_qs("+1952-03-11T00:00:00Z/11").unwrap();
        assert_eq!(d.as_qs(), "+1952-03-11T00:00:00Z/11");
        let d = Date::from_qs("+1952-03-11T00:00:00Z/11/J").unwrap();
        assert_eq!(d.calendar(), Calendar::Julian);
        assert_eq!(d.as_qs(), "+1952-03-11T00:00:00Z/11/J");
        // Default precision 9; missing parts default to 1.
        let d = Date::from_qs("+1952").unwrap();
        assert_eq!(d.as_qs(), "+1952-00-00T00:00:00Z/9");
        let d = Date::from_qs("-0500-01-01T00:00:00Z/9").unwrap();
        assert_eq!(d.year(), -500);
        // A 00 day or month lowers the precision.
        let d = Date::from_qs("+1952-03-00T00:00:00Z/11").unwrap();
        assert_eq!(d.as_qs(), "+1952-03-00T00:00:00Z/10");
        let d = Date::from_qs("+1952-00-00T00:00:00Z/11").unwrap();
        assert_eq!(d.as_qs(), "+1952-00-00T00:00:00Z/9");
        // Finer than a day is clamped.
        let d = Date::from_qs("+1952-03-11T10:20:30Z/14").unwrap();
        assert_eq!(d.as_qs(), "+1952-03-11T00:00:00Z/11");
        // Leading zeros are dropped.
        assert_eq!(
            Date::from_qs("+00001952-03-11T00:00:00Z/11")
                .unwrap()
                .time(),
            "+1952-03-11T00:00:00Z"
        );
        for bad in [
            "",
            "abc",
            "+1952-13-01T00:00:00Z/11",
            "+1952-02-30T00:00:00Z/11",
            "+1952-03-11T00:00:00Z/x",
            "+1952-03-11Tab:00:00Z/11",
            "+1952-03-11T00:00:00:00Z/11",
        ] {
            assert!(Date::from_qs(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_today() {
        let d = Date::today();
        assert_eq!(d.precision(), 11);
        assert_eq!(d.year(), i64::from(chrono::Utc::now().year()));
        assert_eq!(
            d.time(),
            chrono::Utc::now().format("+%Y-%m-%dT00:00:00Z").to_string()
        );
    }

    #[test]
    fn test_with_precision_and_calendar() {
        let d = Date::new(1952, Some(3), Some(11)).unwrap();
        let year = d.clone().with_precision(9).unwrap();
        assert_eq!(year.time(), "+1952-00-00T00:00:00Z");
        assert!(year.clone().with_precision(10).is_err());
        assert!(d.clone().with_precision(12).is_err());
        let decade = d.clone().with_precision(8).unwrap();
        assert_eq!(decade.describe(), "1950s");
        let julian = d.clone().with_calendar(Calendar::Julian);
        assert_ne!(julian, d);
        assert_eq!(julian.with_calendar(Calendar::Gregorian), d);
    }

    #[test]
    fn test_equality_ignores_spelling() {
        assert_eq!(
            Date::from_str("800").unwrap(),
            Date::new(800, None, None).unwrap()
        );
        assert_ne!(
            Date::new(800, None, None).unwrap(),
            Date::new(800, Some(1), None).unwrap()
        );
    }

    #[test]
    fn test_describe() {
        let at = |time: &str, precision| Date::from_wikibase(time, precision).unwrap().describe();
        assert_eq!(at("+1952-03-11T00:00:00Z", 11), "1952-03-11");
        assert_eq!(at("+1952-03-11T00:00:00Z", 10), "1952-03");
        assert_eq!(at("+1952-03-11T00:00:00Z", 9), "1952");
        assert_eq!(at("+0800-00-00T00:00:00Z", 9), "0800");
        assert_eq!(at("+1995-00-00T00:00:00Z", 8), "1990s");
        assert_eq!(at("+1901-00-00T00:00:00Z", 7), "20th century");
        assert_eq!(at("+1900-00-00T00:00:00Z", 7), "19th century");
        assert_eq!(at("+2001-00-00T00:00:00Z", 7), "21st century");
        assert_eq!(at("+1201-00-00T00:00:00Z", 7), "13th century");
        assert_eq!(at("+1500-00-00T00:00:00Z", 6), "2nd millennium");
        assert_eq!(at("+2001-00-00T00:00:00Z", 6), "3rd millennium");
        assert_eq!(at("-0500-00-00T00:00:00Z", 7), "-6th century");
        assert_eq!(at("-0999-00-00T00:00:00Z", 6), "-1st millennium");
        assert_eq!(at("-0500-00-00T00:00:00Z", 9), "-0500");
        assert_eq!(at("+1952-00-00T00:00:00Z", 4), "1952");
    }
}
