//! Blazegraph's named subqueries, inlined as SPARQL 1.1 subqueries.
//!
//! ```sparql
//! SELECT ?item WITH { SELECT ?item WHERE { … } } AS %items
//! WHERE { INCLUDE %items … }
//! ```
//!
//! becomes `SELECT ?item WHERE { { SELECT ?item WHERE { … } } … }`. Blazegraph
//! evaluates a named subquery once and joins its solutions wherever it is
//! included, which is what a subquery means in SPARQL 1.1 too. A named
//! subquery may include another one.

use crate::sparql_lexer::{tokenize, Brackets, Token};
use std::collections::HashMap;
use std::fmt;

/// The outcome of [`rewrite_named_subqueries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedSubqueryRewrite {
    /// The query uses no named subqueries.
    Unchanged,
    /// The query with every named subquery inlined.
    Rewritten(String),
    /// The query uses named subqueries this rewrite cannot inline.
    Unsupported(UnsupportedNamedSubquery),
}

/// Why [`rewrite_named_subqueries`] left a query unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnsupportedNamedSubquery {
    /// Brackets do not balance, or a `WITH { … }` is not followed by
    /// `AS %name`.
    Syntax,
    /// `INCLUDE %name` names no subquery.
    Undefined(String),
    /// The subqueries include each other in a cycle, or one is defined twice.
    Ambiguous(String),
}

impl fmt::Display for UnsupportedNamedSubquery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax => f.write_str("named subqueries could not be parsed"),
            Self::Undefined(name) => write!(f, "named subquery %{name} is not defined"),
            Self::Ambiguous(name) => {
                write!(
                    f,
                    "named subquery %{name} is defined twice or includes itself"
                )
            }
        }
    }
}

/// Inlines Blazegraph's named subqueries (`WITH { … } AS %name` and
/// `INCLUDE %name`) as SPARQL 1.1 subqueries, splicing the query text.
pub fn rewrite_named_subqueries(sparql: &str) -> NamedSubqueryRewrite {
    let tokens = tokenize(sparql);
    let includes = includes(&tokens);
    let definitions_present = tokens
        .windows(2)
        .any(|pair| pair[0].is_keyword("WITH") && pair[1].is_punct('{'));
    if includes.is_empty() && !definitions_present {
        return NamedSubqueryRewrite::Unchanged;
    }
    match Inliner::new(sparql, &tokens).and_then(|inliner| inliner.rewrite(&includes)) {
        Ok(query) => NamedSubqueryRewrite::Rewritten(query),
        Err(reason) => NamedSubqueryRewrite::Unsupported(reason),
    }
}

type Result<T> = std::result::Result<T, UnsupportedNamedSubquery>;

/// An `INCLUDE %name`: its name, and the byte range to replace.
struct Include<'a> {
    name: &'a str,
    start: usize,
    end: usize,
}

/// A `WITH { body } AS %name`: the byte range of the body, and of the whole.
struct Definition {
    body: (usize, usize),
    whole: (usize, usize),
}

struct Inliner<'a> {
    sparql: &'a str,
    definitions: HashMap<&'a str, Definition>,
}

impl<'a> Inliner<'a> {
    fn new(sparql: &'a str, tokens: &[Token<'a>]) -> Result<Self> {
        let brackets = Brackets::new(tokens).ok_or(UnsupportedNamedSubquery::Syntax)?;
        let mut definitions = HashMap::new();
        for (i, token) in tokens.iter().enumerate() {
            let opens_body = tokens.get(i + 1).is_some_and(|t| t.is_punct('{'));
            if !token.is_keyword("WITH") || !opens_body {
                continue;
            }
            let close = brackets
                .partner(i + 1)
                .ok_or(UnsupportedNamedSubquery::Syntax)?;
            let (name, end) = match tokens.get(close + 1..close + 4) {
                Some([as_, percent, name]) if as_.is_keyword("AS") && percent.is_punct('%') => {
                    (name.text, name.end())
                }
                _ => return Err(UnsupportedNamedSubquery::Syntax),
            };
            let body = (tokens[i + 1].end(), tokens[close].start);
            let definition = Definition {
                body,
                whole: (token.start, end),
            };
            if definitions.insert(name, definition).is_some() {
                return Err(UnsupportedNamedSubquery::Ambiguous(name.to_string()));
            }
        }
        Ok(Self {
            sparql,
            definitions,
        })
    }

    /// The query without its definitions, every include replaced by the
    /// subquery it names.
    fn rewrite(&self, includes: &[Include<'a>]) -> Result<String> {
        let mut edits: Vec<(usize, usize, String)> = self
            .definitions
            .values()
            .map(|d| (d.whole.0, d.whole.1, String::new()))
            .collect();
        for include in includes.iter().filter(|inc| !self.in_definition(inc.start)) {
            let text = self.inlined(include.name, &mut Vec::new(), includes)?;
            edits.push((include.start, include.end, text));
        }
        Ok(splice(self.sparql, (0, self.sparql.len()), edits))
    }

    /// `{ body }` of the named subquery, with its own includes inlined.
    fn inlined(
        &self,
        name: &'a str,
        path: &mut Vec<&'a str>,
        includes: &[Include<'a>],
    ) -> Result<String> {
        let definition = self
            .definitions
            .get(name)
            .ok_or_else(|| UnsupportedNamedSubquery::Undefined(name.to_string()))?;
        if path.contains(&name) {
            return Err(UnsupportedNamedSubquery::Ambiguous(name.to_string()));
        }
        path.push(name);
        let (start, end) = definition.body;
        let mut edits = Vec::new();
        for include in includes.iter().filter(|i| start <= i.start && i.end <= end) {
            edits.push((
                include.start,
                include.end,
                self.inlined(include.name, path, includes)?,
            ));
        }
        path.pop();
        Ok(format!("{{{}}}", splice(self.sparql, (start, end), edits)))
    }

    fn in_definition(&self, offset: usize) -> bool {
        self.definitions
            .values()
            .any(|d| d.whole.0 <= offset && offset < d.whole.1)
    }
}

/// Every `INCLUDE %name` in the query.
fn includes<'a>(tokens: &[Token<'a>]) -> Vec<Include<'a>> {
    tokens
        .windows(3)
        .filter_map(|window| match window {
            [include, percent, name] if include.is_keyword("INCLUDE") && percent.is_punct('%') => {
                Some(Include {
                    name: name.text,
                    start: include.start,
                    end: name.end(),
                })
            }
            _ => None,
        })
        .collect()
}

/// `sparql[range]` with `edits` (non-overlapping byte ranges inside it, and
/// their replacements) made.
fn splice(
    sparql: &str,
    (start, end): (usize, usize),
    mut edits: Vec<(usize, usize, String)>,
) -> String {
    edits.sort_by_key(|&(from, _, _)| from);
    let mut out = String::with_capacity(end.saturating_sub(start));
    let mut kept_from = start;
    for (from, to, text) in edits {
        out.push_str(sparql.get(kept_from..from).unwrap_or_default());
        out.push_str(&text);
        kept_from = to;
    }
    out.push_str(sparql.get(kept_from..end).unwrap_or_default());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql_endpoint::WIKIDATA_PREFIXES;

    fn rewritten(sparql: &str) -> String {
        match rewrite_named_subqueries(sparql) {
            NamedSubqueryRewrite::Rewritten(query) => {
                let mut parser = spargebra::SparqlParser::new();
                for (name, iri) in WIKIDATA_PREFIXES {
                    parser = parser.with_prefix(*name, *iri).unwrap();
                }
                if let Err(e) = parser.parse_query(&query) {
                    panic!("rewritten query is not valid SPARQL 1.1: {e}\n{query}");
                }
                query
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn test_query_without_named_subqueries_is_unchanged() {
        let query = "SELECT ?with WHERE { ?with wdt:P31 wd:Q5 } # WITH { } AS %x";
        assert_eq!(
            rewrite_named_subqueries(query),
            NamedSubqueryRewrite::Unchanged
        );
    }

    #[test]
    fn test_named_subquery_is_inlined() {
        let query = "SELECT ?item ?label
WITH {
  SELECT ?item WHERE { ?item wdt:P31 wd:Q146 } LIMIT 10
} AS %cats
WHERE {
  INCLUDE %cats
  ?item rdfs:label ?label .
}";
        assert_eq!(
            rewritten(query),
            "SELECT ?item ?label

WHERE {
  {
  SELECT ?item WHERE { ?item wdt:P31 wd:Q146 } LIMIT 10
}
  ?item rdfs:label ?label .
}"
        );
    }

    #[test]
    fn test_nested_and_repeated_includes() {
        let query = "SELECT * WITH { SELECT ?a WHERE { ?a wdt:P31 wd:Q5 } } AS %a
            WITH { SELECT ?a ?b WHERE { INCLUDE %a ?a wdt:P27 ?b } } AS %b
            WHERE { INCLUDE %b INCLUDE %a }";
        let query = rewritten(query);
        assert!(
            !query.contains("INCLUDE") && !query.contains("WITH"),
            "{query}"
        );
        assert_eq!(query.matches("wdt:P31").count(), 2, "{query}");
    }

    #[test]
    fn test_unsupported_named_subqueries() {
        for (query, reason) in [
            ("SELECT * WHERE { INCLUDE %x }", UnsupportedNamedSubquery::Undefined("x".into())),
            (
                "SELECT * WITH { SELECT * WHERE { INCLUDE %x } } AS %x WHERE { INCLUDE %x }",
                UnsupportedNamedSubquery::Ambiguous("x".into()),
            ),
            (
                "SELECT * WITH { SELECT * {} } AS %x WITH { SELECT * {} } AS %x WHERE { INCLUDE %x }",
                UnsupportedNamedSubquery::Ambiguous("x".into()),
            ),
            ("SELECT * WITH { SELECT * {} } WHERE { }", UnsupportedNamedSubquery::Syntax),
            ("SELECT * WITH { SELECT * { } AS %x WHERE { }", UnsupportedNamedSubquery::Syntax),
        ] {
            assert_eq!(
                rewrite_named_subqueries(query),
                NamedSubqueryRewrite::Unsupported(reason),
                "{query}"
            );
        }
    }

    #[test]
    fn test_reason_is_readable() {
        assert_eq!(
            UnsupportedNamedSubquery::Undefined("x".into()).to_string(),
            "named subquery %x is not defined"
        );
    }

    #[test]
    fn test_label_service_rewrite_follows() {
        use crate::sparql_rewrite::{rewrite_label_service, LabelServiceRewrite};
        let query = "SELECT ?item ?itemLabel
WITH { SELECT ?item WHERE { ?item wdt:P31 wd:Q146 } LIMIT 5 } AS %cats
WHERE { INCLUDE %cats SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\". } }";
        let inlined = rewritten(query);
        match rewrite_label_service(&inlined, |_| true) {
            LabelServiceRewrite::Rewritten(query) => assert!(!query.contains("SERVICE"), "{query}"),
            other => panic!("got {other:?}"),
        }
    }
}
