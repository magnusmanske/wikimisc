//! Useful functions to handle dates.

use regex::Regex;

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
    /// Parses a date from a string. Returns None if the string is not a recognized or valid date.
    pub fn from_str(s: &str) -> Option<Self> {
        let (time, precision) = DATES
            .iter()
            .filter_map(|e| {
                let replaced = e.0.replace_all(s, &e.1);
                if replaced == s {
                    None
                } else {
                    Some((replaced.to_string(), e.2))
                }
            })
            .next()?;
        let parts = time.split('-').collect::<Vec<&str>>();
        let year = parts.get(0)?.parse::<i16>().ok()?;
        if year>=2025 {
            return None;
        }
        let month = parts.get(1)?.parse::<u8>().ok()?;
        if month>12 || (month==0 && precision>=11) {
            return None;
        }
        let parts = parts.get(2)?.split('T').collect::<Vec<&str>>();
        let day = parts.get(0)?.parse::<u8>().ok()?;
        if precision >=11 && ( day<1 || day>31) {
            return None;
        }
        Some(Self{time, precision})
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date() {
        assert_eq!(Date::from_str("1234").unwrap().time, "+1234-00-00T00:00:00Z");
        assert_eq!(Date::from_str("1234-05").unwrap().time, "+1234-05-00T00:00:00Z");
        assert!(Date::from_str("1234-15").is_none());
        assert_eq!(Date::from_str("1234-05-17").unwrap().time, "+1234-05-17T00:00:00Z");
        assert_eq!(Date::from_str("https://data.bnf.fr/date/1234/").unwrap().time, "+1234-00-00T00:00:00Z");
        assert_eq!(Date::from_str("https://data.bnf.fr/date/1234").unwrap().time, "+1234-00-00T00:00:00Z");
        assert_eq!(Date::from_str("https://data.bnf.fr/date/1234/").unwrap().precision, 9);
        assert_eq!(Date::from_str("https://data.bnf.fr/date/1234").unwrap().precision, 9);
        assert_eq!(Date::from_str("1234-05-17").unwrap().precision, 11);
        assert_eq!(Date::from_str("1234-05").unwrap().precision, 10);
        assert_eq!(Date::from_str("1234").unwrap().precision, 9);
        assert_eq!(Date::from_str("1234-05-17T00:00:00Z/11").unwrap().time, "+1234-05-17T00:00:00Z");
        assert_eq!(Date::from_str("1234-05-17T00:00:00Z/11").unwrap().precision, 11);
        assert_eq!(Date::from_str("1234-05-00T00:00:00Z/10").unwrap().time, "+1234-05-00T00:00:00Z");
        assert_eq!(Date::from_str("1234-05-01T00:00:00Z/10").unwrap().precision, 10);
        assert_eq!(Date::from_str("1234-00-01T00:00:00Z/9").unwrap().precision, 9);
        assert_eq!(Date::from_str("1234-01-00T00:00:00Z/9").unwrap().time, "+1234-00-00T00:00:00Z");
    }

    #[test]
    fn test_as_qs() {
        assert_eq!(Date::from_str("1234").unwrap().as_qs(), "+1234-00-00T00:00:00Z/9");
        assert_eq!(Date::from_str("1234-05").unwrap().as_qs(), "+1234-05-00T00:00:00Z/10");
        assert_eq!(Date::from_str("1234-05-17").unwrap().as_qs(), "+1234-05-17T00:00:00Z/11");
    }

    #[test]
    fn test_time() {
        assert_eq!(Date::from_str("1234").unwrap().time(), "+1234-00-00T00:00:00Z");
        assert_eq!(Date::from_str("1234-05").unwrap().time(), "+1234-05-00T00:00:00Z");
        assert_eq!(Date::from_str("1234-05-17").unwrap().time(), "+1234-05-17T00:00:00Z");
    }

    #[test]
    fn test_precision() {
        assert_eq!(Date::from_str("1234").unwrap().precision(), 9);
        assert_eq!(Date::from_str("1234-05").unwrap().precision(), 10);
        assert_eq!(Date::from_str("1234-05-17").unwrap().precision(), 11);
    }
}