//! This module contains the MergeDiff struct, which is used by the ItemMerger to generate the differences between two items.

use regex::Regex;
use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde_json::json;
use std::collections::HashMap;
use std::vec::Vec;
use wikibase::*;

lazy_static! {
    static ref YEAR_FIX: Regex = Regex::new(r"-\d\d-\d\dT").unwrap();
    static ref MONTH_FIX: Regex = Regex::new(r"-\d\dT").unwrap();
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

    pub fn extend(&mut self, other: &MergeDiff) {
        self.labels.extend(other.labels.clone());
        self.aliases.extend(other.aliases.clone());
        self.descriptions.extend(other.descriptions.clone());
        self.sitelinks.extend(other.sitelinks.clone());
        self.altered_statements
            .extend(other.altered_statements.clone());
        self.added_statements.extend(other.added_statements.clone());
    }

    // TODO tests
    pub fn apply(&self, item: &mut ItemEntity) {
        item.labels_mut().extend(self.labels.clone());
        item.aliases_mut().extend(self.aliases.clone());
        item.descriptions_mut().extend(self.descriptions.clone());
        if let Some(sitelinks) = item.sitelinks_mut() {
            sitelinks.extend(self.sitelinks.clone());
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
        item.claims_mut().extend(self.added_statements.clone());
    }

    pub fn add_statement(&mut self, s: Statement) {
        if let Some(id) = s.id() {
            self.altered_statements.insert(id, s);
        } else {
            self.added_statements.push(s);
        }
    }

    fn serialize_labels(&self, list: &[LocaleString]) -> Option<serde_json::Value> {
        match list.is_empty() {
            true => None,
            false => {
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
        }
    }

    fn _serialize_aliases(&self) -> Option<serde_json::Value> {
        match self.aliases.is_empty() {
            true => None,
            false => {
                let mut ret: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
                for alias in &self.aliases {
                    let v = json!({"language":alias.language(),"value":alias.value(), "add": ""});
                    ret.entry(alias.language().into())
                        .and_modify(|vec| vec.push(v.to_owned()))
                        .or_insert(vec![v]);
                }
                Some(json!(ret))
            }
        }
    }

    fn serialize_sitelinks(&self) -> Option<serde_json::Value> {
        match self.sitelinks.is_empty() {
            true => None,
            false => {
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
        }
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
            .cloned()
            .map(|c| json!(c))
            .map(|c| {
                let mut c = c;
                if let Some(snak) = c.get_mut("mainsnak") {
                    self.clean_snak(snak)
                }
                match c["references"].as_array_mut() {
                    Some(references) => {
                        for refgroup in references {
                            if let Some(prop_snaks_map) = refgroup["snaks"].as_object_mut() {
                                prop_snaks_map.iter_mut().for_each(|prop_snaks| {
                                    if let Some(snaks) = prop_snaks.1.as_array_mut() {
                                        snaks.iter_mut().for_each(|snak| self.clean_snak(snak));
                                    }
                                })
                            }
                        }
                    }
                    None => {
                        if let Some(x) = c.as_object_mut() {
                            x.remove("references");
                        }
                    }
                }
                c
            })
            .collect();
        match ret.is_empty() {
            true => None,
            false => Some(json!(ret)),
        }
    }
}

impl Serialize for MergeDiff {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut data: HashMap<&str, Option<serde_json::Value>> = HashMap::new();
        data.insert("label", self.serialize_labels(&self.labels));
        data.insert("descriptions", self.serialize_labels(&self.descriptions));
        //data.insert("aliases",self.serialize_aliases()); // DEACTIVATED too much noise
        data.insert("sitelinks", self.serialize_sitelinks());
        data.insert("claims", self.serialize_claims());
        let data: HashMap<&str, serde_json::Value> = data
            .iter()
            .filter(|(_, v)| v.is_some())
            .map(|(k, v)| (k.to_owned(), v.to_owned().unwrap())) // unwrap() is safe
            .collect();

        let mut state = serializer.serialize_struct("MergeDiff", data.len())?;
        for (k, v) in data {
            state.serialize_field(k, &v)?
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
}
