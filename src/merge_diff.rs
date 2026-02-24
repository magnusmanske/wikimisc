//! This module contains the MergeDiff struct, which is used by the ItemMerger to generate the differences between two items.

use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde_json::json;
use std::collections::HashMap;
use std::vec::Vec;
use wikibase::*;

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
        self.added_statements
            .extend(other.added_statements.iter().cloned());
    }

    // TODO tests
    pub fn apply(&self, item: &mut ItemEntity) {
        item.labels_mut().extend(self.labels.iter().cloned());
        item.aliases_mut().extend(self.aliases.iter().cloned());
        item.descriptions_mut()
            .extend(self.descriptions.iter().cloned());
        if let Some(sitelinks) = item.sitelinks_mut() {
            sitelinks.extend(self.sitelinks.iter().cloned());
        };
        for (id, statement) in self.altered_statements.iter() {
            let existing_statement = item
                .claims_mut()
                .iter_mut()
                .find(|s| s.id() == Some(id.to_string()));
            if let Some(existing_statement) = existing_statement {
                *existing_statement = statement.to_owned();
            }
        }
        item.claims_mut()
            .extend(self.added_statements.iter().cloned());
    }

    pub fn add_statement(&mut self, s: Statement) {
        if let Some(id) = s.id() {
            self.altered_statements.insert(id, s);
        } else {
            self.added_statements.push(s);
        }
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

    fn _serialize_aliases(&self) -> Option<serde_json::Value> {
        if self.aliases.is_empty() {
            return None;
        }

        let mut ret: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        for alias in &self.aliases {
            let v = json!({"language":alias.language(),"value":alias.value(), "add": ""});
            ret.entry(alias.language().into())
                .and_modify(|vec| vec.push(v.to_owned()))
                .or_insert_with(|| vec![v]);
        }
        Some(json!(ret))
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

                if let Some(references) = c["references"].as_array_mut() {
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
            ("label", self.serialize_labels(&self.labels)),
            ("descriptions", self.serialize_labels(&self.descriptions)),
            //("aliases", self.serialize_aliases()), // DEACTIVATED too much noise
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

    #[test]
    fn test_serialize_claims_no_references_not_removed() {
        // A statement with no references must serialize without a "references" key —
        // the old else-if branch would remove it even when it was already absent.
        let mut diff = MergeDiff::new();
        diff.added_statements.push(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![], // no references
        ));
        let serialized = serde_json::to_value(&diff).unwrap();
        let claims = serialized["claims"].as_array().unwrap();
        assert_eq!(claims.len(), 1);
        // "references" key must either be absent or be an empty array — never null or garbage
        let refs = &claims[0]["references"];
        assert!(
            refs.is_null() || refs.as_array().map(|a| a.is_empty()).unwrap_or(false),
            "Expected absent or empty references, got: {refs}"
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
}
