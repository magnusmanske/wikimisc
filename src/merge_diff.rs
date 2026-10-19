//! This module contains the MergeDiff struct, which is used by the ItemMerger to generate the differences between two items.

use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde_json::json;
use std::collections::HashMap;
use wikibase::*;

/// Whether `statement_id` (`<entity ID>$<GUID>`) names a statement of the
/// entity `entity_id`. Wikibase rejects the whole edit if it carries a
/// statement ID of another entity, and adds one of its own entity that it does
/// not have yet as a new statement.
pub(crate) fn statement_belongs_to(statement_id: &str, entity_id: &str) -> bool {
    !entity_id.is_empty()
        && statement_id
            .split_once('$')
            .is_some_and(|(entity, _)| entity.eq_ignore_ascii_case(entity_id))
}

/// This contains the wbeditentiry payload to ADD data to a base item, generated from a merge
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergeDiff {
    pub labels: Vec<LocaleString>,
    pub aliases: Vec<LocaleString>,
    pub descriptions: Vec<LocaleString>,
    pub sitelinks: Vec<SiteLink>,
    pub altered_statements: HashMap<String, Statement>,
    pub added_statements: Vec<Statement>,
}

impl MergeDiff {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append every field of `other` onto `self`.
    ///
    /// **No dedup of terms.** Labels, aliases, descriptions and sitelinks from
    /// `other` are appended in order, and identical entries from a prior
    /// `extend` are kept duplicate. This matches how `ItemMerger` accumulates
    /// per-call diffs that have already been deduplicated against the merger's
    /// internal item state — so the diff stream is duplicate-free *if* it comes
    /// from the same merger. Hand-built diffs should be deduplicated by the
    /// caller before extending.
    ///
    /// Statements go through [`Self::add_statement`], because a later merge
    /// can extend a statement an earlier one added: that later version must
    /// replace the earlier one, or Wikibase would create the statement twice.
    pub fn extend(&mut self, other: &MergeDiff) {
        self.labels.extend(other.labels.iter().cloned());
        self.aliases.extend(other.aliases.iter().cloned());
        self.descriptions.extend(other.descriptions.iter().cloned());
        self.sitelinks.extend(other.sitelinks.iter().cloned());
        self.altered_statements.extend(
            other
                .altered_statements
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        for statement in &other.added_statements {
            self.add_statement(statement.clone());
        }
    }

    /// Fold this diff into `item`.
    pub fn apply(&self, item: &mut ItemEntity) {
        item.labels_mut().extend(self.labels.iter().cloned());
        item.aliases_mut().extend(self.aliases.iter().cloned());
        item.descriptions_mut()
            .extend(self.descriptions.iter().cloned());
        if !self.sitelinks.is_empty() {
            // A fresh `ItemEntity` has no sitelink list at all; create one rather
            // than discarding the diff's links. Guarded on `is_empty` so a diff
            // with no sitelinks leaves the item's `None` untouched.
            item.sitelinks_mut()
                .get_or_insert_with(Vec::new)
                .extend(self.sitelinks.iter().cloned());
        }
        for (id, statement) in self.altered_statements.iter() {
            // Wikibase adds a statement whose ID is unknown but belongs to the
            // entity, as from a newer copy of the same item.
            let belongs_to_item = statement_belongs_to(id, item.id());
            let existing_statement = item
                .claims_mut()
                .iter_mut()
                .find(|s| s.id().as_deref() == Some(id.as_str()));
            match existing_statement {
                Some(existing_statement) => *existing_statement = statement.to_owned(),
                None if belongs_to_item => item.add_claim(statement.to_owned()),
                None => {}
            }
        }
        for statement in &self.added_statements {
            Self::add_or_supersede(item.claims_mut(), statement.to_owned());
        }
    }

    /// Record a statement that was added or altered.
    ///
    /// A statement with an ID is an alteration of a saved statement. One
    /// without an ID is new — unless it merely extends a statement this diff
    /// already adds (same main snak, rank and type, with qualifiers and
    /// references only appended), in which case it replaces that one:
    /// `ItemMerger` reports a statement it added earlier, and then gave
    /// more references or qualifiers, as altered, but it still has no ID.
    pub fn add_statement(&mut self, s: Statement) {
        if let Some(id) = s.id() {
            self.altered_statements.insert(id, s);
        } else {
            Self::add_or_supersede(&mut self.added_statements, s);
        }
    }

    fn add_or_supersede(statements: &mut Vec<Statement>, new: Statement) {
        match statements
            .iter_mut()
            .find(|old| Self::supersedes(&new, old))
        {
            Some(old) => *old = new,
            None => statements.push(new),
        }
    }

    /// Whether `new` is a not-yet-saved statement `old` grown by merging: the
    /// same unsaved statement with qualifiers and references only appended,
    /// which is how `ItemMerger` folds a matching claim into an existing one.
    fn supersedes(new: &Statement, old: &Statement) -> bool {
        new.id().is_none()
            && old.id().is_none()
            && new.claim_type() == old.claim_type()
            && new.rank() == old.rank()
            && new.main_snak() == old.main_snak()
            && new.qualifiers().starts_with(old.qualifiers())
            && new.references().starts_with(old.references())
    }

    fn serialize_labels(&self, list: &[LocaleString]) -> Option<serde_json::Value> {
        if list.is_empty() {
            return None;
        }

        let labels: HashMap<String, serde_json::Value> = list
            .iter()
            .map(|l| {
                (
                    l.language().to_owned(),
                    json!({"language":l.language(),"value":l.value(), "add": ""}),
                )
            })
            .collect();
        Some(json!(labels))
    }

    /// Aliases are shaped differently from labels and descriptions in the
    /// `wbeditentity` payload: each language maps to a *list* of values, because
    /// an entity can carry many aliases per language — and [`MergeDiff::aliases`]
    /// routinely does, since `ItemMerger` turns clashing labels into aliases.
    /// Serialising them through [`Self::serialize_labels`] would silently keep
    /// only one per language.
    fn serialize_aliases(&self) -> Option<serde_json::Value> {
        if self.aliases.is_empty() {
            return None;
        }

        let mut by_language: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        for alias in &self.aliases {
            by_language
                .entry(alias.language().to_owned())
                .or_default()
                .push(json!({"language":alias.language(),"value":alias.value(), "add": ""}));
        }
        Some(json!(by_language))
    }

    fn serialize_sitelinks(&self) -> Option<serde_json::Value> {
        if self.sitelinks.is_empty() {
            return None;
        }

        let sitelinks: HashMap<String, serde_json::Value> = self
            .sitelinks
            .iter()
            .map(|l| {
                (
                    l.site().to_owned(),
                    json!({"site":l.site(),"title":l.title()}),
                )
            })
            .collect();
        Some(json!(sitelinks))
    }

    fn clean_snak(&self, snak: &mut serde_json::Value) {
        if let Some(o) = snak.as_object_mut() {
            o.remove("datatype");
        }
    }

    fn serialize_claims(&self) -> Option<serde_json::Value> {
        let ret: Vec<serde_json::Value> = self
            .added_statements
            .iter()
            .chain(self.altered_statements.values())
            .map(|c| json!(c))
            .map(|mut c| {
                if let Some(snak) = c.get_mut("mainsnak") {
                    self.clean_snak(snak);
                }

                if let Some(references) = c.get_mut("references").and_then(|r| r.as_array_mut()) {
                    for refgroup in references {
                        if let Some(prop_snaks_map) = refgroup["snaks"].as_object_mut() {
                            for (_, snaks) in prop_snaks_map.iter_mut() {
                                if let Some(snaks_array) = snaks.as_array_mut() {
                                    for snak in snaks_array {
                                        self.clean_snak(snak);
                                    }
                                }
                            }
                        }
                    }
                }
                c
            })
            .collect();

        if ret.is_empty() {
            None
        } else {
            Some(json!(ret))
        }
    }
}

impl Serialize for MergeDiff {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Build a Vec of only the fields that have content, avoiding the two-HashMap
        // allocate-then-filter pattern.
        let fields: Vec<(&str, serde_json::Value)> = [
            ("labels", self.serialize_labels(&self.labels)),
            ("aliases", self.serialize_aliases()),
            ("descriptions", self.serialize_labels(&self.descriptions)),
            ("sitelinks", self.serialize_sitelinks()),
            ("claims", self.serialize_claims()),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect();

        let mut state = serializer.serialize_struct("MergeDiff", fields.len())?;
        for (k, v) in &fields {
            state.serialize_field(k, v)?;
        }
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use crate::item_merger::ItemMerger;

    use super::*;

    #[test]
    fn test_time_compare() {
        // Year, ignore month and day
        let t1 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            9,
            "+1650-00-00T00:00:00Z",
            0,
        );
        let t2 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            9,
            "+1650-12-29T00:00:00Z",
            0,
        );
        assert!(ItemMerger::is_time_value_identical(&t1, &t1));
        assert!(ItemMerger::is_time_value_identical(&t1, &t2));

        // Month, ignore day
        let t1 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            10,
            "+1650-12-00T00:00:00Z",
            0,
        );
        let t2 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            10,
            "+1650-12-29T00:00:00Z",
            0,
        );
        assert!(ItemMerger::is_time_value_identical(&t1, &t1));
        assert!(ItemMerger::is_time_value_identical(&t1, &t2));
    }

    #[test]
    fn test_compare_locale_string() {
        let ls1 = LocaleString::new("en", "foo");
        let ls2 = LocaleString::new("en", "bar");
        let ls3 = LocaleString::new("de", "foo");
        assert_eq!(
            Ordering::Equal,
            ItemMerger::compare_locale_string(&ls1, &ls1)
        );
        assert_eq!(
            Ordering::Less,
            ItemMerger::compare_locale_string(&ls2, &ls1)
        );
        assert_eq!(
            Ordering::Greater,
            ItemMerger::compare_locale_string(&ls1, &ls2)
        );
        assert_eq!(
            Ordering::Greater,
            ItemMerger::compare_locale_string(&ls1, &ls3)
        );
    }

    #[test]
    fn test_merge_diff_new() {
        let diff = MergeDiff::new();
        assert!(diff.labels.is_empty());
        assert!(diff.aliases.is_empty());
        assert!(diff.descriptions.is_empty());
        assert!(diff.sitelinks.is_empty());
        assert!(diff.altered_statements.is_empty());
        assert!(diff.added_statements.is_empty());
    }

    #[test]
    fn test_merge_diff_extend() {
        let mut diff1 = MergeDiff::new();
        diff1.labels.push(LocaleString::new("en", "test1"));
        diff1.added_statements.push(Statement::new_normal(
            Snak::new_string("P1", "value1"),
            vec![],
            vec![],
        ));

        let mut diff2 = MergeDiff::new();
        diff2.labels.push(LocaleString::new("de", "test2"));
        diff2.added_statements.push(Statement::new_normal(
            Snak::new_string("P2", "value2"),
            vec![],
            vec![],
        ));

        diff1.extend(&diff2);
        assert_eq!(diff1.labels.len(), 2);
        assert_eq!(diff1.added_statements.len(), 2);
    }

    #[test]
    fn test_merge_diff_add_statement_with_id() {
        let mut diff = MergeDiff::new();
        let mut statement = Statement::new_normal(Snak::new_string("P123", "test"), vec![], vec![]);
        statement.set_id("Q1$abc-123");

        diff.add_statement(statement.clone());
        assert_eq!(diff.altered_statements.len(), 1);
        assert_eq!(diff.added_statements.len(), 0);
        assert!(diff.altered_statements.contains_key("Q1$abc-123"));
    }

    #[test]
    fn test_merge_diff_add_statement_without_id() {
        let mut diff = MergeDiff::new();
        let statement = Statement::new_normal(Snak::new_string("P123", "test"), vec![], vec![]);

        diff.add_statement(statement.clone());
        assert_eq!(diff.altered_statements.len(), 0);
        assert_eq!(diff.added_statements.len(), 1);
    }

    fn p31_q5(urls: &[&str]) -> Statement {
        let references = urls
            .iter()
            .map(|url| Reference::new(vec![Snak::new_url("P854", url)]))
            .collect();
        Statement::new_normal(Snak::new_item("P31", "Q5"), vec![], references)
    }

    fn source_with(statement: Statement) -> ItemEntity {
        let mut item = ItemEntity::new_empty();
        item.add_claim(statement);
        item
    }

    #[test]
    fn test_merge_diff_add_statement_replaces_the_unsaved_statement_it_extends() {
        let mut diff = MergeDiff::new();
        diff.add_statement(p31_q5(&["http://a.example.com"]));
        diff.add_statement(p31_q5(&["http://a.example.com", "http://b.example.com"]));
        assert_eq!(
            diff.added_statements,
            vec![p31_q5(&["http://a.example.com", "http://b.example.com"])]
        );
    }

    #[test]
    fn test_merge_diff_add_statement_keeps_unrelated_statements() {
        // Same main snak, but neither reference list extends the other.
        let mut diff = MergeDiff::new();
        diff.add_statement(p31_q5(&["http://a.example.com"]));
        diff.add_statement(p31_q5(&["http://b.example.com"]));
        assert_eq!(diff.added_statements.len(), 2);
    }

    #[test]
    fn test_merge_diff_extend_does_not_duplicate_statement_extended_by_later_merge() {
        let mut im = ItemMerger::new(ItemEntity::new_empty());
        let first = im.merge(&source_with(p31_q5(&["http://a.example.com"])));
        let second = im.merge(&source_with(p31_q5(&["http://b.example.com"])));

        let mut total = MergeDiff::new();
        total.extend(&first);
        total.extend(&second);

        assert_eq!(
            total.added_statements,
            vec![p31_q5(&["http://a.example.com", "http://b.example.com"])]
        );
        let payload = serde_json::to_value(&total).unwrap();
        assert_eq!(payload["claims"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_merge_diff_apply_does_not_duplicate_statement_extended_by_later_merge() {
        let mut im = ItemMerger::new(ItemEntity::new_empty());
        let first = im.merge(&source_with(p31_q5(&["http://a.example.com"])));
        let second = im.merge(&source_with(p31_q5(&["http://b.example.com"])));

        let mut item = ItemEntity::new_empty();
        first.apply(&mut item);
        second.apply(&mut item);

        assert_eq!(item.claims(), im.item().claims());
        assert_eq!(item.claims().len(), 1);
    }

    // Altered statements are sent whole, with their ID, so whatever does not
    // survive serialization is deleted from the saved statement. Both of these
    // were lost by wikibase < 0.7.7.

    #[test]
    fn test_serialize_altered_statement_keeps_all_qualifiers_of_a_property() {
        let mut base = ItemEntity::new_empty();
        let mut existing = Statement::new_normal(Snak::new_item("P31", "Q5"), vec![], vec![]);
        existing.set_id("Q1$p31");
        base.add_claim(existing);
        let source = source_with(Statement::new_normal(
            Snak::new_item("P31", "Q5"),
            vec![
                Snak::new_string("P1810", "A"),
                Snak::new_string("P1810", "B"),
            ],
            vec![],
        ));

        let diff = ItemMerger::new(base).merge(&source);
        let payload = serde_json::to_value(&diff).unwrap();

        let claim = &payload["claims"][0];
        assert_eq!(claim["id"], "Q1$p31");
        let values: Vec<&str> = claim["qualifiers"]["P1810"]
            .as_array()
            .unwrap()
            .iter()
            .map(|q| q["datavalue"]["value"].as_str().unwrap())
            .collect();
        assert_eq!(values, ["A", "B"]);
    }

    #[test]
    fn test_serialize_altered_statement_keeps_quantity_bounds() {
        let quantity = Snak::new(
            SnakDataType::Quantity,
            "P2067",
            SnakType::Value,
            Some(DataValue::new(
                DataValueType::Quantity,
                Value::Quantity(QuantityValue::new(
                    5.0,
                    Some(4.5),
                    "http://www.wikidata.org/entity/Q11570",
                    Some(5.5),
                )),
            )),
        );
        let mut base = ItemEntity::new_empty();
        let mut existing = Statement::new_normal(quantity.clone(), vec![], vec![]);
        existing.set_id("Q1$mass");
        base.add_claim(existing);
        let source = source_with(Statement::new_normal(
            quantity,
            vec![],
            vec![Reference::new(vec![Snak::new_url(
                "P854",
                "http://a.example.com",
            )])],
        ));

        let diff = ItemMerger::new(base).merge(&source);
        let payload = serde_json::to_value(&diff).unwrap();

        let claim = &payload["claims"][0];
        assert_eq!(claim["id"], "Q1$mass");
        let value = &claim["mainsnak"]["datavalue"]["value"];
        assert_eq!(value["lowerBound"].as_f64(), Some(4.5));
        assert_eq!(value["upperBound"].as_f64(), Some(5.5));
        assert!(value.get("lower_bound").is_none(), "{value}");
        assert!(value.get("upper_bound").is_none(), "{value}");
    }

    #[test]
    fn test_serialize_claims_no_references_not_removed() {
        // A statement with no references must serialize WITHOUT a "references" key.
        // Regression: `c["references"].as_array_mut()` uses serde_json's IndexMut, which
        // inserts `"references": null` for the missing key before `as_array_mut()` returns
        // None. The wbeditentity API rejects that with "The ReferenceList serialization
        // should be an array", so the key must be entirely absent (not present-and-null).
        let mut diff = MergeDiff::new();
        diff.added_statements.push(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![], // no references
        ));
        let serialized = serde_json::to_value(&diff).unwrap();
        let claims = serialized["claims"].as_array().unwrap();
        assert_eq!(claims.len(), 1);
        let claim = claims[0].as_object().unwrap();
        assert!(
            !claim.contains_key("references"),
            "A reference-less statement must not contain a \"references\" key at all, got: {}",
            claims[0]
        );
    }

    #[test]
    fn test_serialize_claims_references_snaks_cleaned() {
        // References that do exist must have their snak datatype fields removed.
        let mut diff = MergeDiff::new();
        diff.added_statements.push(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![Reference::new(vec![Snak::new_url(
                "P854",
                "http://example.com",
            )])],
        ));
        let serialized = serde_json::to_value(&diff).unwrap();
        let claims = serialized["claims"].as_array().unwrap();
        let refs = claims[0]["references"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        // Each snak inside the reference must not have a "datatype" field
        let snaks_map = refs[0]["snaks"].as_object().unwrap();
        for snaks in snaks_map.values() {
            for snak in snaks.as_array().unwrap() {
                assert!(
                    snak.get("datatype").is_none(),
                    "datatype should be cleaned from reference snaks"
                );
            }
        }
    }

    #[test]
    fn test_merge_diff_apply_to_item() {
        let mut item = ItemEntity::new_empty();
        item.labels_mut().push(LocaleString::new("en", "original"));

        let mut diff = MergeDiff::new();
        diff.labels.push(LocaleString::new("de", "new_label"));
        diff.descriptions
            .push(LocaleString::new("en", "description"));
        diff.added_statements.push(Statement::new_normal(
            Snak::new_string("P123", "test"),
            vec![],
            vec![],
        ));

        diff.apply(&mut item);
        assert_eq!(item.labels().len(), 2);
        assert_eq!(item.descriptions().len(), 1);
        assert_eq!(item.claims().len(), 1);
    }

    #[test]
    fn test_merge_diff_apply_altered_statement() {
        let mut item = ItemEntity::new_empty();
        let mut original_statement =
            Statement::new_normal(Snak::new_string("P123", "original"), vec![], vec![]);
        original_statement.set_id("Q1$test-id");
        item.add_claim(original_statement);

        let mut diff = MergeDiff::new();
        let mut altered_statement = Statement::new_normal(
            Snak::new_string("P123", "modified"),
            vec![Snak::new_string("P1", "qualifier")],
            vec![],
        );
        altered_statement.set_id("Q1$test-id");
        diff.altered_statements
            .insert("Q1$test-id".to_string(), altered_statement);

        diff.apply(&mut item);
        assert_eq!(item.claims().len(), 1);
        assert_eq!(item.claims()[0].qualifiers().len(), 1);
    }

    #[test]
    fn test_merge_diff_apply_with_sitelinks() {
        // apply() must append diff sitelinks to a pre-existing sitelinks Vec on the item.
        let mut item = ItemEntity::new_empty();
        item.sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Existing", vec![]));

        let mut diff = MergeDiff::new();
        diff.sitelinks.push(SiteLink::new("dewiki", "Neu", vec![]));

        diff.apply(&mut item);

        let sl = item.sitelinks().clone().unwrap();
        assert_eq!(sl.len(), 2);
        assert!(sl.iter().any(|s| s.site() == "enwiki"));
        assert!(sl.iter().any(|s| s.site() == "dewiki"));
    }

    #[test]
    fn test_merge_diff_apply_altered_statement_not_in_item_is_ignored() {
        // If an altered-statement ID does not exist on the item, apply() must not
        // panic and must leave the item's existing claims unchanged.
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1", "existing"),
            vec![],
            vec![],
        ));

        let mut diff = MergeDiff::new();
        let mut ghost = Statement::new_normal(Snak::new_string("P99", "ghost"), vec![], vec![]);
        ghost.set_id("Q1$does-not-exist");
        diff.altered_statements
            .insert("Q1$does-not-exist".to_string(), ghost);

        diff.apply(&mut item);

        // The existing claim must be untouched and no new claim must have appeared.
        assert_eq!(item.claims().len(), 1);
        assert_eq!(item.claims()[0].property(), "P1");
    }

    #[test]
    fn test_statement_belongs_to() {
        assert!(statement_belongs_to("Q1$abc", "Q1"));
        assert!(statement_belongs_to("q1$abc", "Q1"));
        assert!(!statement_belongs_to("Q2$abc", "Q1"));
        assert!(!statement_belongs_to("Q12$abc", "Q1"));
        assert!(!statement_belongs_to("Q1", "Q1"));
        assert!(!statement_belongs_to("$abc", ""));
    }

    #[test]
    fn test_merge_diff_apply_adds_unknown_altered_statement_of_the_item() {
        // Wikibase adds a statement whose ID belongs to the entity but is not
        // on it yet, so `apply` must too.
        let mut item = ItemEntity::new_empty();
        item.set_id("Q1".to_string());

        let mut diff = MergeDiff::new();
        let mut statement = Statement::new_normal(Snak::new_string("P1", "new"), vec![], vec![]);
        statement.set_id("Q1$new");
        diff.add_statement(statement.clone());

        diff.apply(&mut item);
        assert_eq!(item.claims(), &vec![statement]);
    }

    #[test]
    fn test_merge_diff_extend_covers_all_fields() {
        // extend() must propagate every field, including aliases and sitelinks.
        let mut diff1 = MergeDiff::new();
        diff1.aliases.push(LocaleString::new("en", "alias1"));
        diff1
            .sitelinks
            .push(SiteLink::new("enwiki", "Article", vec![]));

        let mut diff2 = MergeDiff::new();
        diff2.aliases.push(LocaleString::new("de", "alias2"));
        diff2
            .sitelinks
            .push(SiteLink::new("dewiki", "Artikel", vec![]));

        diff1.extend(&diff2);

        assert_eq!(diff1.aliases.len(), 2);
        assert_eq!(diff1.sitelinks.len(), 2);
        assert!(diff1.aliases.iter().any(|a| a.language() == "en"));
        assert!(diff1.aliases.iter().any(|a| a.language() == "de"));
        assert!(diff1.sitelinks.iter().any(|s| s.site() == "enwiki"));
        assert!(diff1.sitelinks.iter().any(|s| s.site() == "dewiki"));
    }

    #[test]
    fn test_merge_diff_serialize_empty_emits_no_fields() {
        // An empty MergeDiff must serialise to an object with no fields, since every
        // sub-serializer returns None for empty input. This locks in the wbeditentity
        // contract that a no-op merge produces a no-op payload.
        let diff = MergeDiff::new();
        let serialized = serde_json::to_value(&diff).unwrap();
        let obj = serialized
            .as_object()
            .expect("MergeDiff serialises to object");
        assert!(
            obj.is_empty(),
            "Empty MergeDiff must serialise to {{}}, got: {serialized}"
        );
    }

    #[test]
    fn test_merge_diff_apply_empty_diff_is_noop() {
        // Applying an empty diff must not change the item.
        let mut item = ItemEntity::new_empty();
        item.labels_mut().push(LocaleString::new("en", "kept"));
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1", "kept"),
            vec![],
            vec![],
        ));

        let diff = MergeDiff::new();
        diff.apply(&mut item);

        assert_eq!(item.labels().len(), 1);
        assert_eq!(item.labels()[0].value(), "kept");
        assert_eq!(item.claims().len(), 1);
    }

    #[test]
    fn test_merge_diff_apply_creates_sitelink_list_when_item_has_none() {
        // A fresh ItemEntity has `sitelinks() == None`. apply() must create the
        // list rather than dropping the diff's sitelinks.
        let mut item = ItemEntity::new_empty();
        assert!(
            item.sitelinks().is_none(),
            "fresh item must start with no sitelinks"
        );

        let mut diff = MergeDiff::new();
        diff.sitelinks.push(SiteLink::new("enwiki", "Test", vec![]));

        diff.apply(&mut item);

        let sitelinks = item
            .sitelinks()
            .as_ref()
            .expect("sitelinks must be created");
        assert_eq!(sitelinks.len(), 1);
        assert_eq!(sitelinks[0].site(), "enwiki");
        assert_eq!(sitelinks[0].title(), "Test");
    }

    #[test]
    fn test_merge_diff_apply_leaves_none_sitelinks_alone_when_diff_has_none() {
        // The converse: an empty sitelink list in the diff must not materialise an
        // empty Vec on the item.
        let mut item = ItemEntity::new_empty();
        MergeDiff::new().apply(&mut item);
        assert!(item.sitelinks().is_none());
    }

    #[test]
    fn test_merge_diff_apply_aliases_and_descriptions() {
        let mut item = ItemEntity::new_empty();

        let mut diff = MergeDiff::new();
        diff.aliases.push(LocaleString::new("en", "alt-name"));
        diff.descriptions
            .push(LocaleString::new("en", "a description"));

        diff.apply(&mut item);

        assert_eq!(item.aliases().len(), 1);
        assert_eq!(item.aliases()[0].value(), "alt-name");
        assert_eq!(item.descriptions().len(), 1);
        assert_eq!(item.descriptions()[0].value(), "a description");
    }

    // --- Serialization of the term/sitelink half of the wbeditentity payload ---
    //
    // NOTE: `serialize_labels` also emits an `"add": ""` key for labels and
    // descriptions. That is the *alias* append idiom rather than anything
    // `wbeditentity` documents for labels, but it is long-standing behaviour, so
    // these tests pin it as-is rather than quietly changing a third thing.

    #[test]
    fn test_serialize_labels_uses_plural_key_and_maps_by_language() {
        let mut diff = MergeDiff::new();
        diff.labels.push(LocaleString::new("en", "Douglas Adams"));
        diff.labels.push(LocaleString::new("de", "Douglas Adams"));

        let serialized = serde_json::to_value(&diff).unwrap();

        // `wbeditentity` expects "labels", not "label".
        assert!(
            serialized.get("label").is_none(),
            "the singular \"label\" key must not be emitted, got: {serialized}"
        );
        let labels = serialized["labels"].as_object().unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels["en"]["language"], "en");
        assert_eq!(labels["en"]["value"], "Douglas Adams");
        assert_eq!(labels["de"]["value"], "Douglas Adams");
    }

    #[test]
    fn test_serialize_descriptions_keyed_by_language() {
        let mut diff = MergeDiff::new();
        diff.descriptions
            .push(LocaleString::new("en", "English writer"));

        let serialized = serde_json::to_value(&diff).unwrap();

        let descriptions = serialized["descriptions"].as_object().unwrap();
        assert_eq!(descriptions.len(), 1);
        assert_eq!(descriptions["en"]["language"], "en");
        assert_eq!(descriptions["en"]["value"], "English writer");
    }

    #[test]
    fn test_serialize_aliases_are_emitted_at_all() {
        // Regression: aliases were computed by ItemMerger and then dropped on
        // serialization, so they never reached the API.
        let mut diff = MergeDiff::new();
        diff.aliases
            .push(LocaleString::new("en", "Douglas Noel Adams"));

        let serialized = serde_json::to_value(&diff).unwrap();

        assert!(
            serialized.get("aliases").is_some(),
            "aliases present in the diff must be serialized, got: {serialized}"
        );
        assert_eq!(
            serialized["aliases"]["en"][0]["value"],
            "Douglas Noel Adams"
        );
    }

    #[test]
    fn test_serialize_aliases_groups_multiple_per_language_into_a_list() {
        // An entity can have many aliases per language, and ItemMerger produces
        // exactly that when a clashing label is demoted to an alias. Each
        // language must therefore map to a list, not a single object.
        let mut diff = MergeDiff::new();
        diff.aliases.push(LocaleString::new("en", "DNA"));
        diff.aliases
            .push(LocaleString::new("en", "Douglas N. Adams"));
        diff.aliases
            .push(LocaleString::new("de", "Douglas Noel Adams"));

        let serialized = serde_json::to_value(&diff).unwrap();
        let aliases = serialized["aliases"].as_object().unwrap();

        assert_eq!(aliases.len(), 2, "two languages expected: {serialized}");
        let en = aliases["en"].as_array().unwrap();
        assert_eq!(
            en.len(),
            2,
            "both English aliases must survive: {serialized}"
        );
        let values: Vec<&str> = en.iter().map(|a| a["value"].as_str().unwrap()).collect();
        assert!(values.contains(&"DNA"));
        assert!(values.contains(&"Douglas N. Adams"));
        // `add` marks the alias list as an append rather than a replacement.
        assert_eq!(en[0]["add"], "");
        assert_eq!(aliases["de"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_serialize_sitelinks_keyed_by_site() {
        let mut diff = MergeDiff::new();
        diff.sitelinks
            .push(SiteLink::new("enwiki", "Douglas Adams", vec![]));
        diff.sitelinks
            .push(SiteLink::new("dewiki", "Douglas Adams", vec![]));

        let serialized = serde_json::to_value(&diff).unwrap();
        let sitelinks = serialized["sitelinks"].as_object().unwrap();

        assert_eq!(sitelinks.len(), 2);
        assert_eq!(sitelinks["enwiki"]["site"], "enwiki");
        assert_eq!(sitelinks["enwiki"]["title"], "Douglas Adams");
        assert_eq!(sitelinks["dewiki"]["title"], "Douglas Adams");
    }

    #[test]
    fn test_serialize_omits_every_empty_section() {
        // An empty diff must serialize to an empty object rather than a payload
        // full of nulls, so that wbeditentity does not clear existing terms.
        let serialized = serde_json::to_value(MergeDiff::new()).unwrap();
        assert_eq!(
            serialized.as_object().unwrap().len(),
            0,
            "expected no keys at all, got: {serialized}"
        );
    }

    #[test]
    fn test_serialize_full_payload_has_all_sections() {
        let mut diff = MergeDiff::new();
        diff.labels.push(LocaleString::new("en", "Label"));
        diff.aliases.push(LocaleString::new("en", "Alias"));
        diff.descriptions.push(LocaleString::new("en", "Desc"));
        diff.sitelinks
            .push(SiteLink::new("enwiki", "Title", vec![]));
        diff.added_statements.push(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![],
        ));

        let serialized = serde_json::to_value(&diff).unwrap();
        let keys: Vec<&str> = serialized
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();

        for expected in ["labels", "aliases", "descriptions", "sitelinks", "claims"] {
            assert!(
                keys.contains(&expected),
                "missing \"{expected}\" in payload: {serialized}"
            );
        }
    }
}
