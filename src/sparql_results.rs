use serde::Deserialize;
use std::collections::HashMap;

use crate::sparql_value::SparqlValue;

pub type SparqlResultRow = HashMap<String, SparqlValue>;
pub type SparqlResultRows = Vec<SparqlResultRow>;
pub type SparqlRow = Vec<Option<SparqlValue>>;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SparqlApiResults {
    bindings: SparqlResultRows,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SparqlApiResult {
    head: HashMap<String, Vec<String>>,
    results: SparqlApiResults,
}

impl SparqlApiResult {
    pub fn head(&self) -> &HashMap<String, Vec<String>> {
        &self.head
    }

    pub fn bindings(&self) -> &SparqlResultRows {
        &self.results.bindings
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
}
