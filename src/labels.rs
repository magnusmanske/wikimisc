//! Labels and descriptions with language fallback, and statements by rank —
//! for `wikibase` entities and for raw entity JSON (as `wbgetentities` and the
//! dumps return it) alike.
//!
//! Which fallback languages make sense differs per tool, so the functions take
//! the languages to try in order; [`fallback_languages`] builds such a list
//! from a user's language and [`DEFAULT_FALLBACKS`].
//!
//! The language helpers and the JSON functions need nothing but `serde_json`.
//! Those taking `wikibase` types (`label_in`, `description_in`,
//! `label_or_any`, `best_statements`, `RankFilter::apply`, `rank_from_uri`)
//! are there when the `wikibase` feature is enabled too.

use serde_json::Value;
use std::fmt::Display;
#[cfg(feature = "wikibase")]
use wikibase::{EntityTrait, LocaleString, Statement, StatementRank};

/// Languages worth trying after the user's own: `mul` (Wikidata's
/// "multiple languages" label, e.g. for names) and English.
pub const DEFAULT_FALLBACKS: &[&str] = &["mul", "en"];

/// The base language of a language code: trimmed, lowercased, and a region
/// subtag (two letters or three digits) removed — `de-DE`, `de_DE` and `DE`
/// are all `de`, `es-419` is `es`, while `zh-hans` and `be-tarask` stay as
/// they are. `None` for an empty code.
pub fn base_language(code: &str) -> Option<String> {
    let code = code.trim().to_lowercase().replace('_', "-");
    if code.is_empty() {
        return None;
    }
    let base = match code.rsplit_once('-') {
        Some((base, region)) if !base.is_empty() && is_region(region) => base.to_string(),
        _ => code,
    };
    Some(base)
}

fn is_region(subtag: &str) -> bool {
    (subtag.len() == 2 && subtag.bytes().all(|b| b.is_ascii_alphabetic()))
        || (subtag.len() == 3 && subtag.bytes().all(|b| b.is_ascii_digit()))
}

/// `language` followed by `fallbacks`, without duplicates and empty entries:
/// `fallback_languages("en", DEFAULT_FALLBACKS)` is `["en", "mul"]`.
pub fn fallback_languages(language: &str, fallbacks: &[&str]) -> Vec<String> {
    let mut languages: Vec<String> = Vec::with_capacity(fallbacks.len() + 1);
    for language in std::iter::once(language).chain(fallbacks.iter().copied()) {
        let language = language.trim();
        if !language.is_empty() && !languages.iter().any(|l| l == language) {
            languages.push(language.to_string());
        }
    }
    languages
}

#[cfg(feature = "wikibase")]
/// The text of the first of `languages` that has one.
fn term_in<'a>(terms: &'a [LocaleString], languages: &[&str]) -> Option<&'a str> {
    languages.iter().find_map(|language| {
        terms
            .iter()
            .find(|term| term.language() == *language)
            .map(|term| term.value())
    })
}

#[cfg(feature = "wikibase")]
/// The term of the alphabetically first language, so that "any" is the same
/// one on every call.
fn any_term(terms: &[LocaleString]) -> Option<&str> {
    terms
        .iter()
        .min_by(|a, b| a.language().cmp(b.language()))
        .map(|term| term.value())
}

#[cfg(feature = "wikibase")]
/// The label in the first of `languages` the entity has one in.
pub fn label_in<'a, E: EntityTrait + ?Sized>(entity: &'a E, languages: &[&str]) -> Option<&'a str> {
    term_in(entity.labels(), languages)
}

#[cfg(feature = "wikibase")]
/// The description in the first of `languages` the entity has one in.
pub fn description_in<'a, E: EntityTrait + ?Sized>(
    entity: &'a E,
    languages: &[&str],
) -> Option<&'a str> {
    term_in(entity.descriptions(), languages)
}

#[cfg(feature = "wikibase")]
/// As [`label_in`], falling back to a label in any language (the
/// alphabetically first language code, for a stable choice).
pub fn label_or_any<'a, E: EntityTrait + ?Sized>(
    entity: &'a E,
    languages: &[&str],
) -> Option<&'a str> {
    label_in(entity, languages).or_else(|| any_term(entity.labels()))
}

/// The `labels` or `descriptions` (`field`) text of entity JSON in the first
/// of `languages` that has one.
pub fn json_term<'a>(entity: &'a Value, field: &str, languages: &[&str]) -> Option<&'a str> {
    let terms = entity.get(field)?.as_object()?;
    languages
        .iter()
        .find_map(|language| terms.get(*language)?.get("value")?.as_str())
}

/// As [`json_term`], falling back to the term in any language (the
/// alphabetically first language code, for a stable choice).
pub fn json_term_or_any<'a>(entity: &'a Value, field: &str, languages: &[&str]) -> Option<&'a str> {
    json_term(entity, field, languages).or_else(|| {
        entity
            .get(field)?
            .as_object()?
            .iter()
            .filter_map(|(language, term)| Some((language, term.get("value")?.as_str()?)))
            .min_by(|a, b| a.0.cmp(b.0))
            .map(|(_, value)| value)
    })
}

/// Which statements of a property count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RankFilter {
    /// All of them.
    Any,
    /// All but deprecated ones.
    NonDeprecated,
    /// The "truthy" ones, as the Query Service's `wdt:` sees them: the
    /// preferred statements if there are any, otherwise the normal ones.
    #[default]
    Best,
}

impl RankFilter {
    /// The statements that pass, in their original order.
    #[cfg(feature = "wikibase")]
    pub fn apply<'a>(
        self,
        statements: impl IntoIterator<Item = &'a Statement>,
    ) -> Vec<&'a Statement> {
        self.select(statements, |statement| match statement.rank() {
            StatementRank::Preferred => Rank::Preferred,
            StatementRank::Normal => Rank::Normal,
            StatementRank::Deprecated => Rank::Deprecated,
        })
    }

    /// The same for statements as JSON (a missing or unknown `rank` counts
    /// as normal).
    pub fn apply_json<'a>(self, statements: impl IntoIterator<Item = &'a Value>) -> Vec<&'a Value> {
        self.select(statements, json_rank)
    }

    fn select<T>(
        self,
        statements: impl IntoIterator<Item = T>,
        rank: impl Fn(&T) -> Rank,
    ) -> Vec<T> {
        let statements = statements.into_iter();
        match self {
            Self::Any => statements.collect(),
            Self::NonDeprecated => statements.filter(|s| rank(s) != Rank::Deprecated).collect(),
            Self::Best => {
                let (preferred, normal): (Vec<T>, Vec<T>) = statements
                    .filter(|s| rank(s) != Rank::Deprecated)
                    .partition(|s| rank(s) == Rank::Preferred);
                if preferred.is_empty() {
                    normal
                } else {
                    preferred
                }
            }
        }
    }
}

#[cfg(feature = "wikibase")]
/// The best-ranked statements of `property` (`"P18"` or a `PropertyId`): the
/// preferred ones if there are any, otherwise the normal ones.
pub fn best_statements<E: EntityTrait + ?Sized>(
    entity: &E,
    property: impl Display,
) -> Vec<&Statement> {
    let property = property.to_string();
    RankFilter::Best.apply(entity.claims().iter().filter(|s| s.property() == property))
}

/// The statements of `property` in entity JSON (`claims` or, for MediaInfo,
/// `statements`), in their original order.
pub fn json_statements(entity: &Value, property: impl Display) -> &[Value] {
    let property = property.to_string();
    ["claims", "statements"]
        .iter()
        .find_map(|key| entity.get(key)?.get(&property)?.as_array())
        .map_or(&[], Vec::as_slice)
}

/// The best-ranked statements of `property` in entity JSON: the preferred
/// ones if there are any, otherwise the normal ones.
pub fn best_statements_json(entity: &Value, property: impl Display) -> Vec<&Value> {
    RankFilter::Best.apply_json(json_statements(entity, property))
}

#[cfg(feature = "wikibase")]
/// The rank of a statement as the RDF/SPARQL rank IRI names it
/// (`http://wikiba.se/ontology#PreferredRank`; the `wikibase:PreferredRank`
/// prefixed form too).
pub fn rank_from_uri(uri: &str) -> Option<StatementRank> {
    let name = uri
        .strip_prefix("http://wikiba.se/ontology#")
        .or_else(|| uri.strip_prefix("wikibase:"))?;
    match name {
        "PreferredRank" => Some(StatementRank::Preferred),
        "NormalRank" => Some(StatementRank::Normal),
        "DeprecatedRank" => Some(StatementRank::Deprecated),
        _ => None,
    }
}

/// A statement's rank, independent of the `wikibase` types.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rank {
    Preferred,
    Normal,
    Deprecated,
}

fn json_rank(statement: &&Value) -> Rank {
    match statement.get("rank").and_then(Value::as_str) {
        Some("preferred") => Rank::Preferred,
        Some("deprecated") => Rank::Deprecated,
        _ => Rank::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[cfg(feature = "wikibase")]
    use wikibase::{ItemEntity, Snak};

    #[cfg(feature = "wikibase")]
    fn item() -> ItemEntity {
        let mut item = ItemEntity::new_empty();
        item.labels_mut()
            .push(LocaleString::new("en", "Douglas Adams"));
        item.labels_mut()
            .push(LocaleString::new("de", "Douglas Adams (de)"));
        item.labels_mut()
            .push(LocaleString::new("bg", "Дъглас Адамс"));
        item.descriptions_mut()
            .push(LocaleString::new("en", "English writer"));
        item
    }

    #[cfg(feature = "wikibase")]
    fn statement(value: &str, rank: StatementRank) -> Statement {
        let mut statement = Statement::new_normal(Snak::new_string("P1", value), vec![], vec![]);
        statement.set_rank(rank);
        statement
    }

    #[test]
    fn base_languages() {
        assert_eq!(base_language("de-DE").as_deref(), Some("de"));
        assert_eq!(base_language(" DE_de ").as_deref(), Some("de"));
        assert_eq!(base_language("es-419").as_deref(), Some("es"));
        assert_eq!(base_language("zh-hans").as_deref(), Some("zh-hans"));
        assert_eq!(base_language("be-tarask").as_deref(), Some("be-tarask"));
        assert_eq!(base_language("en").as_deref(), Some("en"));
        assert_eq!(base_language("  "), None);
    }

    #[test]
    fn fallback_lists() {
        assert_eq!(
            fallback_languages("de", DEFAULT_FALLBACKS),
            ["de", "mul", "en"]
        );
        assert_eq!(fallback_languages("en", DEFAULT_FALLBACKS), ["en", "mul"]);
        assert_eq!(fallback_languages("", &["en", "en"]), ["en"]);
    }

    #[test]
    #[cfg(feature = "wikibase")]
    fn entity_terms() {
        let item = item();
        assert_eq!(
            label_in(&item, &["fr", "de", "en"]),
            Some("Douglas Adams (de)")
        );
        assert_eq!(label_in(&item, &["fr"]), None);
        assert_eq!(label_or_any(&item, &["fr"]), Some("Дъглас Адамс"));
        assert_eq!(description_in(&item, &["de", "en"]), Some("English writer"));
        assert_eq!(label_or_any(&ItemEntity::new_empty(), &["en"]), None);
    }

    #[test]
    fn json_terms() {
        let entity = json!({
            "labels": {
                "en": {"language": "en", "value": "Douglas Adams"},
                "bg": {"language": "bg", "value": "Дъглас Адамс"}
            },
            "descriptions": {}
        });
        assert_eq!(
            json_term(&entity, "labels", &["de", "en"]),
            Some("Douglas Adams")
        );
        assert_eq!(json_term(&entity, "labels", &["de"]), None);
        assert_eq!(
            json_term_or_any(&entity, "labels", &["de"]),
            Some("Дъглас Адамс")
        );
        assert_eq!(json_term_or_any(&entity, "descriptions", &["de"]), None);
        assert_eq!(json_term(&json!({}), "labels", &["en"]), None);
    }

    #[test]
    #[cfg(feature = "wikibase")]
    fn rank_filters() {
        let statements = [
            statement("normal", StatementRank::Normal),
            statement("deprecated", StatementRank::Deprecated),
            statement("preferred", StatementRank::Preferred),
        ];
        let values = |filter: RankFilter| -> Vec<String> {
            filter
                .apply(&statements)
                .iter()
                .map(|s| format!("{:?}", s.main_snak().data_value()))
                .collect()
        };
        assert_eq!(values(RankFilter::Any).len(), 3);
        assert_eq!(values(RankFilter::NonDeprecated).len(), 2);
        let best = RankFilter::Best.apply(&statements);
        assert_eq!(best.len(), 1);
        assert_eq!(*best[0].rank(), StatementRank::Preferred);
        // Without a preferred statement, the normal ones are best.
        let best = RankFilter::Best.apply(&statements[..2]);
        assert_eq!(best.len(), 1);
        assert_eq!(*best[0].rank(), StatementRank::Normal);
    }

    #[test]
    #[cfg(feature = "wikibase")]
    fn best_statements_of_an_entity() {
        let mut item = item();
        item.add_claim(statement("a", StatementRank::Normal));
        item.add_claim(statement("b", StatementRank::Normal));
        item.add_claim(Statement::new_normal(
            Snak::new_string("P2", "c"),
            vec![],
            vec![],
        ));
        assert_eq!(best_statements(&item, "P1").len(), 2);
        assert_eq!(best_statements(&item, "P2").len(), 1);
        assert!(best_statements(&item, "P3").is_empty());
    }

    #[test]
    fn json_statements_by_rank() {
        let entity = json!({"claims": {"P18": [
            {"rank": "deprecated", "mainsnak": {"datavalue": {"value": "Old.jpg"}}},
            {"rank": "normal", "mainsnak": {"datavalue": {"value": "A.jpg"}}},
            {"mainsnak": {"datavalue": {"value": "B.jpg"}}}
        ]}});
        assert_eq!(json_statements(&entity, "P18").len(), 3);
        let best = best_statements_json(&entity, "P18");
        assert_eq!(best.len(), 2);
        assert_eq!(best[0]["mainsnak"]["datavalue"]["value"], "A.jpg");
        assert!(json_statements(&entity, "P31").is_empty());
        let mediainfo = json!({"statements": {"P180": [{"rank": "preferred"}]}});
        assert_eq!(best_statements_json(&mediainfo, "P180").len(), 1);
    }

    #[test]
    #[cfg(feature = "wikibase")]
    fn ranks_from_uris() {
        assert_eq!(
            rank_from_uri("http://wikiba.se/ontology#PreferredRank"),
            Some(StatementRank::Preferred)
        );
        assert_eq!(
            rank_from_uri("wikibase:DeprecatedRank"),
            Some(StatementRank::Deprecated)
        );
        assert_eq!(
            rank_from_uri("http://wikiba.se/ontology#NormalRank"),
            Some(StatementRank::Normal)
        );
        assert_eq!(rank_from_uri("http://wikiba.se/ontology#BestRank"), None);
        assert_eq!(rank_from_uri("PreferredRank"), None);
    }
}
