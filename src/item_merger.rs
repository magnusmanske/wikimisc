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

pub struct ItemMerger {
    pub item: ItemEntity,
}

impl ItemMerger {
    pub fn new(item: ItemEntity) -> Self {
        Self { item }
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
                Self::are_qualifiers_identical(new_claim.qualifiers(), existing_claim.qualifiers())
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
            if reference_changed {
                existing_claim.set_references(new_references);
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

    pub fn get_external_ids_from_reference(reference: &Reference) -> Vec<ExternalId> {
        reference
            .snaks()
            .iter()
            .filter(|snak| *snak.datatype() == SnakDataType::ExternalId)
            .map(|snak| (ExternalId::prop_numeric(snak.property()), snak.data_value()))
            .filter(|(prop, dv)| prop.is_some() && dv.is_some())
            .map(|(prop, dv)| (prop.unwrap(), dv.to_owned().unwrap())) // unwrap()s are safe
            .map(|(prop, dv)| (prop, dv.value().to_owned()))
            .filter_map(|(prop, value)| match value {
                Value::StringValue(s) => Some(ExternalId::new(prop, &s)),
                _ => None,
            })
            .collect()
    }

    /// Checks if a reference already exists in a list of references.
    /// Uses direct equal, or the presence of any external ID from the new reference.
    /// Returns `true` if the reference exists, `false` otherwise.
    fn reference_exists(existing_references: &[Reference], new_reference: &Reference) -> bool {
        if existing_references.contains(new_reference) {
            // Easy case
            return true;
        }
        // Check if any external ID in the new reference is present in any existing reference
        let ext_ids = Self::get_external_ids_from_reference(new_reference);
        existing_references
            .iter()
            .map(Self::get_external_ids_from_reference)
            .filter(|existing_external_ids| !existing_external_ids.is_empty())
            .any(|existing_external_ids| {
                ext_ids
                    .iter()
                    .any(|ext_id| existing_external_ids.contains(ext_id))
            })
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
            .collect();
        diff.append(&mut new_ones.clone());
        mine.append(&mut new_ones);
        ret
    }
}
