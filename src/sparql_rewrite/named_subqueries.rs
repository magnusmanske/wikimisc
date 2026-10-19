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
//!
//! Inlining copies a subquery to every place it is included, so a query whose
//! inlined form would be far longer than the original, or nest subqueries
//! very deeply, is refused rather than built.

use crate::sparql_lexer::{tokenize, Brackets, Token};
use std::collections::HashMap;
use std::fmt;

/// How deeply named subqueries may include one another. Real queries nest a
/// handful of levels; the limit keeps the recursion that inlines them far
/// from the end of the stack.
const MAX_DEPTH: usize = 64;

/// The inlined query may be this many times as long as the original, or
/// [`MIN_LENGTH_LIMIT`] bytes, whichever is more. Subqueries that each include
/// the previous one twice double the query with every link; real queries
/// grow far less.
const MAX_GROWTH: usize = 16;

/// See [`MAX_GROWTH`].
const MIN_LENGTH_LIMIT: usize = 1 << 20;

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
    /// The named subquery is defined inside another one.
    Nested(String),
    /// `INCLUDE %name JOIN ON (…)`: Blazegraph's join hint.
    JoinHint(String),
    /// Inlining would make the query too long, or nest subqueries too deeply.
    TooLarge,
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
            Self::Nested(name) => {
                write!(f, "named subquery %{name} is defined inside another one")
            }
            Self::JoinHint(name) => write!(f, "INCLUDE %{name} has a JOIN ON hint"),
            Self::TooLarge => f.write_str(
                "inlining the named subqueries would make the query too long or too deeply nested",
            ),
        }
    }
}

/// Inlines Blazegraph's named subqueries (`WITH { … } AS %name` and
/// `INCLUDE %name`) as SPARQL 1.1 subqueries, splicing the query text.
pub fn rewrite_named_subqueries(sparql: &str) -> NamedSubqueryRewrite {
    match inline(sparql) {
        Ok(Some(query)) => NamedSubqueryRewrite::Rewritten(query),
        Ok(None) => NamedSubqueryRewrite::Unchanged,
        Err(reason) => NamedSubqueryRewrite::Unsupported(reason),
    }
}

type Result<T> = std::result::Result<T, UnsupportedNamedSubquery>;

/// The query with its named subqueries inlined, or `None` if it has none.
fn inline(sparql: &str) -> Result<Option<String>> {
    let tokens = tokenize(sparql);
    let includes = includes(&tokens)?;
    let definitions_present = tokens
        .windows(2)
        .any(|pair| pair[0].is_keyword("WITH") && pair[1].is_punct('{'));
    if includes.is_empty() && !definitions_present {
        return Ok(None);
    }
    Inliner::new(sparql, &tokens, includes)?.rewrite().map(Some)
}

/// An `INCLUDE %name`: its name, and the byte range to replace.
struct Include<'a> {
    name: &'a str,
    start: usize,
    end: usize,
}

/// A `WITH { body } AS %name`.
struct Definition<'a> {
    /// The byte range of the body, inside the braces.
    body: (usize, usize),
    /// The byte range of the whole definition.
    whole: (usize, usize),
    /// The includes in the body, in query order.
    includes: Vec<Include<'a>>,
}

/// What inlining a named subquery makes of it.
#[derive(Debug, Clone, Copy)]
struct Inlined {
    /// The length of `{ body }`, with the includes in the body inlined.
    len: usize,
    /// How many named subqueries nest in it, itself included.
    depth: usize,
}

struct Inliner<'a> {
    sparql: &'a str,
    /// In query order. They do not overlap.
    definitions: Vec<Definition<'a>>,
    /// Indices into `definitions`.
    by_name: HashMap<&'a str, usize>,
    /// The includes outside every definition, in query order.
    includes: Vec<Include<'a>>,
    /// The longest the inlined query may be.
    max_len: usize,
}

impl<'a> Inliner<'a> {
    fn new(sparql: &'a str, tokens: &[Token<'a>], includes: Vec<Include<'a>>) -> Result<Self> {
        let brackets = Brackets::new(tokens).ok_or(UnsupportedNamedSubquery::Syntax)?;
        let mut definitions: Vec<Definition<'a>> = Vec::new();
        let mut by_name = HashMap::new();
        for (i, token) in tokens.iter().enumerate() {
            let Some(open) = tokens.get(i + 1).filter(|t| t.is_punct('{')) else {
                continue;
            };
            if !token.is_keyword("WITH") {
                continue;
            }
            let close = brackets
                .partner(i + 1)
                .ok_or(UnsupportedNamedSubquery::Syntax)?;
            let (body_end, name, end) = match tokens.get(close..close + 4) {
                Some([close, as_, percent, name])
                    if as_.is_keyword("AS") && percent.is_punct('%') =>
                {
                    (close.start, name.text, name.end())
                }
                _ => return Err(UnsupportedNamedSubquery::Syntax),
            };
            // Definitions are found in query order, so one inside another
            // starts before the previous one ends.
            if definitions.last().is_some_and(|d| token.start < d.whole.1) {
                return Err(UnsupportedNamedSubquery::Nested(name.to_string()));
            }
            if by_name.insert(name, definitions.len()).is_some() {
                return Err(UnsupportedNamedSubquery::Ambiguous(name.to_string()));
            }
            definitions.push(Definition {
                body: (open.end(), body_end),
                whole: (token.start, end),
                includes: Vec::new(),
            });
        }
        let mut top_level = Vec::new();
        for include in includes {
            // The last definition starting before the include.
            let before = definitions.partition_point(|d| d.whole.0 <= include.start);
            match before.checked_sub(1).and_then(|i| definitions.get_mut(i)) {
                Some(definition) if include.start < definition.whole.1 => {
                    if include.end > definition.body.1 {
                        return Err(UnsupportedNamedSubquery::Syntax);
                    }
                    definition.includes.push(include);
                }
                _ => top_level.push(include),
            }
        }
        Ok(Self {
            sparql,
            definitions,
            by_name,
            includes: top_level,
            max_len: sparql
                .len()
                .saturating_mul(MAX_GROWTH)
                .max(MIN_LENGTH_LIMIT),
        })
    }

    /// The query without its definitions, every include replaced by the
    /// subquery it names. Its length is worked out first, so that a query
    /// that would be too long is never built.
    fn rewrite(&self) -> Result<String> {
        let mut inlined = HashMap::new();
        let mut len = self.sparql.len();
        for definition in &self.definitions {
            len = len.saturating_sub(span_len(definition.whole));
        }
        for include in &self.includes {
            let subquery = self.measure(include.name, &mut Vec::new(), &mut inlined)?;
            len = len
                .saturating_sub(span_len((include.start, include.end)))
                .saturating_add(subquery.len);
        }
        if len > self.max_len {
            return Err(UnsupportedNamedSubquery::TooLarge);
        }
        let mut query = String::with_capacity(len);
        let mut kept_from = 0;
        let mut includes = self.includes.as_slice();
        for definition in &self.definitions {
            let (before, after) =
                includes.split_at(includes.partition_point(|i| i.start < definition.whole.0));
            self.write(&mut query, (kept_from, definition.whole.0), before);
            includes = after;
            kept_from = definition.whole.1;
        }
        self.write(&mut query, (kept_from, self.sparql.len()), includes);
        Ok(query)
    }

    /// What inlining the named subquery makes of it, checking that it is
    /// defined, does not include itself, and is neither too long nor nested
    /// too deeply. `path` holds the subqueries being measured, which include
    /// this one; `inlined` those already measured.
    fn measure(
        &self,
        name: &'a str,
        path: &mut Vec<&'a str>,
        inlined: &mut HashMap<&'a str, Inlined>,
    ) -> Result<Inlined> {
        if let Some(&measured) = inlined.get(name) {
            return Ok(measured);
        }
        let definition = self.definition(name)?;
        if path.contains(&name) {
            return Err(UnsupportedNamedSubquery::Ambiguous(name.to_string()));
        }
        if path.len() >= MAX_DEPTH {
            return Err(UnsupportedNamedSubquery::TooLarge);
        }
        path.push(name);
        let mut measured = Inlined {
            len: span_len(definition.body).saturating_add(2),
            depth: 1,
        };
        for include in &definition.includes {
            let subquery = self.measure(include.name, path, inlined)?;
            measured.len = measured
                .len
                .saturating_sub(span_len((include.start, include.end)))
                .saturating_add(subquery.len);
            measured.depth = measured.depth.max(subquery.depth.saturating_add(1));
        }
        path.pop();
        if measured.len > self.max_len || measured.depth > MAX_DEPTH {
            return Err(UnsupportedNamedSubquery::TooLarge);
        }
        inlined.insert(name, measured);
        Ok(measured)
    }

    /// Appends `sparql[range]` to `query`, with `includes` (inside the range,
    /// in order) replaced by `{ body }` of the subquery they name. Only
    /// called once every include has been measured, so each names a subquery
    /// and the recursion is at most [`MAX_DEPTH`] deep.
    fn write(&self, query: &mut String, (start, end): (usize, usize), includes: &[Include<'a>]) {
        let mut kept_from = start;
        for include in includes {
            query.push_str(
                self.sparql
                    .get(kept_from..include.start)
                    .unwrap_or_default(),
            );
            if let Ok(definition) = self.definition(include.name) {
                query.push('{');
                self.write(query, definition.body, &definition.includes);
                query.push('}');
            }
            kept_from = include.end;
        }
        query.push_str(self.sparql.get(kept_from..end).unwrap_or_default());
    }

    fn definition(&self, name: &str) -> Result<&Definition<'a>> {
        self.by_name
            .get(name)
            .and_then(|&i| self.definitions.get(i))
            .ok_or_else(|| UnsupportedNamedSubquery::Undefined(name.to_string()))
    }
}

/// Every `INCLUDE %name` in the query, in order. They do not overlap.
fn includes<'a>(tokens: &[Token<'a>]) -> Result<Vec<Include<'a>>> {
    let mut includes = Vec::new();
    let mut i = 0;
    while let Some(window) = tokens.get(i..i + 3) {
        match window {
            [include, percent, name] if include.is_keyword("INCLUDE") && percent.is_punct('%') => {
                if tokens.get(i + 3).is_some_and(|t| t.is_keyword("JOIN")) {
                    return Err(UnsupportedNamedSubquery::JoinHint(name.text.to_string()));
                }
                includes.push(Include {
                    name: name.text,
                    start: include.start,
                    end: name.end(),
                });
                i += 3;
            }
            _ => i += 1,
        }
    }
    Ok(includes)
}

/// The length of a byte range.
fn span_len((start, end): (usize, usize)) -> usize {
    end.saturating_sub(start)
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

    /// `n` named subqueries `%s1` … `%sn`, each including the one before it
    /// `per_link` times, and a query including the last one.
    fn chain(n: usize, per_link: usize) -> String {
        let mut query =
            "SELECT ?a WITH { SELECT ?a WHERE { ?a wdt:P31 wd:Q5 } } AS %s0\n".to_string();
        for k in 1..=n {
            let includes = format!("INCLUDE %s{} ", k - 1).repeat(per_link);
            query.push_str(&format!(
                "WITH {{ SELECT ?a WHERE {{ {includes}}} }} AS %s{k}\n"
            ));
        }
        query.push_str(&format!("WHERE {{ INCLUDE %s{n} }}"));
        query
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
            (
                "SELECT * WITH { SELECT ?a WITH { SELECT ?a { ?a ?b ?c } } AS %in WHERE { INCLUDE %in } } AS %out WHERE { INCLUDE %out }",
                UnsupportedNamedSubquery::Nested("in".into()),
            ),
            (
                "SELECT * WITH { SELECT ?a { ?a ?b ?c } } AS %x WHERE { INCLUDE %x JOIN ON (?a) }",
                UnsupportedNamedSubquery::JoinHint("x".into()),
            ),
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

    #[test]
    fn test_doubling_includes_are_inlined_while_small() {
        let query = rewritten(&chain(5, 2));
        assert_eq!(query.matches("wdt:P31").count(), 32, "{query}");
    }

    #[test]
    fn test_exponential_inlining_is_refused() {
        // Each link doubles the inlined query: 22 links of a 1.3 KB query
        // came to 117 MB, 30 to some 30 GB. Refused before building any of it.
        for n in [22, 40, 1000] {
            assert_eq!(
                rewrite_named_subqueries(&chain(n, 2)),
                NamedSubqueryRewrite::Unsupported(UnsupportedNamedSubquery::TooLarge),
                "{n} links"
            );
        }
    }

    #[test]
    fn test_moderately_deep_includes_are_inlined() {
        let query = rewritten(&chain(MAX_DEPTH - 2, 1));
        assert!(!query.contains("INCLUDE"), "{query}");
    }

    #[test]
    fn test_deep_includes_do_not_overflow_the_stack() {
        // Recursing once per link, 8000 links overflowed a 2 MiB stack.
        const LINKS: usize = 20_000;
        let top_down = chain(LINKS, 1);
        // Measured bottom up: every subquery is included at the top level,
        // the shallowest first.
        let bottom_up = top_down.replace(
            &format!("WHERE {{ INCLUDE %s{LINKS} }}"),
            &format!(
                "WHERE {{ {} }}",
                (0..=LINKS)
                    .map(|k| format!("INCLUDE %s{k}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        );
        let outcomes = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || [top_down, bottom_up].map(|query| rewrite_named_subqueries(&query)))
            .unwrap()
            .join()
            .unwrap();
        for outcome in outcomes {
            assert_eq!(
                outcome,
                NamedSubqueryRewrite::Unsupported(UnsupportedNamedSubquery::TooLarge)
            );
        }
    }

    #[test]
    fn test_include_named_include_does_not_overlap() {
        // Not valid SPARQL, but must not splice overlapping ranges.
        let query = "SELECT * WITH { SELECT ?a { ?a ?b ?c } } AS %x WHERE { INCLUDE %INCLUDE %x }";
        assert_eq!(
            rewrite_named_subqueries(query),
            NamedSubqueryRewrite::Unsupported(UnsupportedNamedSubquery::Undefined(
                "INCLUDE".into()
            ))
        );
    }
}
