//! Building blocks for SPARQL queries against Wikidata: entity terms and
//! `VALUES` clauses, string literals, and label/rank patterns that run on
//! both WDQS (Blazegraph) and QLever.
//!
//! Variables may be given with or without their `?`. Everything spliced into
//! a query here is either a typed entity ID or escaped, so untrusted input
//! cannot break out of the generated fragment.

use crate::entity_id::{EntityId, COMMONS_ENTITY_PREFIX};
use std::fmt::Write;

/// The term for an entity: `wd:Q42`, or the full IRI for MediaInfo entities,
/// which live on Commons rather than under the `wd:` prefix.
pub fn entity_term(id: impl Into<EntityId>) -> String {
    match id.into() {
        id @ EntityId::MediaInfo(_) => format!("<{COMMONS_ENTITY_PREFIX}{id}>"),
        id => format!("wd:{id}"),
    }
}

/// The IRI WDQS uses for a Commons media value (P18 and the like):
/// `http://commons.wikimedia.org/wiki/Special:FilePath/<name>`, with the name
/// in its space form (underscores read as spaces), percent-encoded like PHP's
/// `rawurlencode` — `Foo bar (1).jpg` → `…/Foo%20bar%20%281%29.jpg`.
pub fn commons_media_iri(file: &str) -> String {
    format!(
        "{COMMONS_FILE_PATH_PREFIX}{}",
        urlencoding::encode(&file.trim().replace('_', " "))
    )
}

/// [`commons_media_iri`] as a SPARQL term, `<…>`, for comparing with or
/// binding a Commons media value.
pub fn commons_media_term(file: &str) -> String {
    format!("<{}>", commons_media_iri(file))
}

/// The prefix of [`commons_media_iri`].
pub const COMMONS_FILE_PATH_PREFIX: &str = "http://commons.wikimedia.org/wiki/Special:FilePath/";

/// `VALUES ?item { wd:Q1 wd:Q2 }`. Without IDs the clause has no rows, so the
/// query matches nothing.
pub fn values_clause<I>(variable: &str, ids: I) -> String
where
    I: IntoIterator,
    I::Item: Into<EntityId>,
{
    let terms: Vec<String> = ids.into_iter().map(entity_term).collect();
    if terms.is_empty() {
        format!("VALUES {} {{ }}", var(variable))
    } else {
        format!("VALUES {} {{ {} }}", var(variable), terms.join(" "))
    }
}

/// `s` as a double-quoted SPARQL string literal, escaped.
pub fn string_literal(s: &str) -> String {
    let mut literal = String::with_capacity(s.len() + 2);
    literal.push('"');
    for c in s.chars() {
        match c {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            c => literal.push(c),
        }
    }
    literal.push('"');
    literal
}

/// A pattern binding `?out` to `?item`'s label in the first of `languages` it
/// has, then the multilingual (`mul`) label, else the entity ID — what
/// `SERVICE wikibase:label` does, in standard SPARQL (QLever has no label
/// service). `mul` is tried after the given languages unless listed among
/// them: many items now carry their name only there. Place the pattern in the
/// group where `?item` is bound.
///
/// Language codes are lowercased; one that is not a well-formed language tag
/// is skipped rather than spliced into the query.
pub fn label_pattern(item_var: &str, out_var: &str, languages: &[&str]) -> String {
    let item = var(item_var);
    let out = var(out_var);
    let mut tags: Vec<String> = Vec::new();
    for tag in languages
        .iter()
        .map(|language| language.trim().to_lowercase())
        .chain(["mul".to_string()])
        .filter(|tag| is_language_tag(tag))
    {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    let mut pattern = String::new();
    let mut candidates = Vec::with_capacity(tags.len() + 1);
    for (i, tag) in tags.iter().enumerate() {
        let candidate = format!("{out}_{i}");
        let _ = write!(
            pattern,
            "OPTIONAL {{ {item} rdfs:label {candidate} FILTER(LANG({candidate})=\"{tag}\") }} "
        );
        candidates.push(candidate);
    }
    candidates.push(format!("STRAFTER(STR({item}), \"/entity/\")"));
    let _ = write!(
        pattern,
        "BIND(COALESCE({}) AS {out})",
        candidates.join(", ")
    );
    pattern
}

/// Excludes deprecated statements bound to `?statement`. A `MINUS`, because
/// QLever silently matches nothing for `FILTER(?rank !=
/// wikibase:DeprecatedRank)` and `FILTER(?rank IN (…))` and lacks `sameTerm`;
/// this form gives the same rows on both engines.
pub fn not_deprecated(statement_var: &str) -> String {
    format!(
        "MINUS {{ {} wikibase:rank wikibase:DeprecatedRank }}",
        var(statement_var)
    )
}

/// `?name`, whether or not `name` came with its `?` (or `$`).
fn var(name: &str) -> String {
    let name = name.trim();
    let name = name
        .strip_prefix('?')
        .or_else(|| name.strip_prefix('$'))
        .unwrap_or(name);
    format!("?{name}")
}

/// A BCP 47-shaped tag: letters, then `-`-separated alphanumeric subtags.
fn is_language_tag(tag: &str) -> bool {
    let mut subtags = tag.split('-');
    let primary_ok = subtags
        .next()
        .is_some_and(|s| (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphabetic()));
    primary_ok
        && subtags
            .all(|s| (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {

    #[test]
    fn commons_media_terms_match_wdqs() {
        // As WDQS returns Q937's P18.
        assert_eq!(
            commons_media_term("Albert Einstein Head cleaned.jpg"),
            "<http://commons.wikimedia.org/wiki/Special:FilePath/Albert%20Einstein%20Head%20cleaned.jpg>"
        );
        assert_eq!(
            commons_media_iri("Foo_bar_(1).jpg"),
            "http://commons.wikimedia.org/wiki/Special:FilePath/Foo%20bar%20%281%29.jpg"
        );
        assert_eq!(
            commons_media_term("a>b"),
            "<http://commons.wikimedia.org/wiki/Special:FilePath/a%3Eb>"
        );
    }
    use super::*;
    use crate::entity_id::{ItemId, MediaInfoId};

    #[test]
    fn test_values_clause() {
        assert_eq!(
            values_clause("item", [ItemId::new(1), ItemId::new(2)]),
            "VALUES ?item { wd:Q1 wd:Q2 }"
        );
        let ids: Vec<EntityId> = vec!["P31".parse().unwrap(), "L1-F2".parse().unwrap()];
        assert_eq!(values_clause("?x", ids), "VALUES ?x { wd:P31 wd:L1-F2 }");
        assert_eq!(
            values_clause("item", Vec::<ItemId>::new()),
            "VALUES ?item { }"
        );
        assert_eq!(
            entity_term(MediaInfoId::new(5)),
            "<https://commons.wikimedia.org/entity/M5>"
        );
    }

    #[test]
    fn test_string_literal() {
        assert_eq!(string_literal("plain"), "\"plain\"");
        assert_eq!(string_literal("a \"b\" \\ c\nd"), r#""a \"b\" \\ c\nd""#);
    }

    #[test]
    fn test_label_pattern() {
        assert_eq!(
            label_pattern("item", "?label", &["de", "en"]),
            "OPTIONAL { ?item rdfs:label ?label_0 FILTER(LANG(?label_0)=\"de\") } \
             OPTIONAL { ?item rdfs:label ?label_1 FILTER(LANG(?label_1)=\"en\") } \
             OPTIONAL { ?item rdfs:label ?label_2 FILTER(LANG(?label_2)=\"mul\") } \
             BIND(COALESCE(?label_0, ?label_1, ?label_2, STRAFTER(STR(?item), \"/entity/\")) AS ?label)"
        );
        // `mul` keeps its place when listed; duplicates and bad tags drop out.
        let pattern = label_pattern("i", "l", &["mul", "EN", "en", "x\") }"]);
        assert!(pattern.contains("?l_0)=\"mul\""), "{pattern}");
        assert!(pattern.contains("?l_1)=\"en\""), "{pattern}");
        assert!(!pattern.contains("?l_2"), "{pattern}");
        assert!(!pattern.contains("x\""), "{pattern}");
    }

    #[test]
    fn test_not_deprecated() {
        assert_eq!(
            not_deprecated("statement"),
            "MINUS { ?statement wikibase:rank wikibase:DeprecatedRank }"
        );
    }

    #[test]
    fn test_language_tags() {
        for good in ["en", "de-ch", "zh-hans", "sr-el", "be-tarask"] {
            assert!(is_language_tag(good), "{good}");
        }
        for bad in ["", "-en", "en-", "e n", "en\"", "1en"] {
            assert!(!is_language_tag(bad), "{bad}");
        }
    }
}
