//! Wire-format types for a SPARQL JSON API response (the body returned by
//! WDQS and other endpoints when `Accept: application/sparql-results+json`).
//!
//! [`SparqlApiResult`] is the deserializable struct, for SELECT and ASK
//! results from WDQS (Blazegraph) and QLever alike; [`SparqlRow`] is the
//! row representation used by [`crate::sparql_table::SparqlTable`] after
//! column-projection through the table's headers.

use serde::Deserialize;
use std::collections::HashMap;

use crate::sparql_value::SparqlValue;

pub type SparqlResultRow = HashMap<String, SparqlValue>;
pub type SparqlResultRows = Vec<SparqlResultRow>;
pub type SparqlRow = Vec<Option<SparqlValue>>;

#[derive(Debug, Clone, Deserialize, Default)]
struct SparqlApiResults {
    bindings: SparqlResultRows,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SparqlApiResult {
    head: HashMap<String, Vec<String>>,
    // An ASK result has `boolean` instead of `results`; QLever then omits
    // `results` entirely.
    #[serde(default)]
    results: SparqlApiResults,
    #[serde(default)]
    boolean: Option<bool>,
}

impl SparqlApiResult {
    pub fn head(&self) -> &HashMap<String, Vec<String>> {
        &self.head
    }

    pub fn bindings(&self) -> &SparqlResultRows {
        &self.results.bindings
    }

    /// The answer to an ASK query; `None` for a SELECT result.
    pub fn boolean(&self) -> Option<bool> {
        self.boolean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sparql_results() {
        let j = json!({"head":{"vars":["q","x"]},"results":{"bindings":[{"q":{"type":"uri","value":"http://www.wikidata.org/entity/Q21"},"x":{"type":"literal","value":"GB-ENG"}},{"q":{"type":"uri","value":"http://www.wikidata.org/entity/Q145"},"x":{"type":"literal","value":"GB-UKM"}},{"q":{"type":"uri","value":"http://www.wikidata.org/entity/Q21272276"},"x":{"type":"literal","value":"GB-CAM"}}]}});
        let r = SparqlApiResult::deserialize(j).unwrap();
        assert_eq!(r.head["vars"], vec!["q", "x"]);
        assert_eq!(r.results.bindings.len(), 3);
        assert_eq!(
            r.results.bindings[0]["q"],
            SparqlValue::Entity("Q21".to_string())
        );
        assert_eq!(
            r.results.bindings[0]["x"],
            SparqlValue::Literal("GB-ENG".to_string())
        );
    }

    #[test]
    fn test_sparql_api_result_default_is_empty() {
        let r = SparqlApiResult::default();
        assert!(r.head().is_empty());
        assert!(r.bindings().is_empty());
    }

    #[test]
    fn test_sparql_api_result_accessors() {
        // Verify head() and bindings() return references to the parsed data,
        // not a fresh allocation, by checking values match deserialised input.
        let j = json!({
            "head": {"vars": ["item"]},
            "results": {"bindings": [
                {"item": {"type": "uri", "value": "http://www.wikidata.org/entity/Q42"}}
            ]}
        });
        let r = SparqlApiResult::deserialize(j).unwrap();
        assert_eq!(r.head().get("vars").map(|v| v.len()), Some(1));
        assert_eq!(r.bindings().len(), 1);
        assert_eq!(
            r.bindings()[0].get("item"),
            Some(&SparqlValue::Entity("Q42".to_string()))
        );
    }

    #[test]
    fn test_sparql_api_result_empty_bindings() {
        // Empty bindings is a valid SPARQL response and must deserialize cleanly.
        let j = json!({
            "head": {"vars": ["x"]},
            "results": {"bindings": []}
        });
        let r = SparqlApiResult::deserialize(j).unwrap();
        assert_eq!(r.head().get("vars"), Some(&vec!["x".to_string()]));
        assert!(r.bindings().is_empty());
    }

    #[test]
    fn test_sparql_api_result_ask() {
        // QLever's shape: no `results` key at all.
        let r: SparqlApiResult = serde_json::from_str(r#"{"boolean":true,"head":{}}"#).unwrap();
        assert_eq!(r.boolean(), Some(true));
        assert!(r.bindings().is_empty());

        // Blazegraph's shape.
        let r: SparqlApiResult = serde_json::from_str(r#"{"head":{},"boolean":false}"#).unwrap();
        assert_eq!(r.boolean(), Some(false));
    }

    #[test]
    fn test_sparql_api_result_select_has_no_boolean() {
        let r: SparqlApiResult =
            serde_json::from_str(r#"{"head":{"vars":[]},"results":{"bindings":[]}}"#).unwrap();
        assert_eq!(r.boolean(), None);
    }

    // The same query (Berlin's coordinates, inception, population and image)
    // as answered by each engine, verbatim apart from whitespace.
    const WDQS_RESPONSE: &str = r#"{
      "head" : { "vars" : [ "item", "coord", "dob", "pop", "img", "moon" ] },
      "results" : { "bindings" : [ {
        "item" : { "type" : "uri", "value" : "http://www.wikidata.org/entity/Q64" },
        "coord" : { "datatype" : "http://www.opengis.net/ont/geosparql#wktLiteral", "type" : "literal", "value" : "Point(13.383333333 52.516666666)" },
        "dob" : { "datatype" : "http://www.w3.org/2001/XMLSchema#dateTime", "type" : "literal", "value" : "1244-01-01T00:00:00Z" },
        "pop" : { "datatype" : "http://www.w3.org/2001/XMLSchema#decimal", "type" : "literal", "value" : "3700577" },
        "img" : { "type" : "uri", "value" : "http://commons.wikimedia.org/wiki/Special:FilePath/Cityscape%20Berlin.jpg" }
      } ] }
    }"#;
    const QLEVER_RESPONSE: &str = r#"{"head":{"vars":["item","coord","dob","pop","img","moon"]},"results":{"bindings":[{"item":{"type":"uri","value":"http://www.wikidata.org/entity/Q64"},"coord":{"datatype":"http://www.opengis.net/ont/geosparql#wktLiteral","type":"literal","value":"POINT(13.383333 52.516667)"},"dob":{"datatype":"http://www.w3.org/2001/XMLSchema#dateTime","type":"literal","value":"1244-01-01T00:00:00Z"},"pop":{"datatype":"http://www.w3.org/2001/XMLSchema#decimal","type":"literal","value":"3700577.0"},"img":{"type":"uri","value":"http://commons.wikimedia.org/wiki/Special:FilePath/Cityscape%20Berlin.jpg"}}]},"meta":{"query-time-ms":9,"result-size-total":1}}"#;

    #[test]
    fn test_wdqs_and_qlever_responses_parse_alike() {
        for (engine, json) in [("WDQS", WDQS_RESPONSE), ("QLever", QLEVER_RESPONSE)] {
            let r: SparqlApiResult = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("{engine} response must parse: {e}"));
            assert_eq!(r.head()["vars"].len(), 6, "{engine}");
            let row = &r.bindings()[0];
            assert_eq!(row["item"], SparqlValue::Entity("Q64".into()), "{engine}");
            assert_eq!(
                row["dob"],
                SparqlValue::Time("1244-01-01".into()),
                "{engine}"
            );
            assert_eq!(
                row["img"],
                SparqlValue::File("Cityscape Berlin.jpg".into()),
                "{engine}"
            );
            match &row["coord"] {
                SparqlValue::Location(ll) => {
                    assert!((ll.lat - 52.516667).abs() < 1e-5, "{engine}");
                    assert!((ll.lon - 13.383333).abs() < 1e-5, "{engine}");
                }
                other => panic!("{engine}: expected a location, got {other:?}"),
            }
            assert!(!row.contains_key("moon"), "{engine}: unbound stays absent");
        }
    }
}
