//! Useful functionality for dealing with external identifiers in Wikidata.

use crate::entity_id::{ItemId, PropertyId};
use crate::statements;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::LazyLock;
use wikibase::*;

#[cfg(feature = "wikidata")]
use crate::wikidata::{CirrusQuery, Wikidata};

// Literal patterns, held as `Option<Regex>` so a pattern that somehow failed to
// compile degrades to a `None` return rather than panicking in a library.
// `test_all_static_regexes_compile` makes sure CI catches a broken literal.
static RE_PROPERTY_NUMERIC: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"^\s*[Pp](\d+)\s*$"#).ok());
static RE_FROM_STRING: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"^[Pp](\d+):(.+)$"#).ok());

/// Properties whose values the external site treats case-insensitively.
const CASE_INSENSITIVE_PROPERTIES: &[usize] = &[2002];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash, Default)]
pub struct ExternalId {
    property: usize,
    id: String,
}

impl fmt::Display for ExternalId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "P{}:{}", self.property, self.id)
    }
}

impl ExternalId {
    /// Creates a new ExternalId from a property number and an ID string.
    pub fn new(property: usize, id: &str) -> Self {
        let id = Self::fix_property_value(property, id);
        Self { property, id }
    }

    /// Normalises an ID value to the form Wikidata stores for the property.
    fn fix_property_value(property: usize, id: &str) -> String {
        match property {
            213 => id.replace(' ', ""),                    // ISNI: no spaces
            244 => id.replace("%20", "").replace('+', ""), // LCNAF: URL-encoded spaces
            1207 => id.replace('+', ""),                   // NUKAT: URL-encoded spaces
            1368 => id.strip_prefix("LNC10-").unwrap_or(id).to_string(), // LNB: URI prefix
            8034 => id.replace('_', "/"),                  // VIAF cluster path separator
            _ => id.to_string(),
        }
    }

    /// Whether values of `property` are compared case-insensitively, i.e.
    /// the external site treats `Foo` and `foo` as the same ID: P2002 (X
    /// username).
    pub fn is_case_insensitive(property: usize) -> bool {
        CASE_INSENSITIVE_PROPERTIES.contains(&property)
    }

    /// Returns a new ExternalId from a string like "P123:ABC456DEF".
    pub fn from_string(s: &str) -> Option<Self> {
        let captures = RE_FROM_STRING.as_ref()?.captures(s)?;
        // Group 1 is already `\d+` from the regex, so parse directly without
        // running the second RE_PROPERTY_NUMERIC regex that prop_numeric uses.
        let property = captures.get(1)?.as_str().parse::<usize>().ok()?;
        let id = captures.get(2)?.as_str();
        Some(Self::new(property, id))
    }

    /// Parses a property number from a string like "P123".
    pub fn prop_numeric(prop: &str) -> Option<usize> {
        let normalized = match RE_PROPERTY_NUMERIC.as_ref() {
            Some(re) => re.replace(prop, "${1}"),
            // Without the regex, parse the value as-is — exactly what `replace`
            // does for input that does not match the pattern anyway.
            None => std::borrow::Cow::Borrowed(prop),
        };
        normalized.parse::<usize>().ok()
    }

    /// Retrieves the ExternalId from a Wikidata claim (statement).
    pub fn from_external_id_claim(claim: &Statement) -> Option<Self> {
        if *claim.main_snak().datatype() != SnakDataType::ExternalId {
            return None;
        }
        let prop_numeric = Self::prop_numeric(claim.property())?;
        let datavalue = (*claim.main_snak().data_value()).to_owned()?;
        let id = match datavalue.value() {
            Value::StringValue(id) => id,
            _ => return None,
        };
        // NOTE: the value is stored as Wikidata holds it. Rendering it in each
        // property's external display format (e.g. P213/ISNI
        // "0000000121849233" -> "0000 0001 2184 9233") is not implemented: it
        // needs a per-property spec, and it is the inverse of
        // `fix_property_value`, so the two would have to be introduced together
        // to keep `from_external_id_claim` -> `new` round-tripping.
        Some(Self::new(prop_numeric, id))
    }

    /// Returns the property number.
    pub const fn property(&self) -> usize {
        self.property
    }

    /// Returns the property as a typed ID.
    pub const fn property_id(&self) -> PropertyId {
        PropertyId::new(self.property as u64)
    }

    /// Returns the ID string.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The usual reference for a value taken from this ID's database: "stated
    /// in" `stated_in` (an item ID such as `Q123`), this ID, and "retrieved"
    /// today if `use_current_date`. See [`statements::stated_in_reference`].
    ///
    /// A `stated_in` that is not an item ID leaves the "stated in" snak out.
    pub fn as_reference(&self, stated_in: &str, use_current_date: bool) -> Reference {
        let id = Some((self.property_id(), self.id.as_str()));
        match ItemId::parse_lenient(stated_in) {
            Ok(item) => statements::stated_in_reference(item, id, use_current_date),
            Err(_) => {
                let mut snaks = vec![statements::external_id_snak(self.property_id(), &self.id)];
                if use_current_date {
                    snaks.push(statements::retrieved_today());
                }
                Reference::new(snaks)
            }
        }
    }
}

/// Lookups that hit the live Wikidata Action API, and so require the
/// `wikidata` feature for the [`Wikidata`] client. Each one is a single
/// search request; "single" means exactly one hit (none or several → `None`).
/// Failed requests also yield `None`.
#[cfg(feature = "wikidata")]
impl ExternalId {
    /// Searches Wikidata for a single item with the given query.
    #[deprecated(
        since = "0.3.0",
        note = "identifies as wikimisc rather than your tool; use `search_wikidata_single_item_with` with a `Wikidata` carrying your user agent"
    )]
    pub async fn search_wikidata_single_item(&self, query: &str) -> Option<String> {
        self.search_wikidata_single_item_with(&library_client(), query)
            .await
    }

    /// Searches Wikidata for a single item with the given query, using the
    /// supplied [`Wikidata`] client.
    pub async fn search_wikidata_single_item_with(
        &self,
        wd: &Wikidata,
        query: &str,
    ) -> Option<String> {
        let item = wd.search_single_item(query).await.ok()??;
        Some(item.to_string())
    }

    /// Searches Wikidata for a single item with this property/value.
    #[deprecated(
        since = "0.3.0",
        note = "identifies as wikimisc rather than your tool; use `get_item_for_external_id_value_with` with a `Wikidata` carrying your user agent"
    )]
    pub async fn get_item_for_external_id_value(&self) -> Option<String> {
        self.get_item_for_external_id_value_with(&library_client())
            .await
    }

    /// Searches Wikidata for a single item with this property/value, using
    /// the supplied [`Wikidata`] client.
    pub async fn get_item_for_external_id_value_with(&self, wd: &Wikidata) -> Option<String> {
        self.search_wikidata_single_item_with(wd, &self.search_query("").to_string())
            .await
    }

    /// Searches Wikidata for a single item matching `s` with this
    /// property/value.
    #[deprecated(
        since = "0.3.0",
        note = "identifies as wikimisc rather than your tool; use `get_item_for_string_external_id_value_with` with a `Wikidata` carrying your user agent"
    )]
    pub async fn get_item_for_string_external_id_value(&self, s: &str) -> Option<String> {
        self.get_item_for_string_external_id_value_with(&library_client(), s)
            .await
    }

    /// Searches Wikidata for a single item matching `s` with this
    /// property/value, using the supplied [`Wikidata`] client.
    pub async fn get_item_for_string_external_id_value_with(
        &self,
        wd: &Wikidata,
        s: &str,
    ) -> Option<String> {
        self.search_wikidata_single_item_with(wd, &self.search_query(s).to_string())
            .await
    }

    /// The search for items with this ID: `{text} haswbstatement:"P214=…"`.
    pub fn search_query(&self, text: &str) -> CirrusQuery {
        CirrusQuery::new(text).has_statement(self.property_id(), &self.id)
    }
}

/// The client behind the deprecated lookups that take none.
#[cfg(feature = "wikidata")]
fn library_client() -> Wikidata {
    Wikidata::new(concat!(
        "wikimisc/",
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("CARGO_PKG_REPOSITORY"),
        ")"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_string() {
        let ext_id = ExternalId::from_string("P123:ABC456DEF").unwrap();
        assert_eq!(ext_id.property, 123);
        assert_eq!(ext_id.id, "ABC456DEF");
    }

    #[test]
    fn test_isni() {
        let ext_id = ExternalId::new(213, "0000 0001 2184 9233");
        assert_eq!(ext_id.id, "0000000121849233");
    }

    #[test]
    fn test_to_string() {
        let ext_id = ExternalId::new(123, "ABC456DEF");
        assert_eq!(ext_id.to_string(), "P123:ABC456DEF".to_string());
    }

    #[test]
    fn test_prop_numeric() {
        assert_eq!(ExternalId::prop_numeric("  P123  "), Some(123));
        assert_eq!(ExternalId::prop_numeric("  FOO  "), None);
    }

    #[test]
    fn test_from_external_id_claim() {
        // Test OK
        let statement = Statement::new(
            "statement",
            StatementRank::Normal,
            Snak::new(
                SnakDataType::ExternalId,
                "P214",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::StringType,
                    Value::StringValue("ABCDEF".to_string()),
                )),
            ),
            vec![],
            vec![],
        );
        assert_eq!(
            ExternalId::from_string("P214:ABCDEF"),
            ExternalId::from_external_id_claim(&statement)
        );

        // Test wrong value type
        let statement = Statement::new(
            "statement",
            StatementRank::Normal,
            Snak::new(
                SnakDataType::ExternalId,
                "P214",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::StringType,
                    Value::Entity(EntityValue::new(EntityType::Item, "Q123")),
                )),
            ),
            vec![],
            vec![],
        );
        assert_eq!(None, ExternalId::from_external_id_claim(&statement));

        // Test wrong snak type
        let statement = Statement::new(
            "statement",
            StatementRank::Normal,
            Snak::new(
                SnakDataType::CommonsMedia,
                "P214",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::StringType,
                    Value::StringValue("ABCDEF".to_string()),
                )),
            ),
            vec![],
            vec![],
        );
        assert_eq!(None, ExternalId::from_external_id_claim(&statement));
    }

    #[test]
    fn test_from_string_invalid_formats() {
        assert!(ExternalId::from_string("notavalidid").is_none());
        assert!(ExternalId::from_string("").is_none());
        assert!(ExternalId::from_string("P123").is_none()); // missing colon and value
        assert!(ExternalId::from_string(":value").is_none()); // missing property number
        assert!(ExternalId::from_string("P:value").is_none()); // P without digits
    }

    #[test]
    fn test_from_string_preserves_id_case_and_chars() {
        let ext_id = ExternalId::from_string("P999:abc-DEF_123").unwrap();
        assert_eq!(ext_id.property(), 999);
        assert_eq!(ext_id.id(), "abc-DEF_123");
    }

    #[test]
    fn test_prop_numeric_lowercase() {
        assert_eq!(ExternalId::prop_numeric("p42"), Some(42));
        assert_eq!(ExternalId::prop_numeric("P42"), Some(42));
    }

    #[test]
    fn test_prop_numeric_with_surrounding_whitespace() {
        assert_eq!(ExternalId::prop_numeric(" P10 "), Some(10));
    }

    #[test]
    fn test_prop_numeric_zero_and_large() {
        assert_eq!(ExternalId::prop_numeric("P0"), Some(0));
        assert_eq!(ExternalId::prop_numeric("P999999"), Some(999_999));
    }

    #[test]
    fn test_fix_isni_strips_spaces() {
        // P213 (ISNI): spaces must be stripped
        let ext = ExternalId::new(213, "0000 0001 2345 6789");
        assert_eq!(ext.id(), "0000000123456789");
        // Other properties must keep spaces (if any)
        let other = ExternalId::new(214, "1234 5678");
        assert_eq!(other.id(), "1234 5678");
    }

    #[test]
    fn test_display_roundtrip_via_from_string() {
        let original = ExternalId::new(214, "12345");
        let s = original.to_string();
        let roundtripped = ExternalId::from_string(&s).unwrap();
        assert_eq!(original, roundtripped);
    }

    #[test]
    fn test_as_reference_without_date() {
        let ext = ExternalId::new(214, "12345");
        let reference = ext.as_reference("Q54919", false);
        let snaks = reference.snaks();

        // Must have exactly 2 snaks: P248 (stated in) and P214 (external ID)
        assert_eq!(snaks.len(), 2);

        let stated_in = snaks.iter().find(|s| s.property() == "P248").unwrap();
        assert_eq!(*stated_in.snak_type(), SnakType::Value);

        let ext_id_snak = snaks.iter().find(|s| s.property() == "P214").unwrap();
        assert_eq!(*ext_id_snak.datatype(), SnakDataType::ExternalId);
        if let Some(dv) = ext_id_snak.data_value() {
            assert_eq!(dv.value(), &Value::StringValue("12345".to_string()));
        } else {
            panic!("Expected data value for external-ID snak");
        }
    }

    #[test]
    fn test_as_reference_with_date_has_three_snaks() {
        let ext = ExternalId::new(214, "12345");
        let reference = ext.as_reference("Q54919", true);
        // P248, P214, P813 (retrieval date)
        assert_eq!(reference.snaks().len(), 3);
        assert!(reference.snaks().iter().any(|s| s.property() == "P813"));
    }

    #[test]
    fn test_as_reference_stated_in_item() {
        let ext = ExternalId::new(214, "12345");
        let reference = ext.as_reference("Q54919", false);
        let p248 = reference
            .snaks()
            .iter()
            .find(|s| s.property() == "P248")
            .unwrap();
        if let Some(dv) = p248.data_value() {
            match dv.value() {
                Value::Entity(ev) => assert_eq!(ev.id(), "Q54919"),
                _ => panic!("Expected entity value for P248"),
            }
        } else {
            panic!("Expected data value for P248");
        }
    }

    #[test]
    fn test_from_external_id_claim_p213_normalises_isni() {
        // When building an ExternalId from a P213 claim, spaces should be stripped.
        let statement = Statement::new(
            "statement",
            StatementRank::Normal,
            Snak::new(
                SnakDataType::ExternalId,
                "P213",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::StringType,
                    Value::StringValue("0000 0001 2184 9233".to_string()),
                )),
            ),
            vec![],
            vec![],
        );
        let ext = ExternalId::from_external_id_claim(&statement).unwrap();
        assert_eq!(ext.id(), "0000000121849233");
    }

    #[cfg(feature = "wikidata")]
    #[tokio::test]
    async fn test_get_item_for_external_id() {
        use crate::test_support::{mount_post, wikidata_for};
        use wiremock::MockServer;

        let server = MockServer::start().await;

        // wiremock-rs scans mocks in registration order and uses the *first*
        // match, so the specific 1-hit mocks must be registered before the
        // catch-all zero-hit fallback. The needles are the form-encoded
        // `srsearch` values.
        let one_hit = serde_json::json!({
            "batchcomplete": "",
            "query": {
                "searchinfo": { "totalhits": 1 },
                "search": [ { "title": "Q13520818", "ns": 0 } ]
            }
        });
        for srsearch in [
            "srsearch=haswbstatement%3A%22P214%3D30701597%22",
            "srsearch=Magnus+haswbstatement%3A%22P214%3D30701597%22",
        ] {
            mount_post(&server, srsearch, one_hit.clone()).await;
        }

        // Fallback: any other `list=search` request returns zero hits.
        mount_post(
            &server,
            "list=search",
            serde_json::json!({
                "batchcomplete": "",
                "query": { "searchinfo": { "totalhits": 0 }, "search": [] }
            }),
        )
        .await;

        let wd = wikidata_for(&server);

        // Test OK — bare external-ID lookup
        let ext_id = ExternalId::new(214, "30701597");
        assert_eq!(
            ext_id.get_item_for_external_id_value_with(&wd).await,
            Some("Q13520818".to_string())
        );

        // Test OK — combined string + external-ID lookup
        assert_eq!(
            ext_id
                .get_item_for_string_external_id_value_with(&wd, "Magnus")
                .await,
            Some("Q13520818".to_string())
        );

        // Test wrong string — falls through to the zero-hit fallback
        assert_eq!(
            ext_id
                .get_item_for_string_external_id_value_with(&wd, "ocshs87gvdsu6gsdi7vchkuchs")
                .await,
            None
        );

        // Test wrong ID — also falls through to the zero-hit fallback
        let ext_id = ExternalId::new(214, "3070159777777");
        assert_eq!(ext_id.get_item_for_external_id_value_with(&wd).await, None);
    }

    #[cfg(feature = "wikidata")]
    #[tokio::test]
    async fn test_get_item_for_external_id_returns_none_on_multiple_hits() {
        // Exercises the `totalhits != 1` branch: when search returns more than
        // one item, the helper must yield None rather than picking the first.
        use crate::test_support::{mount_post, wikidata_for};
        use wiremock::MockServer;

        let server = MockServer::start().await;
        mount_post(
            &server,
            "list=search",
            serde_json::json!({
                "batchcomplete": "",
                "query": {
                    "searchinfo": { "totalhits": 2 },
                    "search": [
                        { "title": "Q1", "ns": 0 },
                        { "title": "Q2", "ns": 0 }
                    ]
                }
            }),
        )
        .await;

        let wd = wikidata_for(&server);
        let ext_id = ExternalId::new(214, "12345");
        assert_eq!(ext_id.get_item_for_external_id_value_with(&wd).await, None);
    }

    #[cfg(feature = "wikidata")]
    #[tokio::test]
    async fn test_search_failure_is_none() {
        use crate::test_support::wikidata_for;
        use wiremock::MockServer;

        // No mock mounted: the server answers 404.
        let server = MockServer::start().await;
        let wd = wikidata_for(&server);
        let ext_id = ExternalId::new(214, "12345");
        assert_eq!(ext_id.get_item_for_external_id_value_with(&wd).await, None);
    }

    #[cfg(feature = "wikidata")]
    #[test]
    fn test_search_query_quotes_the_value() {
        let ext_id = ExternalId::new(244, "n 79021164");
        assert_eq!(
            ext_id.search_query("Adams").to_string(),
            "Adams haswbstatement:\"P244=n 79021164\""
        );
    }

    #[test]
    fn test_property_specific_normalisation() {
        assert_eq!(ExternalId::new(244, "n+79021164").id(), "n79021164");
        assert_eq!(ExternalId::new(244, "n%2079021164").id(), "n79021164");
        assert_eq!(ExternalId::new(1207, "n+2007008766").id(), "n2007008766");
        assert_eq!(ExternalId::new(1368, "LNC10-000012345").id(), "000012345");
        assert_eq!(ExternalId::new(1368, "000012345").id(), "000012345");
        assert_eq!(
            ExternalId::new(8034, "vtls_000012345").id(),
            "vtls/000012345"
        );
        // Other properties keep their value as it is.
        assert_eq!(ExternalId::new(21, "a+b_c LNC10-").id(), "a+b_c LNC10-");
    }

    #[test]
    fn test_case_insensitivity_and_typed_property() {
        assert!(ExternalId::is_case_insensitive(2002));
        assert!(!ExternalId::is_case_insensitive(214));
        assert_eq!(
            ExternalId::new(214, "1").property_id(),
            PropertyId::new(214)
        );
    }

    #[test]
    fn test_from_external_id_claim_novalue_snak_returns_none() {
        // A snak with ExternalId datatype but SnakType::NoValue has no data value;
        // from_external_id_claim must return None rather than panic.
        let statement = Statement::new(
            "statement",
            StatementRank::Normal,
            Snak::new(SnakDataType::ExternalId, "P214", SnakType::NoValue, None),
            vec![],
            vec![],
        );
        assert_eq!(ExternalId::from_external_id_claim(&statement), None);
    }

    #[test]
    fn test_from_external_id_claim_unknownvalue_snak_returns_none() {
        // UnknownValue ("somevalue") snaks also have no concrete data value.
        let statement = Statement::new(
            "statement",
            StatementRank::Normal,
            Snak::new(
                SnakDataType::ExternalId,
                "P214",
                SnakType::UnknownValue,
                None,
            ),
            vec![],
            vec![],
        );
        assert_eq!(ExternalId::from_external_id_claim(&statement), None);
    }

    #[test]
    fn test_property_and_id_accessors() {
        let ext = ExternalId::new(42, "ABC-123");
        assert_eq!(ext.property(), 42);
        assert_eq!(ext.id(), "ABC-123");
    }

    #[test]
    fn test_default_is_property_zero_empty_id() {
        let ext = ExternalId::default();
        assert_eq!(ext.property(), 0);
        assert_eq!(ext.id(), "");
    }

    #[test]
    fn test_prop_numeric_bare_digits_still_parse() {
        // When the regex does not match (no P/p prefix), replace() returns the
        // original string unchanged, and parse::<usize>() then succeeds on bare
        // digit strings.  This is an emergent behaviour of the implementation.
        assert_eq!(ExternalId::prop_numeric("123"), Some(123));
        assert_eq!(ExternalId::prop_numeric("0"), Some(0));
    }

    #[test]
    fn test_prop_numeric_non_numeric_returns_none() {
        // Strings that cannot be parsed as usize after regex processing return None.
        assert_eq!(ExternalId::prop_numeric("abc"), None);
        assert_eq!(ExternalId::prop_numeric(""), None);
        assert_eq!(ExternalId::prop_numeric("P"), None); // P with no digits
    }

    #[test]
    fn test_from_string_colon_in_id() {
        // The ID part may itself contain colons — only the first colon is the separator.
        let ext = ExternalId::from_string("P999:part1:part2").unwrap();
        assert_eq!(ext.property(), 999);
        assert_eq!(ext.id(), "part1:part2");
    }

    #[test]
    fn test_ordering() {
        // ExternalId derives Ord; verify that ordering is property-first, then id.
        let a = ExternalId::new(100, "aaa");
        let b = ExternalId::new(100, "bbb");
        let c = ExternalId::new(200, "aaa");
        assert!(a < b);
        assert!(b < c);
        assert!(a < c);
    }
    #[test]
    fn test_all_static_regexes_compile() {
        // The statics degrade to `None` rather than panicking, so assert here
        // that they are in fact available.
        assert!(RE_PROPERTY_NUMERIC.is_some());
        assert!(RE_FROM_STRING.is_some());
    }
}
