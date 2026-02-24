//! Useful functionality for dealing with external identifiers in Wikidata.

use crate::wikidata::Wikidata;
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::LazyLock;
use wikibase::*;

static RE_PROPERTY_NUMERIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*[Pp](\d+)\s*$"#).expect("Regexp error"));
static RE_FROM_STRING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[Pp](\d+):(.+)$"#).expect("Regexp error"));

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

    /// Fixes potential issues with the ID value for a given property number.
    fn fix_property_value(property: usize, id: &str) -> String {
        match property {
            213 => id.replace(' ', ""), // P213 (ISNI) has no spaces
            _ => id.to_string(),
        }
    }

    /// Returns a new ExternalId from a string like "P123:ABC456DEF".
    pub fn from_string(s: &str) -> Option<Self> {
        let captures = RE_FROM_STRING.captures(s)?;
        // Group 1 is already `\d+` from the regex, so parse directly without
        // running the second RE_PROPERTY_NUMERIC regex that prop_numeric uses.
        let property = captures.get(1)?.as_str().parse::<usize>().ok()?;
        let id = captures.get(2)?.as_str();
        Some(Self::new(property, id))
    }

    /// Parses a property number from a string like "P123".
    pub fn prop_numeric(prop: &str) -> Option<usize> {
        RE_PROPERTY_NUMERIC
            .replace(prop, "${1}")
            .parse::<usize>()
            .ok()
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
        // TODO change value eg P213(ISNI) from Wikidata format to external format
        Some(Self::new(prop_numeric, id))
    }

    /// Searches Wikidata for a single item with the given query.
    /// Returns None if none or multiple items are found.
    pub async fn search_wikidata_single_item(&self, query: &str) -> Option<String> {
        let url = "https://www.wikidata.org/w/api.php";
        let wd = Wikidata::new();
        let client = wd.reqwest_client().ok()?;
        let text: String = client
            .get(url)
            .query(&[
                ("action", "query"),
                ("list", "search"),
                ("srnamespace", "0"),
                ("format", "json"),
                ("srsearch", query),
            ])
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        let j: serde_json::Value = serde_json::from_str(&text).ok()?;
        if j["query"]["searchinfo"]["totalhits"].as_i64()? == 1 {
            Some(j["query"]["search"][0]["title"].as_str()?.to_string())
        } else {
            None
        }
    }

    /// Searches Wikidata for a single item with the given property/value.
    /// Returns None if none or multiple items are found.
    pub async fn get_item_for_external_id_value(&self) -> Option<String> {
        let query = format!("haswbstatement:\"P{}={}\"", self.property, self.id);
        self.search_wikidata_single_item(&query).await
    }

    /// Searches Wikidata for a single item with the given property/value and string.
    /// Returns None if none or multiple items are found.
    pub async fn get_item_for_string_external_id_value(&self, s: &str) -> Option<String> {
        let query = format!("{s} haswbstatement:\"P{}={}\"", self.property, &self.id);
        self.search_wikidata_single_item(&query).await
    }

    /// Returns the property number.
    pub fn property(&self) -> usize {
        self.property
    }

    /// Returns the ID string.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns a Reference object for this ExternalId.
    pub fn as_reference(&self, stated_in: &str, use_current_date: bool) -> Reference {
        let time = Utc::now();
        let time = time.format("+%Y-%m-%dT00:00:00Z").to_string();
        let mut reference = vec![
            Snak::new(
                SnakDataType::WikibaseItem,
                "P248",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::EntityId,
                    Value::Entity(EntityValue::new(EntityType::Item, stated_in)),
                )),
            ),
            Snak::new(
                SnakDataType::ExternalId,
                format!("P{}", self.property),
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::StringType,
                    Value::StringValue(self.id.to_owned()),
                )),
            ),
        ];
        if use_current_date {
            reference.push(Snak::new(
                SnakDataType::Time,
                "P813",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Time,
                    Value::Time(TimeValue::new(
                        0,
                        0,
                        "http://www.wikidata.org/entity/Q1985727",
                        11,
                        &time,
                        0,
                    )),
                )),
            ));
        }
        Reference::new(reference)
    }
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

    #[tokio::test]
    async fn test_get_item_for_external_id() {
        // Test OK
        let ext_id = ExternalId::new(214, "30701597");
        assert_eq!(
            ext_id.get_item_for_external_id_value().await,
            Some("Q13520818".to_string())
        );

        // Test OK
        assert_eq!(
            ext_id.get_item_for_string_external_id_value("Magnus").await,
            Some("Q13520818".to_string())
        );

        // Test wrong string
        assert_eq!(
            ext_id
                .get_item_for_string_external_id_value("ocshs87gvdsu6gsdi7vchkuchs")
                .await,
            None
        );

        // Test wrong ID
        let ext_id = ExternalId::new(214, "3070159777777");
        assert_eq!(ext_id.get_item_for_external_id_value().await, None);

        // TODOO multiple items
    }
}
