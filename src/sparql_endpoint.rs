//! Where a SPARQL query is sent, and the prefixes it needs on the way.
//!
//! WDQS (Blazegraph) declares the Wikidata prefixes (`wd:`, `wdt:`, `p:`, …)
//! implicitly; QLever declares none, so a query written for WDQS fails there
//! with "Prefix wdt was not registered" unless they are prepended.
//! [`SparqlEndpoint`] pairs an endpoint URL with the prefixes to add, and
//! [`SparqlEndpoint::prepare_query`] adds them — skipping any the query
//! declares itself, because Blazegraph rejects a prefix declared twice.
//!
//! This is plain configuration with no I/O; `sparql_client::SparqlClient`
//! (feature `sparql-client`) sends the prepared query.

use regex::Regex;
use std::collections::HashSet;
use std::fmt::Write;
use std::sync::LazyLock;

/// The Wikidata Query Service v1, running Blazegraph. To be shut down once
/// v2 has replaced it.
pub const WDQS_URL: &str = "https://query.wikidata.org/sparql";

/// The scholarly articles graph on WDQS v1. As on v2, scholarly articles are
/// *only* here, not at [`WDQS_URL`].
pub const WDQS_SCHOLARLY_URL: &str = "https://query-scholarly.wikidata.org/sparql";

/// Wikidata's main graph — everything except scholarly articles — on the
/// Wikidata Query Service v2 (QLever). Named by graph rather than by
/// backend: when v2 takes over another host name, only this changes.
pub const WIKIDATA_MAIN_URL: &str = "https://query-next.wikidata.org/sparql";

/// Wikidata's scholarly articles graph on the Wikidata Query Service v2.
/// Scholarly articles are *only* here, and this graph has no labels for
/// items that are not articles, so a query needing both has to be split.
pub const WIKIDATA_SCHOLARLY_URL: &str = "https://query-scholarly-next.wikidata.org/sparql";

/// QLever's own Wikidata endpoint at the University of Freiburg, not
/// Wikimedia's; see [`WIKIDATA_MAIN_URL`] for that. The older `qlever.cs.uni-freiburg.de` address
/// still works, but only as a redirect to this one.
pub const QLEVER_WIKIDATA_URL: &str = "https://qlever.dev/api/wikidata";

/// The prefixes WDQS declares implicitly, as `(name, IRI)` pairs. Prepending
/// these makes a query written for WDQS run unchanged on an endpoint that
/// declares none, such as QLever.
pub const WIKIDATA_PREFIXES: &[(&str, &str)] = &[
    ("wd", "http://www.wikidata.org/entity/"),
    ("wds", "http://www.wikidata.org/entity/statement/"),
    ("wdv", "http://www.wikidata.org/value/"),
    ("wdt", "http://www.wikidata.org/prop/direct/"),
    ("wdtn", "http://www.wikidata.org/prop/direct-normalized/"),
    ("wikibase", "http://wikiba.se/ontology#"),
    ("p", "http://www.wikidata.org/prop/"),
    ("ps", "http://www.wikidata.org/prop/statement/"),
    ("psv", "http://www.wikidata.org/prop/statement/value/"),
    (
        "psn",
        "http://www.wikidata.org/prop/statement/value-normalized/",
    ),
    ("pq", "http://www.wikidata.org/prop/qualifier/"),
    ("pqv", "http://www.wikidata.org/prop/qualifier/value/"),
    (
        "pqn",
        "http://www.wikidata.org/prop/qualifier/value-normalized/",
    ),
    ("pr", "http://www.wikidata.org/prop/reference/"),
    ("prv", "http://www.wikidata.org/prop/reference/value/"),
    (
        "prn",
        "http://www.wikidata.org/prop/reference/value-normalized/",
    ),
    ("wdref", "http://www.wikidata.org/reference/"),
    ("wdno", "http://www.wikidata.org/prop/novalue/"),
    ("wdata", "http://www.wikidata.org/wiki/Special:EntityData/"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("schema", "http://schema.org/"),
    ("prov", "http://www.w3.org/ns/prov#"),
    ("geo", "http://www.opengis.net/ont/geosparql#"),
    ("geof", "http://www.opengis.net/def/geosparql/function/"),
    ("bd", "http://www.bigdata.com/rdf#"),
    ("ontolex", "http://www.w3.org/ns/lemon/ontolex#"),
    ("dct", "http://purl.org/dc/terms/"),
];

// A `PREFIX name:` declaration; the keyword is case-insensitive, the name is
// not. The keyword must not continue a variable or prefixed name, so that
// `?prefix wdt:P31` is not taken for a declaration of `wdt:`. Like the statics in `sparql_value`, an unavailable regex degrades to
// "matches nothing" — every configured prefix is then prepended — rather than
// panicking; `test_static_regex_compiles` keeps the literal honest.
static RE_PREFIX_DECL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w?$:])(?i:PREFIX)\s+([^\s:]*):").ok());

/// A SPARQL endpoint URL and the prefixes to declare in every query sent to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlEndpoint {
    url: String,
    prefixes: Vec<(String, String)>,
}

impl SparqlEndpoint {
    /// An endpoint with no prefixes added to its queries.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            prefixes: Vec::new(),
        }
    }

    /// The Wikidata Query Service. It declares the Wikidata prefixes itself,
    /// so none are added.
    pub fn wdqs() -> Self {
        Self::new(WDQS_URL)
    }

    /// The scholarly articles graph on WDQS v1 ([`WDQS_SCHOLARLY_URL`]),
    /// which declares the Wikidata prefixes itself, like [`Self::wdqs`].
    pub fn wdqs_scholarly() -> Self {
        Self::new(WDQS_SCHOLARLY_URL)
    }

    /// Wikidata's main graph on WDQS v2 ([`WIKIDATA_MAIN_URL`]). It declares
    /// most of [`WIKIDATA_PREFIXES`] itself; all of them are added anyway, so
    /// that queries written for WDQS v1 find every prefix it declared.
    pub fn wikidata_main() -> Self {
        Self::with_wikidata_prefixes(WIKIDATA_MAIN_URL)
    }

    /// Wikidata's scholarly articles graph on WDQS v2
    /// ([`WIKIDATA_SCHOLARLY_URL`]), with prefixes as for
    /// [`Self::wikidata_main`].
    pub fn wikidata_scholarly() -> Self {
        Self::with_wikidata_prefixes(WIKIDATA_SCHOLARLY_URL)
    }

    /// QLever's own Wikidata endpoint ([`QLEVER_WIKIDATA_URL`]), with
    /// [`WIKIDATA_PREFIXES`] added so that queries written for WDQS run
    /// unchanged.
    pub fn qlever_wikidata() -> Self {
        Self::with_wikidata_prefixes(QLEVER_WIKIDATA_URL)
    }

    fn with_wikidata_prefixes(url: &str) -> Self {
        Self::new(url).with_prefixes(WIKIDATA_PREFIXES.iter().copied())
    }

    /// Adds a prefix, replacing any earlier one of the same name. Both
    /// `("wd", "http://…/")` and `("wd:", "<http://…/>")` are accepted.
    pub fn with_prefix(mut self, name: impl Into<String>, iri: impl Into<String>) -> Self {
        let name = name.into().trim_end_matches(':').to_string();
        let iri = iri.into().trim_matches(['<', '>']).to_string();
        match self.prefixes.iter_mut().find(|(n, _)| *n == name) {
            Some(existing) => existing.1 = iri,
            None => self.prefixes.push((name, iri)),
        }
        self
    }

    /// Adds several prefixes; see [`Self::with_prefix`].
    pub fn with_prefixes<N, I>(self, prefixes: impl IntoIterator<Item = (N, I)>) -> Self
    where
        N: Into<String>,
        I: Into<String>,
    {
        prefixes.into_iter().fold(self, |endpoint, (name, iri)| {
            endpoint.with_prefix(name, iri)
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The prefixes added to each query, as `(name, IRI)` pairs.
    pub fn prefixes(&self) -> &[(String, String)] {
        &self.prefixes
    }

    /// Returns `sparql` with a `PREFIX` declaration prepended for every
    /// configured prefix the query does not already declare. The query's own
    /// declaration wins.
    ///
    /// Declarations are found textually, so one inside a comment or string
    /// literal also counts — the query then fails exactly as it would have
    /// without this endpoint's prefixes.
    pub fn prepare_query(&self, sparql: &str) -> String {
        let declared = declared_prefixes(sparql);
        let mut query = String::new();
        for (name, iri) in &self.prefixes {
            if !declared.contains(name.as_str()) {
                // Writing to a `String` cannot fail.
                let _ = writeln!(query, "PREFIX {name}: <{iri}>");
            }
        }
        query.push_str(sparql);
        query
    }
}

/// Names of the prefixes `sparql` declares itself.
fn declared_prefixes(sparql: &str) -> HashSet<&str> {
    let Some(re) = RE_PREFIX_DECL.as_ref() else {
        return HashSet::new();
    };
    re.captures_iter(sparql)
        .filter_map(|caps| caps.get(1))
        .map(|name| name.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUERY: &str = "SELECT ?x WHERE { ?x wdt:P31 wd:Q5 }";

    #[test]
    fn test_static_regex_compiles() {
        assert!(RE_PREFIX_DECL.is_some());
    }

    #[test]
    fn test_no_prefixes_leaves_query_unchanged() {
        let endpoint = SparqlEndpoint::new("http://localhost/sparql");
        assert_eq!(endpoint.prepare_query(QUERY), QUERY);
        assert_eq!(endpoint.url(), "http://localhost/sparql");
        assert!(endpoint.prefixes().is_empty());
    }

    #[test]
    fn test_prefixes_are_prepended_in_order() {
        let endpoint = SparqlEndpoint::new("http://localhost/sparql")
            .with_prefix("wd", "http://www.wikidata.org/entity/")
            .with_prefix("wdt", "http://www.wikidata.org/prop/direct/");
        assert_eq!(
            endpoint.prepare_query(QUERY),
            "PREFIX wd: <http://www.wikidata.org/entity/>\n\
             PREFIX wdt: <http://www.wikidata.org/prop/direct/>\n"
                .to_string()
                + QUERY
        );
    }

    #[test]
    fn test_prefixes_declared_by_the_query_are_skipped() {
        let endpoint = SparqlEndpoint::new("http://localhost/sparql")
            .with_prefix("wd", "http://www.wikidata.org/entity/")
            .with_prefix("wdt", "http://www.wikidata.org/prop/direct/");
        // Keyword case does not matter; the query's own IRI is kept.
        let query = format!("prefix wd: <http://example.org/>\n{QUERY}");
        assert_eq!(
            endpoint.prepare_query(&query),
            format!("PREFIX wdt: <http://www.wikidata.org/prop/direct/>\n{query}")
        );
    }

    #[test]
    fn test_prefix_names_are_case_sensitive() {
        let endpoint = SparqlEndpoint::new("u").with_prefix("wd", "http://a/");
        let query = "PREFIX WD: <http://b/>\nSELECT * {}";
        assert!(endpoint
            .prepare_query(query)
            .starts_with("PREFIX wd: <http://a/>\n"));
    }

    #[test]
    fn test_similar_prefix_names_do_not_shadow_each_other() {
        // `wdt:` being declared must not count as `wd:` being declared.
        let endpoint = SparqlEndpoint::new("u").with_prefix("wd", "http://a/");
        let query = "PREFIX wdt: <http://b/>\nSELECT * {}";
        assert!(endpoint
            .prepare_query(query)
            .starts_with("PREFIX wd: <http://a/>\n"));
    }

    #[test]
    fn test_variable_named_prefix_is_not_a_declaration() {
        let endpoint = SparqlEndpoint::new("u").with_prefix("wdt", "http://a/");
        for query in [
            "SELECT * { ?prefix wdt:P31 ?x }",
            "SELECT * { $prefix wdt:P31 ?x }",
            "SELECT * { ex:prefix wdt:P31 ?x }",
            "SELECT * { ?x myprefix wdt:P31 }",
        ] {
            assert!(
                endpoint.prepare_query(query).starts_with("PREFIX wdt:"),
                "{query}"
            );
        }
        // A declaration at the very start, or after a comment, still counts.
        for query in [
            "PREFIX wdt: <http://b/> ASK {}",
            "#x\nPREFIX wdt: <http://b/> ASK {}",
        ] {
            assert_eq!(endpoint.prepare_query(query), query);
        }
    }

    #[test]
    fn test_empty_prefix_name() {
        let endpoint = SparqlEndpoint::new("u").with_prefix("", "http://a/");
        assert_eq!(
            endpoint.prepare_query("ASK {}"),
            "PREFIX : <http://a/>\nASK {}"
        );
        assert_eq!(
            endpoint.prepare_query("PREFIX : <http://b/> ASK {}"),
            "PREFIX : <http://b/> ASK {}"
        );
    }

    #[test]
    fn test_with_prefix_normalises_and_replaces() {
        let endpoint = SparqlEndpoint::new("u")
            .with_prefix("wd:", "<http://old/>")
            .with_prefix("wd", "http://new/");
        assert_eq!(
            endpoint.prefixes(),
            &[("wd".to_string(), "http://new/".to_string())]
        );
    }

    #[test]
    fn test_with_prefixes_accepts_owned_and_borrowed() {
        let endpoint = SparqlEndpoint::new("u")
            .with_prefixes([("a", "http://a/")])
            .with_prefixes(vec![("b".to_string(), "http://b/".to_string())]);
        assert_eq!(endpoint.prefixes().len(), 2);
    }

    #[test]
    fn test_wdqs_presets() {
        for (endpoint, url) in [
            (SparqlEndpoint::wdqs(), WDQS_URL),
            (SparqlEndpoint::wdqs_scholarly(), WDQS_SCHOLARLY_URL),
        ] {
            assert_eq!(endpoint.url(), url);
            assert_eq!(endpoint.prepare_query(QUERY), QUERY);
        }
    }

    #[test]
    fn test_qlever_wikidata_preset() {
        let endpoint = SparqlEndpoint::qlever_wikidata();
        assert_eq!(endpoint.url(), QLEVER_WIKIDATA_URL);
        assert_eq!(endpoint.prefixes().len(), WIKIDATA_PREFIXES.len());
        let prepared = endpoint.prepare_query(QUERY);
        assert!(prepared.contains("PREFIX wd: <http://www.wikidata.org/entity/>\n"));
        assert!(prepared.contains("PREFIX wdt: <http://www.wikidata.org/prop/direct/>\n"));
        assert!(prepared.ends_with(QUERY));
    }

    #[test]
    fn test_wikidata_v2_presets() {
        for (endpoint, url) in [
            (SparqlEndpoint::wikidata_main(), WIKIDATA_MAIN_URL),
            (SparqlEndpoint::wikidata_scholarly(), WIKIDATA_SCHOLARLY_URL),
        ] {
            assert_eq!(endpoint.url(), url);
            assert_eq!(endpoint.prefixes().len(), WIKIDATA_PREFIXES.len());
        }
    }

    #[test]
    fn test_wikidata_prefix_names_are_unique() {
        let names: HashSet<_> = WIKIDATA_PREFIXES.iter().map(|(name, _)| name).collect();
        assert_eq!(names.len(), WIKIDATA_PREFIXES.len());
    }
}
