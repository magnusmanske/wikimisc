use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::sparql_value::SparqlValue;

pub type SparqlResultRow = HashMap<String, SparqlValue>;
pub type SparqlResultRows = Vec<SparqlResultRow>;

lazy_static! {
    static ref SPARQL_REQUEST_COUNTER: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
}

#[derive(Debug, Clone, Deserialize)]
pub struct SparqlApiResults {
    bindings: SparqlResultRows,
}

#[derive(Debug, Clone, Deserialize)]
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
}
