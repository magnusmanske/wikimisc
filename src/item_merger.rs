//! `ItemMerger` takes an `ItemEntity` and merges another `ItemEntity` into it.
//! It will return the differences as a `MergeDiff` object, which can be used with the `wbeditentity` API action.
//! Note that currently, only added or altered statements will be generated for the diff. Removed statements will be ignored.

use crate::external_id::ExternalId;
use crate::merge_diff::MergeDiff;
use regex::Regex;
use serde_json::json;
use std::cmp::Ordering;
use std::vec::Vec;
use wikibase::*;

lazy_static! {
    static ref YEAR_FIX: Regex = Regex::new(r"-\d\d-\d\dT").unwrap();
    static ref MONTH_FIX: Regex = Regex::new(r"-\d\dT").unwrap();
}

#[derive(Debug, Clone)]
pub struct ItemMerger {
    pub item: ItemEntity,
    properties_ignore_qualifier_match: Vec<String>,
}

impl ItemMerger {
    pub fn new(item: ItemEntity) -> Self {
        Self {
            item,
            properties_ignore_qualifier_match: vec![],
        }
    }

    pub fn merge(&mut self, other: &ItemEntity) -> MergeDiff {
        let mut diff = MergeDiff::new();
        let mut new_aliases =
            Self::merge_locale_strings(self.item.labels_mut(), other.labels(), &mut diff.labels);

        // Descriptions
        let mut new_ones: Vec<LocaleString> = other
            .descriptions()
            .iter()
            .filter_map(|x| {
                match self
                    .item
                    .descriptions()
                    .iter()
                    .find(|y| x.language() == y.language())
                {
                    Some(_) => None,
                    None => Some(x.clone()),
                }
            })
            .filter(|d| !self.item.labels().contains(d))
            .filter(|d| !self.item.aliases().contains(d))
            .collect();
        diff.descriptions.append(&mut new_ones.clone());
        self.item.descriptions_mut().append(&mut new_ones);

        // Aliases
        new_aliases.append(&mut other.aliases().clone());
        new_aliases.sort_by(Self::compare_locale_string);
        new_aliases.dedup();
        diff.aliases = new_aliases
            .iter()
            .filter(|a| !self.item.aliases().contains(a))
            .filter(|a| !self.item.labels().contains(a))
            .filter(|a| !self.item.descriptions().contains(a))
            .cloned()
            .collect();
        self.item
            .aliases_mut()
            .append(&mut other.aliases().to_owned());
        self.item.aliases_mut().sort_by(Self::compare_locale_string);
        self.item.aliases_mut().dedup();

        // Sitelinks: add only
        if let Some(sitelinks) = other.sitelinks() {
            let mut new_ones: Vec<SiteLink> = sitelinks
                .iter()
                .filter(|x| match self.item.sitelinks() {
                    Some(sl) => !sl.iter().any(|y| x.site() == y.site()),
                    None => true,
                })
                .cloned()
                .collect();
            if let Some(my_sitelinks) = self.item.sitelinks_mut() {
                diff.sitelinks = new_ones.clone();
                my_sitelinks.append(&mut new_ones);
            }
        }

        for claim in other.claims() {
            if let Some(s) = self.add_claim(claim.to_owned()) {
                diff.add_statement(s)
            }
        }

        // self.prop_text.append(&mut other.prop_text.clone());
        // self.prop_text.sort();
        // self.prop_text.dedup();
        diff
    }

    /// Adds a new claim to the item claims.
    /// If a claim with the same value and qualifiers (TBD) already exists, it will try and add any new references.
    /// Returns `Some(claim)` if the claim was added or changed, `None` otherwise.
    pub fn add_claim(&mut self, new_claim: Statement) -> Option<Statement> {
        let mut existing_claims_iter = self
            .item
            .claims_mut()
            .iter_mut()
            .filter(|existing_claim| {
                Self::is_snak_identical(new_claim.main_snak(), existing_claim.main_snak())
            })
            .filter(|existing_claim| {
                let property = existing_claim.main_snak().property().to_string();
                self.properties_ignore_qualifier_match.contains(&property)
                    || Self::are_qualifiers_identical(
                        new_claim.qualifiers(),
                        existing_claim.qualifiers(),
                    )
            });
        if let Some(existing_claim) = existing_claims_iter.next() {
            // At least one claim exists, use first one
            if *new_claim.main_snak().datatype() == SnakDataType::ExternalId {
                return None; // Claim already exists, don't add reference to external IDs
            }
            let mut new_references = existing_claim.references().clone();
            let mut reference_changed = false;
            for r in new_claim.references() {
                if !Self::reference_exists(&new_references, r) {
                    new_references.push(r.to_owned());
                    reference_changed = true;
                }
            }
            let qualifier_snaks =
                Self::merge_qualifiers(new_claim.qualifiers(), existing_claim.qualifiers());
            let qualifiers_changed = qualifier_snaks != *existing_claim.qualifiers();

            if reference_changed || qualifiers_changed {
                existing_claim.set_references(new_references);
                existing_claim.set_qualifier_snaks(qualifier_snaks);
                return Some(existing_claim.to_owned()); // Claim has changed (references added)
            }
            return None; // Claim already exists, including references
        }

        let mut new_claim = new_claim.clone();
        self.check_new_claim_for_dates(&mut new_claim);

        // Claim does not exist, adding
        self.item.add_claim(new_claim.clone());
        Some(new_claim)
    }

    fn merge_qualifiers(new_qualifiers: &Vec<Snak>, existing_qualifiers: &Vec<Snak>) -> Vec<Snak> {
        // Start with existing qualifiers
        let mut qualifier_snaks = existing_qualifiers.to_owned();
        // Add new qualifiers, if they do not exist yet
        for qualifier in new_qualifiers {
            if !existing_qualifiers
                .iter()
                .any(|q| Self::is_snak_identical(q, qualifier))
            {
                qualifier_snaks.push(qualifier.to_owned());
            }
        }
        // Return merged qualifiers
        qualifier_snaks
    }

    pub fn get_external_ids_from_reference(reference: &Reference) -> Vec<ExternalId> {
        reference
            .snaks()
            .iter()
            .filter(|snak| *snak.datatype() == SnakDataType::ExternalId)
            .filter_map(|snak| {
                let prop = ExternalId::prop_numeric(snak.property())?;
                let dv = snak.data_value().as_ref()?;
                match dv.value() {
                    Value::StringValue(s) => Some(ExternalId::new(prop, s)),
                    _ => None,
                }
            })
            .collect()
    }

    pub fn get_reference_urls_from_reference(reference: &Reference) -> Vec<String> {
        reference
            .snaks()
            .iter()
            .filter(|snak| *snak.datatype() == SnakDataType::Url)
            .filter_map(|snak| {
                let dv = snak.data_value().as_ref()?;
                match dv.value() {
                    Value::StringValue(s) => Some(s.to_owned()),
                    _ => None,
                }
            })
            .collect()
    }

    /// Checks if a reference already exists in a list of references.
    /// Uses the presence of any external ID or reference URL from the new reference.
    /// If the reference contains neither, falls back to full structural snak comparison.
    /// Returns `true` if the reference exists, `false` otherwise.
    fn reference_exists(existing_references: &[Reference], new_reference: &Reference) -> bool {
        let ext_ids = Self::get_external_ids_from_reference(new_reference);
        let reference_urls = Self::get_reference_urls_from_reference(new_reference);

        // Check if any external ID matches
        let has_external_ids = !ext_ids.is_empty()
            && existing_references
                .iter()
                .flat_map(Self::get_external_ids_from_reference)
                .any(|ext_id| ext_ids.contains(&ext_id));

        // Check if any reference URL matches
        let has_reference_urls = !reference_urls.is_empty()
            && existing_references
                .iter()
                .flat_map(Self::get_reference_urls_from_reference)
                .any(|reference_url| reference_urls.contains(&reference_url));

        if has_external_ids || has_reference_urls {
            return true;
        }

        // Fallback: if the reference has no external IDs or URLs, compare all snaks structurally
        if ext_ids.is_empty() && reference_urls.is_empty() {
            return existing_references.iter().any(|existing| {
                Self::are_qualifiers_identical(existing.snaks(), new_reference.snaks())
            });
        }

        false
    }

    pub fn is_snak_identical(snak1: &Snak, snak2: &Snak) -> bool {
        snak1.property() == snak2.property()
            && Self::is_data_value_identical(snak1.data_value(), snak2.data_value())
    }

    fn is_data_value_identical(dv1: &Option<DataValue>, dv2: &Option<DataValue>) -> bool {
        if let (Some(dv1), Some(dv2)) = (dv1, dv2) {
            if let (Value::Time(t1), Value::Time(t2)) = (dv1.value(), dv2.value()) {
                return Self::is_time_value_identical(t1, t2);
            }
        }
        dv1 == dv2
    }

    pub fn is_time_value_identical(t1: &TimeValue, t2: &TimeValue) -> bool {
        if t1.precision() != t2.precision()
            || t1.calendarmodel() != t1.calendarmodel()
            || t1.before() != t1.before()
            || t1.after() != t1.after()
            || t1.timezone() != t1.timezone()
        {
            return false;
        }
        match t1.precision() {
            9 => {
                let t1s = YEAR_FIX.replace_all(t1.time(), "-00-00T");
                let t2s = YEAR_FIX.replace_all(t2.time(), "-00-00T");
                t1s == t2s
            }
            10 => {
                let t1s = MONTH_FIX.replace_all(t1.time(), "-00T");
                let t2s = MONTH_FIX.replace_all(t2.time(), "-00T");
                t1s == t2s
            }
            _ => *t1 == *t2,
        }
    }

    pub fn are_qualifiers_identical(q1: &[Snak], q2: &[Snak]) -> bool {
        if q1.is_empty() && q2.is_empty() {
            return true;
        }
        if q1.len() != q2.len() {
            return false;
        }
        let mut q1 = q1.to_vec();
        let mut q2 = q2.to_vec();
        q1.sort_by(Self::compare_snak);
        q2.sort_by(Self::compare_snak);
        !q1.iter()
            .zip(q2.iter())
            .any(|(snak1, snak2)| !Self::is_snak_identical(snak1, snak2))
    }

    pub fn check_new_claim_for_dates(&self, new_claim: &mut Statement) {
        let prop = new_claim.property();
        if prop != "P569" && prop != "P570" {
            return;
        }
        if let Some(dv) = new_claim.main_snak().data_value() {
            let new_claim_precision = match dv.value() {
                Value::Time(t) => *t.precision(),
                _ => return,
            };

            let best_existing_precision = self
                .item
                .claims()
                .iter()
                .filter(|c| c.property() == prop)
                .filter_map(|c| c.main_snak().data_value().to_owned())
                .filter_map(|dv| match dv.value() {
                    Value::Time(t) => Some(*t.precision()),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            if new_claim_precision < best_existing_precision {
                new_claim.set_rank(StatementRank::Deprecated);
            }
        }
    }

    pub fn compare_locale_string(a: &LocaleString, b: &LocaleString) -> Ordering {
        match a.language().cmp(b.language()) {
            Ordering::Equal => a.value().cmp(b.value()),
            other => other,
        }
    }

    fn compare_snak(snak1: &Snak, snak2: &Snak) -> Ordering {
        match snak1.property().cmp(snak2.property()) {
            Ordering::Equal => {
                let j1 = json!(snak1.data_value());
                let j2 = json!(snak2.data_value());
                let j1 = j1.to_string();
                let j2 = j2.to_string();
                j1.cmp(&j2)
            }
            other => other,
        }
    }

    fn merge_locale_strings(
        mine: &mut Vec<LocaleString>,
        other: &[LocaleString],
        diff: &mut Vec<LocaleString>,
    ) -> Vec<LocaleString> {
        let mut ret = vec![];
        let mul_label = mine
            .iter()
            .find(|x| x.language() == "mul")
            .map(|l| l.value());
        let mut new_ones: Vec<LocaleString> = other
            .iter()
            .filter_map(|x| {
                match mine.iter().find(|y| x.language() == y.language()) {
                    Some(y) => {
                        if x.value() != y.value() {
                            ret.push(x.clone()); // Labels for which a language already exists, as aliases
                        }
                        None
                    }
                    None => Some(x.clone()),
                }
            })
            // Filter out labels identical to the existing "mul" one
            .filter(|x| match mul_label {
                Some(mul) => x.value() != mul,
                None => true,
            })
            .collect();
        diff.append(&mut new_ones.clone());
        mine.append(&mut new_ones);
        ret
    }

    pub fn set_properties_ignore_qualifier_match(
        &mut self,
        properties_ignore_qualifier_match: Vec<String>,
    ) {
        self.properties_ignore_qualifier_match = properties_ignore_qualifier_match;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_claim_p225_both_with_qualifiers() {
        let mut base_item = ItemEntity::new_empty();
        let mut statement = Statement::new_normal(
            Snak::new_string("P225", "foo bar"),
            vec![Snak::new_item("P31", "Q5")],
            vec![],
        );
        statement.set_id("Blah");
        base_item.add_claim(statement);

        let mut new_item = ItemEntity::new_empty();
        new_item.add_claim(Statement::new_normal(
            Snak::new_string("P225", "foo bar"),
            vec![Snak::new_item("P31", "Q1")],
            vec![],
        ));

        let mut im = ItemMerger::new(base_item);
        im.set_properties_ignore_qualifier_match(vec!["P225".to_string()]);
        let diff = im.merge(&new_item);
        assert!(!diff.altered_statements.is_empty());
        assert_eq!(diff.altered_statements["Blah"].qualifiers().len(), 2);
    }

    #[test]
    fn test_reference_exists_by_external_ids() {
        let reference1 = Reference::new(vec![Snak::new_external_id("P214", "12345")]);
        let reference2 = Reference::new(vec![Snak::new_external_id("P214", "12346")]);
        let references = vec![reference1.to_owned()];
        assert!(ItemMerger::reference_exists(&references, &reference1));
        assert!(!ItemMerger::reference_exists(&references, &reference2));
    }

    #[test]
    fn test_reference_exists_by_reference_urls() {
        let reference1 = Reference::new(vec![Snak::new_url("P854", "http://foo.bar")]);
        let reference2 = Reference::new(vec![Snak::new_url("P854", "http://foo.bars")]);
        let references = vec![reference1.to_owned()];
        assert!(ItemMerger::reference_exists(&references, &reference1));
        assert!(!ItemMerger::reference_exists(&references, &reference2));
    }

    #[test]
    fn test_is_snak_identical() {
        let snak1 = Snak::new_string("P123", "test");
        let snak2 = Snak::new_string("P123", "test");
        let snak3 = Snak::new_string("P123", "different");
        let snak4 = Snak::new_string("P456", "test");

        assert!(ItemMerger::is_snak_identical(&snak1, &snak2));
        assert!(!ItemMerger::is_snak_identical(&snak1, &snak3));
        assert!(!ItemMerger::is_snak_identical(&snak1, &snak4));
    }

    #[test]
    fn test_is_time_value_identical_precision_9() {
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
        let t3 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            9,
            "+1651-00-00T00:00:00Z",
            0,
        );

        assert!(ItemMerger::is_time_value_identical(&t1, &t2)); // Same year, different month/day OK for precision 9
        assert!(!ItemMerger::is_time_value_identical(&t1, &t3)); // Different year
    }

    #[test]
    fn test_is_time_value_identical_precision_10() {
        let t1 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            10,
            "+1650-05-00T00:00:00Z",
            0,
        );
        let t2 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            10,
            "+1650-05-15T00:00:00Z",
            0,
        );
        let t3 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            10,
            "+1650-06-00T00:00:00Z",
            0,
        );

        assert!(ItemMerger::is_time_value_identical(&t1, &t2)); // Same year-month, different day OK for precision 10
        assert!(!ItemMerger::is_time_value_identical(&t1, &t3)); // Different month
    }

    #[test]
    fn test_are_qualifiers_identical() {
        let q1 = vec![Snak::new_string("P1", "a"), Snak::new_string("P2", "b")];
        let q2 = vec![Snak::new_string("P2", "b"), Snak::new_string("P1", "a")]; // Different order
        let q3 = vec![Snak::new_string("P1", "a")];
        let empty: Vec<Snak> = vec![];

        assert!(ItemMerger::are_qualifiers_identical(&q1, &q2)); // Order shouldn't matter
        assert!(!ItemMerger::are_qualifiers_identical(&q1, &q3)); // Different length
        assert!(ItemMerger::are_qualifiers_identical(&empty, &empty)); // Both empty
        assert!(!ItemMerger::are_qualifiers_identical(&q1, &empty)); // One empty
    }

    #[test]
    fn test_get_external_ids_from_reference() {
        let reference = Reference::new(vec![
            Snak::new_external_id("P214", "12345"),
            Snak::new_external_id("P227", "67890"),
            Snak::new_string("P123", "not_an_ext_id"),
        ]);

        let ext_ids = ItemMerger::get_external_ids_from_reference(&reference);
        assert_eq!(ext_ids.len(), 2);
        assert!(ext_ids.contains(&ExternalId::new(214, "12345")));
        assert!(ext_ids.contains(&ExternalId::new(227, "67890")));
    }

    #[test]
    fn test_get_reference_urls_from_reference() {
        let reference = Reference::new(vec![
            Snak::new_url("P854", "http://example.com"),
            Snak::new_url("P973", "http://another.com"),
            Snak::new_string("P123", "not_a_url"),
        ]);

        let urls = ItemMerger::get_reference_urls_from_reference(&reference);
        assert_eq!(urls.len(), 2);
        assert!(urls.contains(&"http://example.com".to_string()));
        assert!(urls.contains(&"http://another.com".to_string()));
    }

    #[test]
    fn test_reference_exists_empty_references() {
        let reference = Reference::new(vec![Snak::new_external_id("P214", "12345")]);
        let empty_refs: Vec<Reference> = vec![];
        assert!(!ItemMerger::reference_exists(&empty_refs, &reference));
    }

    #[test]
    fn test_reference_exists_no_matching_criteria() {
        // Reference with neither external IDs nor URLs
        let reference = Reference::new(vec![Snak::new_string("P123", "test")]);
        let existing = vec![Reference::new(vec![Snak::new_string("P456", "other")])];
        assert!(!ItemMerger::reference_exists(&existing, &reference));
    }

    /// Regression test for https://github.com/magnusmanske/auth2wd/issues/7
    /// Statements with the same datavalue and qualifiers but different references should be merged
    /// into a single statement, with references consolidated (no duplicates).
    #[test]
    fn test_merge_same_value_same_qualifiers_different_references() {
        // Base item has a statement with reference R1
        let mut base_item = ItemEntity::new_empty();
        let ref1 = Reference::new(vec![Snak::new_url("P854", "http://source1.example.com")]);
        let mut stmt1 =
            Statement::new_normal(Snak::new_string("P1476", "some title"), vec![], vec![ref1]);
        stmt1.set_id("Q1$base-stmt");
        base_item.add_claim(stmt1);

        // New item has the same statement (same value, same qualifiers) with reference R2
        let mut new_item = ItemEntity::new_empty();
        let ref2 = Reference::new(vec![Snak::new_url("P854", "http://source2.example.com")]);
        new_item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "some title"),
            vec![],
            vec![ref2],
        ));

        let mut im = ItemMerger::new(base_item);
        let _diff = im.merge(&new_item);

        // Should still be only ONE statement (not two)
        assert_eq!(
            im.item.claims().len(),
            1,
            "Should have exactly one statement after merging identical statements"
        );

        // That one statement should have BOTH references
        let merged_stmt = &im.item.claims()[0];
        assert_eq!(
            merged_stmt.references().len(),
            2,
            "Merged statement should have 2 references (one from each source)"
        );
    }

    /// Regression test: identical references should not be duplicated when merging
    #[test]
    fn test_merge_same_value_same_qualifiers_same_references_no_duplicates() {
        // Base item has a statement with reference R1
        let mut base_item = ItemEntity::new_empty();
        let ref1 = Reference::new(vec![Snak::new_url("P854", "http://source1.example.com")]);
        let mut stmt1 = Statement::new_normal(
            Snak::new_string("P1476", "some title"),
            vec![],
            vec![ref1.clone()],
        );
        stmt1.set_id("Q1$base-stmt");
        base_item.add_claim(stmt1);

        // New item has the same statement with the SAME reference
        let mut new_item = ItemEntity::new_empty();
        new_item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "some title"),
            vec![],
            vec![ref1],
        ));

        let mut im = ItemMerger::new(base_item);
        let _diff = im.merge(&new_item);

        // Should still be only ONE statement
        assert_eq!(im.item.claims().len(), 1);
        // Reference should not be duplicated
        assert_eq!(im.item.claims()[0].references().len(), 1);
    }

    /// Regression test: plain-snak references (no ext ID, no URL) should be deduplicated correctly
    #[test]
    fn test_merge_plain_snak_references_no_duplicates() {
        let mut base_item = ItemEntity::new_empty();
        let ref1 = Reference::new(vec![Snak::new_string("P813", "2024-01-01")]);
        let mut stmt1 = Statement::new_normal(
            Snak::new_string("P31", "some value"),
            vec![],
            vec![ref1.clone()],
        );
        stmt1.set_id("Q1$base-stmt2");
        base_item.add_claim(stmt1);

        // New item has the same statement with the SAME plain-snak reference
        let mut new_item = ItemEntity::new_empty();
        new_item.add_claim(Statement::new_normal(
            Snak::new_string("P31", "some value"),
            vec![],
            vec![ref1],
        ));

        let mut im = ItemMerger::new(base_item);
        let _diff = im.merge(&new_item);

        // Should be ONE statement, and the reference should NOT be duplicated
        assert_eq!(im.item.claims().len(), 1);
        assert_eq!(
            im.item.claims()[0].references().len(),
            1,
            "Identical plain-snak reference should not be duplicated"
        );
    }
}
