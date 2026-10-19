//! Convenience wrapper for talking to Wikidata.
//!
//! [`Wikidata`] holds a configurable user-agent string and request timeout,
//! and constructs `reqwest::Client` / `mediawiki::Api` instances with both
//! applied consistently. New HTTP calls against Wikidata should go through
//! this struct rather than building a `reqwest::Client` directly, so that the
//! project's UA and timeout policy stay uniform. SPARQL queries go through
//! `sparql_client::SparqlClient` (feature `sparql-client`).
//!
//! Also provides [`Wikidata::item2qs`] for converting an `ItemEntity` into a
//! QuickStatements-compatible line list.

use crate::mediawiki::reqwest::{Client, ClientBuilder};
use std::time::Duration;
use thiserror::Error;
use wikibase::mediawiki::media_wiki_error::MediaWikiError;
use wikibase::{
    mediawiki::Api, Coordinate, EntityTrait, ItemEntity, QuantityValue, SnakType, TimeValue, Value,
};

/// Failure modes of [`Wikidata`].
#[derive(Debug, Error)]
pub enum WikidataError {
    /// [`Wikidata::item2qs`] was handed an item that already exists on Wikidata.
    /// QuickStatements can only create new items, so the ID must be empty.
    #[error("item2qs: item ID is not empty")]
    ItemIdNotEmpty,

    /// The HTTP client could not be built.
    #[error(transparent)]
    Http(#[from] reqwest::Error),

    /// Constructing the MediaWiki API client failed.
    #[error(transparent)]
    MediaWiki(#[from] MediaWikiError),
}

const WIKIDATA_USER_AGENT: &str = "wikimisc-wikidata/0.1.0";
const WIKIDATA_SPARQL_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_API_URL: &str = "https://www.wikidata.org/w/api.php";

#[derive(Debug, Clone)]
pub struct Wikidata {
    user_agent: String,
    timeout: Duration,
    api_url: String,
}

impl Default for Wikidata {
    fn default() -> Self {
        Self::new()
    }
}

impl Wikidata {
    pub fn new() -> Self {
        Wikidata {
            user_agent: WIKIDATA_USER_AGENT.to_string(),
            timeout: WIKIDATA_SPARQL_TIMEOUT,
            api_url: DEFAULT_API_URL.to_string(),
        }
    }

    pub async fn api(&self) -> Result<Api, WikidataError> {
        let api = Api::new_from_builder(&self.api_url, self.client_builder()).await?;
        Ok(api)
    }

    /// Returns a reqwest client with the current user agent and timeout.
    pub fn reqwest_client(&self) -> Result<Client, WikidataError> {
        Ok(self.client_builder().build()?)
    }

    pub fn client_builder(&self) -> ClientBuilder {
        Client::builder()
            .user_agent(&self.user_agent)
            .timeout(self.timeout)
    }

    pub fn set_user_agent(&mut self, user_agent: &str) {
        self.user_agent = user_agent.to_string();
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Overrides the MediaWiki Action API endpoint used by [`Self::api`].
    /// Default: `https://www.wikidata.org/w/api.php`. Primarily a test seam
    /// for pointing at a local mock server.
    pub fn set_api_url(&mut self, api_url: &str) {
        self.api_url = api_url.to_string();
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// Converts a new (ID-less) item into QuickStatements V1 commands: a
    /// `CREATE` line followed by one `LAST` line per term, sitelink and
    /// statement (qualifiers and references on the statement's line).
    ///
    /// The values follow the syntax the QuickStatements V1 parser accepts.
    /// Anything that parser would reject or silently change is left out rather
    /// than emitted: novalue/somevalue snaks, entity schemas, non-Earth or
    /// altitude-carrying coordinates, quantities with a non-Wikidata unit or
    /// bounds that are not a symmetric tolerance, times in a calendar other
    /// than Gregorian or Julian or with a timezone or before/after, and text
    /// containing a tab, line break, `||` or `/*` (column, line, command and
    /// comment separators to QuickStatements). A skipped main value drops the
    /// whole statement; a skipped qualifier or reference snak drops only that
    /// snak. Coordinate precision cannot be expressed: QuickStatements applies
    /// its own.
    pub fn item2qs(item: &ItemEntity) -> Result<Vec<String>, WikidataError> {
        if !item.id().is_empty() {
            return Err(WikidataError::ItemIdNotEmpty);
        }

        let mut ret = vec!["CREATE".to_string()];

        let terms = [
            ('L', item.labels()),
            ('D', item.descriptions()),
            ('A', item.aliases()),
        ];
        for (command, terms) in terms {
            ret.extend(
                terms
                    .iter()
                    .filter_map(|ls| Self::term2qs(command, ls.language(), ls.value())),
            );
        }

        ret.extend(
            item.sitelinks()
                .as_ref()
                .map(|sl| sl.as_slice())
                .unwrap_or(&[])
                .iter()
                .filter_map(|sl| Self::term2qs('S', sl.site(), sl.title())),
        );

        // Add statements
        for statement in item.claims() {
            let main_value = match Self::snak2qs(statement.main_snak()) {
                Some(value) => value,
                None => continue,
            };

            let qualifiers: Vec<String> = statement
                .qualifiers()
                .iter()
                .filter_map(|snak| {
                    let v = Self::snak2qs(snak)?;
                    Some(vec![snak.property().to_string(), v])
                })
                .flatten()
                .collect();

            let references: Vec<String> = statement
                .references()
                .iter()
                .flat_map(|r| {
                    let mut parts: Vec<String> = r
                        .snaks()
                        .iter()
                        .filter_map(|snak| {
                            let v = Self::snak2qs(snak)?;
                            let p = snak.property().replace('P', "S");
                            Some(vec![p, v])
                        })
                        .flatten()
                        .collect();
                    if !parts.is_empty() {
                        parts[0] = format!("!{}", parts[0]); // Start new reference section
                    }
                    parts
                })
                .collect();

            let mut parts = vec![main_value];
            parts.extend(qualifiers);
            parts.extend(references);
            ret.push(format!(
                "LAST\t{}\t{}",
                statement.property(),
                parts.join("\t")
            ));
        }
        Ok(ret)
    }

    /// A label (`L`), description (`D`), alias (`A`) or sitelink (`S`) line.
    fn term2qs(command: char, code: &str, text: &str) -> Option<String> {
        if !is_qs_code(code) || !is_qs_text(text) {
            return None;
        }
        Some(format!("LAST\t{command}{code}\t\"{text}\""))
    }

    fn snak2qs(snak: &wikibase::Snak) -> Option<String> {
        if *snak.snak_type() != SnakType::Value {
            return None;
        }
        match snak.data_value().as_ref()?.value() {
            Value::Coordinate(c) => Self::coordinate2qs(c),
            Value::MonoLingual(m) => (is_qs_code(m.language()) && is_qs_text(m.text()))
                .then(|| format!("{}:\"{}\"", m.language(), m.text())),
            // Bare: a quoted ID is a string to QuickStatements.
            Value::Entity(entity) => is_qs_entity_id(entity.id()).then(|| entity.id().to_string()),
            Value::Quantity(quantity) => Self::quantity2qs(quantity),
            Value::StringValue(s) => is_qs_text(s).then(|| format!("\"{s}\"")),
            Value::Time(time) => Self::time2qs(time),
            _ => None,
        }
    }

    /// `@<lat>/<lon>`. QuickStatements places every coordinate on Earth, with
    /// no altitude and a precision of its own choosing.
    fn coordinate2qs(c: &Coordinate) -> Option<String> {
        let (latitude, longitude) = (*c.latitude(), *c.longitude());
        (c.globe() == GLOBE_EARTH
            && c.altitude().is_none()
            && latitude.is_finite()
            && longitude.is_finite())
        .then(|| format!("@{latitude}/{longitude}"))
    }

    /// `<amount>[~<tolerance>][U<unit item number>]`. QuickStatements knows
    /// only a symmetric tolerance, from which it recomputes both bounds, so
    /// bounds are emitted only when that reproduces them exactly.
    fn quantity2qs(quantity: &QuantityValue) -> Option<String> {
        let amount = *quantity.amount();
        if !amount.is_finite() {
            return None;
        }
        let tolerance = match (*quantity.lower_bound(), *quantity.upper_bound()) {
            (None, None) => String::new(),
            (Some(lower), Some(upper)) => {
                let tolerance = upper - amount;
                let exact = tolerance.is_finite()
                    && tolerance >= 0.0
                    && amount - tolerance == lower
                    && amount + tolerance == upper;
                if !exact {
                    return None;
                }
                format!("~{tolerance}")
            }
            _ => return None,
        };
        let unit = match quantity.unit() {
            UNITLESS => String::new(),
            unit => {
                let number = unit.strip_prefix(ENTITY_URI_ITEM_PREFIX)?;
                if !is_number(number) {
                    return None;
                }
                format!("U{number}")
            }
        };
        Some(format!("{amount}{tolerance}{unit}"))
    }

    /// `<time>/<precision>[/J]`. QuickStatements sets timezone, before and
    /// after to 0 and knows only the Gregorian and (`/J`) Julian calendars.
    fn time2qs(time: &TimeValue) -> Option<String> {
        if *time.timezone() != 0 || *time.before() != 0 || *time.after() != 0 {
            return None;
        }
        if !is_qs_time(time.time()) {
            return None;
        }
        let calendar = match time.calendarmodel() {
            CALENDAR_GREGORIAN => "",
            CALENDAR_JULIAN => "/J",
            _ => return None,
        };
        Some(format!("{}/{}{calendar}", time.time(), time.precision()))
    }
}

const ENTITY_URI_ITEM_PREFIX: &str = "http://www.wikidata.org/entity/Q";
const GLOBE_EARTH: &str = "http://www.wikidata.org/entity/Q2";
const CALENDAR_GREGORIAN: &str = "http://www.wikidata.org/entity/Q1985727";
const CALENDAR_JULIAN: &str = "http://www.wikidata.org/entity/Q1985786";
const UNITLESS: &str = "1";

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Text QuickStatements reads back unchanged inside `"…"`. It needs no
/// escaping (a value runs to the last quote), but a tab or line break splits
/// the command, `||` is a command separator, and `/* … */` a comment.
fn is_qs_text(text: &str) -> bool {
    !text.contains(['\t', '\n', '\r']) && !text.contains("||") && !text.contains("/*")
}

/// A language code or site ID, as the QuickStatements parsers match it.
fn is_qs_code(code: &str) -> bool {
    code.starts_with(|c: char| c.is_ascii_lowercase())
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
}

/// An ID QuickStatements reads as an entity: `P`/`Q`/`L`/`M` + number, or a
/// lexeme form/sense such as `L1-F2`.
fn is_qs_entity_id(id: &str) -> bool {
    match id.split_once('-') {
        Some((lexeme, sub)) => {
            lexeme.strip_prefix('L').is_some_and(is_number)
                && sub.strip_prefix(['F', 'S']).is_some_and(is_number)
        }
        None => id.strip_prefix(['P', 'Q', 'L', 'M']).is_some_and(is_number),
    }
}

/// A Wikibase timestamp, `[+-]YYYY…-MM-DDThh:mm:ssZ`.
fn is_qs_time(time: &str) -> bool {
    let two_digits = |s: &&str| s.len() == 2 && is_number(s);
    let unsigned = time.strip_prefix(['+', '-']).unwrap_or(time);
    let Some((date, clock)) = unsigned.split_once('T') else {
        return false;
    };
    let Some(clock) = clock.strip_suffix('Z') else {
        return false;
    };
    let date: Vec<&str> = date.split('-').collect();
    let clock: Vec<&str> = clock.split(':').collect();
    matches!(date.as_slice(), [year, month, day] if is_number(year) && two_digits(month) && two_digits(day))
        && clock.len() == 3
        && clock.iter().all(two_digits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wikibase::*;

    #[test]
    fn test_item2qs_rejects_item_with_id() {
        let mut item = ItemEntity::new_empty();
        item.set_id("Q123".to_string());
        assert!(Wikidata::item2qs(&item).is_err());
    }

    #[test]
    fn test_item2qs_empty_item() {
        let item = ItemEntity::new_empty();
        let qs = Wikidata::item2qs(&item).unwrap();
        assert_eq!(qs, vec!["CREATE"]);
    }

    #[test]
    fn test_item2qs_labels_descriptions_aliases() {
        let mut item = ItemEntity::new_empty();
        item.labels_mut().push(LocaleString::new("en", "Test item"));
        item.labels_mut()
            .push(LocaleString::new("de", "Testelement"));
        item.descriptions_mut()
            .push(LocaleString::new("en", "a test"));
        item.aliases_mut().push(LocaleString::new("en", "alias1"));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"CREATE".to_string()));
        assert!(qs.contains(&"LAST\tLen\t\"Test item\"".to_string()));
        assert!(qs.contains(&"LAST\tLde\t\"Testelement\"".to_string()));
        assert!(qs.contains(&"LAST\tDen\t\"a test\"".to_string()));
        assert!(qs.contains(&"LAST\tAen\t\"alias1\"".to_string()));
    }

    #[test]
    fn test_item2qs_sitelinks() {
        let mut item = ItemEntity::new_empty();
        item.sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Test item", vec![]));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tSenwiki\t\"Test item\"".to_string()));
    }

    #[test]
    fn test_item2qs_string_statement_no_qualifiers_no_refs() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello world"),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tP1476\t\"hello world\"".to_string()));
    }

    #[test]
    fn test_item2qs_item_statement() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_item("P31", "Q5"),
            vec![],
            vec![],
        ));

        // A quoted ID would be read as a string, failing every item statement.
        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tP31\tQ5".to_string()), "got: {qs:?}");
    }

    /// The single statement line `item2qs` emits for `snak` as a main value,
    /// or `None` when it skips the statement.
    fn qs_line(snak: Snak) -> Option<String> {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(snak, vec![], vec![]));
        let mut qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.len() <= 2, "got: {qs:?}");
        qs.pop().filter(|line| line != "CREATE")
    }

    fn value_snak(datatype: SnakDataType, value: Value) -> Snak {
        let value_type = match &value {
            Value::Quantity(_) => DataValueType::Quantity,
            Value::Time(_) => DataValueType::Time,
            Value::Coordinate(_) => DataValueType::GlobeCoordinate,
            Value::MonoLingual(_) => DataValueType::MonoLingualText,
            Value::Entity(_) | Value::EntitySchema(_) => DataValueType::EntityId,
            Value::StringValue(_) => DataValueType::StringType,
        };
        Snak::new(
            datatype,
            "P1",
            SnakType::Value,
            Some(DataValue::new(value_type, value)),
        )
    }

    fn quantity(amount: f64, bounds: Option<(f64, f64)>, unit: &str) -> Option<String> {
        let (lower, upper) = bounds.unzip();
        let value = Value::Quantity(QuantityValue::new(amount, lower, unit, upper));
        qs_line(value_snak(SnakDataType::Quantity, value))
    }

    fn time(time: &str, calendar: &str, timezone: u64) -> Option<String> {
        let value = Value::Time(TimeValue::new(0, 0, calendar, 11, time, timezone));
        qs_line(value_snak(SnakDataType::Time, value))
    }

    const GREGORIAN: &str = "http://www.wikidata.org/entity/Q1985727";

    #[test]
    fn test_item2qs_entity_values_are_bare() {
        for (entity_type, id) in [
            (EntityType::Property, "P31"),
            (EntityType::Lexeme, "L7"),
            (EntityType::Lexeme, "L7-F2"),
            (EntityType::Lexeme, "L7-S1"),
            (EntityType::MediaInfo, "M5"),
        ] {
            let value = Value::Entity(EntityValue::new(entity_type, id));
            assert_eq!(
                qs_line(value_snak(SnakDataType::WikibaseItem, value)),
                Some(format!("LAST\tP1\t{id}"))
            );
        }
        let malformed = Value::Entity(EntityValue::new(EntityType::Item, "Q5 "));
        assert_eq!(
            qs_line(value_snak(SnakDataType::WikibaseItem, malformed)),
            None
        );
    }

    #[test]
    fn test_item2qs_quantity_unit() {
        assert_eq!(
            quantity(-1.5, None, "http://www.wikidata.org/entity/Q11573"),
            Some("LAST\tP1\t-1.5U11573".to_string())
        );
        assert_eq!(quantity(42.0, None, "1"), Some("LAST\tP1\t42".to_string()));
        // A unit QuickStatements cannot name must not be dropped silently.
        assert_eq!(quantity(1.0, None, "http://example.org/Q1"), None);
        assert_eq!(
            quantity(1.0, None, "http://www.wikidata.org/entity/P1"),
            None
        );
    }

    #[test]
    fn test_item2qs_quantity_bounds() {
        assert_eq!(
            quantity(
                5.0,
                Some((4.5, 5.5)),
                "http://www.wikidata.org/entity/Q11570"
            ),
            Some("LAST\tP1\t5~0.5U11570".to_string())
        );
        assert_eq!(
            quantity(5.0, Some((5.0, 5.0)), "1"),
            Some("LAST\tP1\t5~0".to_string())
        );
        // Asymmetric bounds have no QuickStatements syntax.
        assert_eq!(quantity(5.0, Some((4.0, 5.5)), "1"), None);
        // Neither does a tolerance that does not reproduce the bounds exactly.
        assert_eq!(quantity(1.1, Some((1.0, 1.2)), "1"), None);
        assert_eq!(quantity(f64::NAN, None, "1"), None);
    }

    #[test]
    fn test_item2qs_time_calendars() {
        assert_eq!(
            time(
                "+1582-10-04T00:00:00Z",
                "http://www.wikidata.org/entity/Q1985786",
                0
            ),
            Some("LAST\tP1\t+1582-10-04T00:00:00Z/11/J".to_string())
        );
        assert_eq!(
            time("-0500-01-01T00:00:00Z", GREGORIAN, 0),
            Some("LAST\tP1\t-0500-01-01T00:00:00Z/11".to_string())
        );
        // Another calendar, a timezone, or a malformed timestamp would be
        // read differently.
        assert_eq!(
            time("+1582-10-04T00:00:00Z", "http://example.org/calendar", 0),
            None
        );
        assert_eq!(time("+1582-10-04T00:00:00Z", GREGORIAN, 60), None);
        assert_eq!(time("+1582-10-04", GREGORIAN, 0), None);
    }

    #[test]
    fn test_item2qs_coordinate_globe_and_altitude() {
        let coordinate = |globe: &str, altitude| {
            let value = Value::Coordinate(Coordinate::new(
                altitude,
                globe.to_string(),
                -12.5,
                0.000001,
                Some(0.1),
            ));
            qs_line(value_snak(SnakDataType::GlobeCoordinate, value))
        };
        assert_eq!(
            coordinate("http://www.wikidata.org/entity/Q2", None),
            Some("LAST\tP1\t@-12.5/0.000001".to_string())
        );
        // QuickStatements would put a Mars coordinate on Earth.
        assert_eq!(
            coordinate("http://www.wikidata.org/entity/Q111", None),
            None
        );
        assert_eq!(
            coordinate("http://www.wikidata.org/entity/Q2", Some(10.0)),
            None
        );
    }

    #[test]
    fn test_item2qs_text_needs_no_escaping_but_separators_are_skipped() {
        let string = |s: &str| qs_line(Snak::new_string("P1", s));
        assert_eq!(
            string("say \"hi\""),
            Some("LAST\tP1\t\"say \"hi\"\"".to_string())
        );
        for unsafe_text in ["a\tb", "a\nb", "a\rb", "a||b", "a /* b */"] {
            assert_eq!(string(unsafe_text), None, "{unsafe_text:?}");
        }

        let monolingual = |language: &str, text: &str| {
            let value = Value::MonoLingual(MonoLingualText::new(text, language));
            qs_line(value_snak(SnakDataType::MonolingualText, value))
        };
        assert_eq!(
            monolingual("be-tarask", "a \"b\""),
            Some("LAST\tP1\tbe-tarask:\"a \"b\"\"".to_string())
        );
        assert_eq!(monolingual("en", "a\tb"), None);
        assert_eq!(monolingual("EN", "a"), None);
    }

    #[test]
    fn test_item2qs_skips_terms_quickstatements_would_misread() {
        let mut item = ItemEntity::new_empty();
        item.labels_mut().push(LocaleString::new("en", "a\tb"));
        item.descriptions_mut()
            .push(LocaleString::new("de", "fine \"quoted\""));
        item.aliases_mut().push(LocaleString::new("en", "a||b"));
        item.sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Line\nbreak", vec![]));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert_eq!(
            qs,
            vec![
                "CREATE".to_string(),
                "LAST\tDde\t\"fine \"quoted\"\"".to_string()
            ]
        );
    }

    #[test]
    fn test_item2qs_statement_with_qualifier() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![Snak::new_item("P407", "Q1860")],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Property, value, qualifier property, (bare) qualifier value
        assert!(
            qs.contains(&"LAST\tP1476\t\"hello\"\tP407\tQ1860".to_string()),
            "got: {qs:?}"
        );
    }

    #[test]
    fn test_item2qs_statement_with_reference() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![Reference::new(vec![Snak::new_url(
                "P854",
                "http://example.com",
            )])],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Reference property becomes S854, prefixed with ! to start new ref section
        assert!(qs
            .iter()
            .any(|s| s.contains("!S854") && s.contains("http://example.com")));
    }

    #[test]
    fn test_item2qs_statement_with_multiple_references() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![
                Reference::new(vec![Snak::new_url("P854", "http://first.com")]),
                Reference::new(vec![Snak::new_url("P854", "http://second.com")]),
            ],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        let line = qs.iter().find(|s| s.starts_with("LAST\tP1476")).unwrap();
        // Both references start with !S854
        assert_eq!(line.matches("!S854").count(), 2);
    }

    #[test]
    fn test_item2qs_skips_novalue_snak() {
        let mut item = ItemEntity::new_empty();
        // A novalue snak — snak2qs should return None, so this claim is skipped
        item.add_claim(Statement::new_normal(
            Snak::new(SnakDataType::WikibaseItem, "P31", SnakType::NoValue, None),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Only "CREATE" — the novalue statement must be skipped
        assert!(!qs.iter().any(|s| s.contains("P31")));
    }

    #[test]
    fn test_item2qs_quantity() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::Quantity,
                "P1082",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Quantity,
                    Value::Quantity(QuantityValue::new(42.0, None, "1", None)),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.iter().any(|s| s.contains("P1082") && s.contains("42")));
    }

    #[test]
    fn test_item2qs_monolingual() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::MonolingualText,
                "P1705",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::MonoLingualText,
                    Value::MonoLingual(MonoLingualText::new("Original title", "en")),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P1705") && s.contains("en:\"Original title\"")));
    }

    #[test]
    fn test_item2qs_coordinate() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::GlobeCoordinate,
                "P625",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::GlobeCoordinate,
                    Value::Coordinate(Coordinate::new(
                        None,
                        "http://www.wikidata.org/entity/Q2".to_string(),
                        51.5,
                        -0.1,
                        None,
                    )),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P625") && s.contains("@51.5/-0.1")));
    }

    #[test]
    fn test_item2qs_time_value() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::Time,
                "P569",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Time,
                    Value::Time(TimeValue::new(
                        0,
                        0,
                        "http://www.wikidata.org/entity/Q1985727",
                        11,
                        "+1990-05-17T00:00:00Z",
                        0,
                    )),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P569") && s.contains("+1990-05-17T00:00:00Z/11")));
    }

    #[test]
    fn test_set_user_agent() {
        let mut wd = Wikidata::new();
        wd.set_user_agent("my-agent/1.0");
        // Verify it builds without error (can't easily inspect the built client's UA)
        assert!(wd.reqwest_client().is_ok());
    }

    #[test]
    fn test_set_timeout() {
        let mut wd = Wikidata::new();
        wd.set_timeout(std::time::Duration::from_secs(30));
        assert!(wd.reqwest_client().is_ok());
    }

    #[test]
    fn test_item2qs_novalue_qualifier_is_skipped() {
        // A qualifier whose snak_type is NoValue has no concrete value and must be
        // silently skipped by snak2qs / the qualifier filter_map.
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "title"),
            vec![Snak::new(
                SnakDataType::WikibaseItem,
                "P31",
                SnakType::NoValue,
                None,
            )],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        let line = qs.iter().find(|s| s.contains("P1476")).unwrap();
        // The NoValue qualifier must not appear in the output.
        assert!(
            !line.contains("P31"),
            "NoValue qualifier must be omitted from QS output, got: {line}"
        );
    }

    #[test]
    fn test_item2qs_novalue_reference_snak_is_skipped() {
        // A reference snak with NoValue produces no value string and must be
        // omitted from the QS reference section.
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "title"),
            vec![],
            vec![Reference::new(vec![
                Snak::new_url("P854", "http://example.com"), // valid — must appear
                Snak::new(SnakDataType::WikibaseItem, "P248", SnakType::NoValue, None), // must be skipped
            ])],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        let line = qs.iter().find(|s| s.contains("P1476")).unwrap();
        // The valid URL reference must be present.
        assert!(
            line.contains("S854") && line.contains("http://example.com"),
            "URL reference snak must be present, got: {line}"
        );
        // The NoValue reference snak must not appear.
        assert!(
            !line.contains("S248"),
            "NoValue reference snak must be omitted, got: {line}"
        );
    }

    #[test]
    fn test_item2qs_multiple_aliases() {
        // All aliases must each get their own LAST\tA… line.
        let mut item = ItemEntity::new_empty();
        item.aliases_mut().push(LocaleString::new("en", "first"));
        item.aliases_mut().push(LocaleString::new("en", "second"));
        item.aliases_mut().push(LocaleString::new("de", "erstes"));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tAen\t\"first\"".to_string()));
        assert!(qs.contains(&"LAST\tAen\t\"second\"".to_string()));
        assert!(qs.contains(&"LAST\tAde\t\"erstes\"".to_string()));
    }

    #[test]
    fn test_wikidata_new_builds_client() {
        // Wikidata::new() must produce a working instance whose client builder
        // compiles and builds without error.
        let wd = Wikidata::new();
        assert!(wd.reqwest_client().is_ok());
    }

    #[test]
    fn test_default_matches_new() {
        let wd = Wikidata::default();
        assert_eq!(wd.api_url(), Wikidata::new().api_url());
    }

    #[test]
    fn test_url_getters_reflect_the_defaults_and_overrides() {
        let mut wd = Wikidata::new();
        assert_eq!(wd.api_url(), DEFAULT_API_URL);

        wd.set_api_url("http://localhost:1234/w/api.php");
        assert_eq!(wd.api_url(), "http://localhost:1234/w/api.php");
    }

    #[test]
    fn test_item2qs_emits_coordinate_values() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::WikibaseItem,
                "P31",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::GlobeCoordinate,
                    wikibase::Value::Coordinate(wikibase::Coordinate::new(
                        None,
                        "http://www.wikidata.org/entity/Q2".to_string(),
                        1.0,
                        2.0,
                        Some(0.1),
                    )),
                )),
            ),
            vec![],
            vec![],
        ));
        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.iter().any(|l| l.contains("@1/2")), "got: {qs:?}");
    }

    #[test]
    fn test_item2qs_skips_unsupported_value_type() {
        // `EntitySchema` is the one Value variant snak2qs has no QuickStatements
        // representation for, so such a claim must be skipped rather than
        // emitted malformed.
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::WikibaseItem,
                "P31",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::EntityId,
                    wikibase::Value::EntitySchema(EntityValue::new(
                        EntityType::EntitySchema,
                        "E123",
                    )),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert_eq!(qs, vec!["CREATE".to_string()], "got: {qs:?}");
    }
}
