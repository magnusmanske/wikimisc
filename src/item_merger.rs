//! `ItemMerger` takes an `ItemEntity` and merges another `ItemEntity` into it.
//! It returns the differences as a [`MergeDiff`] object, which can be sent to
//! the `wbeditentity` API action.
//!
//! Note that currently, only added or altered statements appear in the diff —
//! removed statements are intentionally not emitted.
//!
//! # Stateful merger
//!
//! `ItemMerger` is a *stateful accumulator*: each call to [`ItemMerger::merge`]
//! both mutates the internal item (so subsequent merges can dedup against the
//! growing union of data) and returns a [`MergeDiff`] describing the changes
//! produced by *that single call*. Typical usage:
//!
//! ```ignore
//! let mut im = ItemMerger::new(target);
//! let mut total = MergeDiff::new();
//! for src in sources {
//!     total.extend(&im.merge(src));
//! }
//! // `im.item()` is now the fully merged entity.
//! // `total` is the cumulative wbeditentity payload.
//! ```

use crate::external_id::ExternalId;
use crate::merge_diff::MergeDiff;
use regex::Regex;

use std::cmp::Ordering;
use std::sync::LazyLock;
use wikibase::*;

// Literal patterns, held as `Option<Regex>` so a pattern that somehow failed to
// compile degrades to exact timestamp comparison rather than panicking in a
// library. `test_all_static_regexes_compile` makes sure CI catches that.
static YEAR_FIX: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"-\d\d-\d\dT").ok());
static MONTH_FIX: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"-\d\dT").ok());

#[derive(Debug, Clone)]
pub struct ItemMerger {
    item: ItemEntity,
    properties_ignore_qualifier_match: Vec<String>,
}

impl ItemMerger {
    pub fn new(item: ItemEntity) -> Self {
        Self {
            item,
            properties_ignore_qualifier_match: vec![],
        }
    }

    /// Borrow the merged-so-far entity.
    pub fn item(&self) -> &ItemEntity {
        &self.item
    }

    /// Consume the merger and return the merged entity.
    pub fn into_item(self) -> ItemEntity {
        self.item
    }

    /// Merge `other` into the internal item.
    ///
    /// **Side effect:** mutates `self.item` by absorbing labels/aliases/etc.
    /// from `other`. This intermediate state is what makes deduplication work
    /// across subsequent `merge` calls.
    ///
    /// **Return value:** the diff for *this call only*. Callers that want a
    /// cumulative diff across multiple merges should hold their own
    /// [`MergeDiff`] and `extend` it on each call.
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
            if !new_ones.is_empty() {
                diff.sitelinks = new_ones.clone();
                // A fresh `ItemEntity` has no sitelink list at all, so create one
                // rather than discarding the incoming links.
                self.item
                    .sitelinks_mut()
                    .get_or_insert_with(Vec::new)
                    .append(&mut new_ones);
            }
        }

        for claim in other.claims() {
            if let Some(s) = self.add_claim(claim.to_owned()) {
                diff.add_statement(s)
            }
        }

        diff
    }

    /// Adds a new claim to the item's claims.
    ///
    /// If a claim with an identical main snak already exists *and* the two
    /// qualifier lists are compatible — see [`Self::are_qualifiers_compatible`],
    /// or the property being listed via
    /// [`Self::set_properties_ignore_qualifier_match`] — the new claim is folded
    /// into the existing one instead of being added: any references and
    /// qualifiers it contributes are merged in. External-ID claims are never
    /// merged this way; a duplicate is simply dropped.
    ///
    /// Returns `Some(claim)` if a claim was added or changed, `None` otherwise.
    pub fn add_claim(&mut self, mut new_claim: Statement) -> Option<Statement> {
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
                    || Self::are_qualifiers_compatible(
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

    /// Checks whether a reference is considered a duplicate of any reference in `existing_references`.
    ///
    /// Matching strategy (in priority order):
    /// 1. If the new reference contains any external IDs (e.g. P214), it is considered a
    ///    duplicate if *any* existing reference shares at least one of those external IDs.
    /// 2. Otherwise if it contains reference URLs (P854), same rule applies.
    /// 3. If it has neither, a full structural comparison of all snaks is used.
    ///
    /// Note: strategies 1 and 2 are intentionally loose — a partial ID match is enough to
    /// consider the reference already covered, avoiding duplicate sourcing from the same source.
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
            || t1.calendarmodel() != t2.calendarmodel()
            || t1.before() != t2.before()
            || t1.after() != t2.after()
            || t1.timezone() != t2.timezone()
        {
            return false;
        }
        match t1.precision() {
            9 => Self::times_match_blanking(YEAR_FIX.as_ref(), t1, t2, "-00-00T"),
            10 => Self::times_match_blanking(MONTH_FIX.as_ref(), t1, t2, "-00T"),
            _ => *t1 == *t2,
        }
    }

    /// Compare two timestamps after blanking the components the precision leaves
    /// undefined. Falls back to exact comparison when `re` is unavailable, which
    /// is conservative: it can only report fewer values as identical.
    fn times_match_blanking(
        re: Option<&Regex>,
        t1: &TimeValue,
        t2: &TimeValue,
        replacement: &str,
    ) -> bool {
        match re {
            Some(re) => {
                re.replace_all(t1.time(), replacement) == re.replace_all(t2.time(), replacement)
            }
            None => *t1 == *t2,
        }
    }

    /// Two qualifier lists are compatible if one is a subset of the other. This prevents adding
    /// a bare statement (e.g. an external ID with no qualifiers) as a duplicate of an existing
    /// statement that merely carries an extra qualifier such as P1810 ("subject named as").
    /// See <https://github.com/magnusmanske/auth2wd/issues/10>.
    pub fn are_qualifiers_compatible(q1: &[Snak], q2: &[Snak]) -> bool {
        Self::is_qualifier_subset(q1, q2) || Self::is_qualifier_subset(q2, q1)
    }

    /// Returns `true` if every snak in `sub` has an identical snak in `sup`.
    fn is_qualifier_subset(sub: &[Snak], sup: &[Snak]) -> bool {
        sub.iter()
            .all(|q| sup.iter().any(|e| Self::is_snak_identical(q, e)))
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
                // Serialise directly to a JSON string for a stable, deterministic ordering.
                // This avoids the intermediate serde_json::Value allocation that json!() would
                // produce before calling to_string().
                let s1 = serde_json::to_string(snak1.data_value()).unwrap_or_default();
                let s2 = serde_json::to_string(snak2.data_value()).unwrap_or_default();
                s1.cmp(&s2)
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
    fn test_is_time_value_identical_different_calendarmodel() {
        let t1 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            11,
            "+1900-01-01T00:00:00Z",
            0,
        );
        let t2 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985786", // Julian calendar
            11,
            "+1900-01-01T00:00:00Z",
            0,
        );
        assert!(!ItemMerger::is_time_value_identical(&t1, &t2));
    }

    #[test]
    fn test_is_time_value_identical_different_timezone() {
        let t1 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            11,
            "+1900-01-01T00:00:00Z",
            0,
        );
        let t2 = TimeValue::new(
            60, // UTC+1
            0,
            "http://www.wikidata.org/entity/Q1985727",
            11,
            "+1900-01-01T00:00:00Z",
            0,
        );
        assert!(!ItemMerger::is_time_value_identical(&t1, &t2));
    }

    #[test]
    fn test_is_time_value_identical_different_before_after() {
        let t1 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            11,
            "+1900-01-01T00:00:00Z",
            0,
        );
        // Different 'before' offset
        let t2 = TimeValue::new(
            0,
            1,
            "http://www.wikidata.org/entity/Q1985727",
            11,
            "+1900-01-01T00:00:00Z",
            0,
        );
        assert!(!ItemMerger::is_time_value_identical(&t1, &t2));
        // Different 'after' offset
        let t3 = TimeValue::new(
            0,
            0,
            "http://www.wikidata.org/entity/Q1985727",
            11,
            "+1900-01-01T00:00:00Z",
            1,
        );
        assert!(!ItemMerger::is_time_value_identical(&t1, &t3));
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
    fn test_reference_exists_partial_ext_id_match_counts_as_duplicate() {
        // A new reference sharing even one external ID with an existing reference
        // is considered a duplicate, even if the new reference has additional snaks.
        let existing = vec![Reference::new(vec![Snak::new_external_id("P214", "123")])];
        let new_ref = Reference::new(vec![
            Snak::new_external_id("P214", "123"), // shared
            Snak::new_external_id("P227", "456"), // extra
        ]);
        assert!(ItemMerger::reference_exists(&existing, &new_ref));
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
            im.item().claims().len(),
            1,
            "Should have exactly one statement after merging identical statements"
        );

        // That one statement should have BOTH references
        let merged_stmt = &im.item().claims()[0];
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
        assert_eq!(im.item().claims().len(), 1);
        // Reference should not be duplicated
        assert_eq!(im.item().claims()[0].references().len(), 1);
    }

    // ── merge() orchestration edge cases ────────────────────────────────────

    #[test]
    fn test_merge_empty_into_empty() {
        let mut im = ItemMerger::new(ItemEntity::new_empty());
        let diff = im.merge(&ItemEntity::new_empty());
        assert!(diff.labels.is_empty());
        assert!(diff.aliases.is_empty());
        assert!(diff.descriptions.is_empty());
        assert!(diff.sitelinks.is_empty());
        assert!(diff.altered_statements.is_empty());
        assert!(diff.added_statements.is_empty());
    }

    #[test]
    fn test_merge_new_labels_added() {
        let mut base = ItemEntity::new_empty();
        base.labels_mut().push(LocaleString::new("en", "English"));

        let mut other = ItemEntity::new_empty();
        other.labels_mut().push(LocaleString::new("de", "Deutsch"));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert_eq!(diff.labels.len(), 1);
        assert_eq!(diff.labels[0], LocaleString::new("de", "Deutsch"));
        // Merged into the item
        assert_eq!(im.item().labels().len(), 2);
    }

    #[test]
    fn test_merge_conflicting_label_becomes_alias() {
        // When the other item has a label for a language that base already has (but different
        // value), the other's label is added as an alias, not a label.
        let mut base = ItemEntity::new_empty();
        base.labels_mut().push(LocaleString::new("en", "Foo"));

        let mut other = ItemEntity::new_empty();
        other.labels_mut().push(LocaleString::new("en", "Bar"));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        // "Bar" goes into aliases (conflicting label), not labels
        assert!(
            diff.labels.is_empty(),
            "Conflicting label must not appear in diff.labels"
        );
        assert!(
            diff.aliases
                .iter()
                .any(|a| a == &LocaleString::new("en", "Bar")),
            "Conflicting label must appear in diff.aliases"
        );
    }

    #[test]
    fn test_merge_duplicate_label_not_re_added() {
        let mut base = ItemEntity::new_empty();
        base.labels_mut().push(LocaleString::new("en", "Same"));

        let mut other = ItemEntity::new_empty();
        other.labels_mut().push(LocaleString::new("en", "Same"));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert!(diff.labels.is_empty());
        assert!(diff.aliases.is_empty());
    }

    #[test]
    fn test_merge_description_not_added_when_language_exists() {
        let mut base = ItemEntity::new_empty();
        base.descriptions_mut()
            .push(LocaleString::new("en", "original description"));

        let mut other = ItemEntity::new_empty();
        other
            .descriptions_mut()
            .push(LocaleString::new("en", "different description"));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        // Descriptions are only added when the language is absent, never overwritten
        assert!(diff.descriptions.is_empty());
    }

    #[test]
    fn test_merge_new_description_added() {
        let mut base = ItemEntity::new_empty();
        base.descriptions_mut()
            .push(LocaleString::new("en", "English desc"));

        let mut other = ItemEntity::new_empty();
        other
            .descriptions_mut()
            .push(LocaleString::new("fr", "description française"));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert_eq!(diff.descriptions.len(), 1);
        assert_eq!(
            diff.descriptions[0],
            LocaleString::new("fr", "description française")
        );
    }

    #[test]
    fn test_merge_description_not_added_when_equals_existing_label() {
        // A description equal to an existing label should be silently dropped
        let mut base = ItemEntity::new_empty();
        base.labels_mut().push(LocaleString::new("en", "shared"));

        let mut other = ItemEntity::new_empty();
        other
            .descriptions_mut()
            .push(LocaleString::new("de", "shared")); // same value, different language — still a label match

        // Actually the filter checks language+value equality via contains(), so only an exact
        // LocaleString match is filtered. Test the case where description == label (same lang+val).
        let mut other2 = ItemEntity::new_empty();
        other2
            .descriptions_mut()
            .push(LocaleString::new("en", "shared"));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other2);
        assert!(
            diff.descriptions.is_empty(),
            "Description that matches an existing label must be dropped"
        );
    }

    #[test]
    fn test_merge_new_claim_added_to_diff() {
        let mut base = ItemEntity::new_empty();
        base.add_claim(Statement::new_normal(
            Snak::new_string("P1", "existing"),
            vec![],
            vec![],
        ));

        let mut other = ItemEntity::new_empty();
        other.add_claim(Statement::new_normal(
            Snak::new_string("P2", "new claim"),
            vec![],
            vec![],
        ));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert_eq!(diff.added_statements.len(), 1);
        assert_eq!(diff.added_statements[0].property(), "P2");
    }

    #[test]
    fn test_merge_duplicate_claim_not_readded() {
        let mut base = ItemEntity::new_empty();
        base.add_claim(Statement::new_normal(
            Snak::new_string("P1", "value"),
            vec![],
            vec![],
        ));

        let mut other = ItemEntity::new_empty();
        other.add_claim(Statement::new_normal(
            Snak::new_string("P1", "value"),
            vec![],
            vec![],
        ));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert!(diff.added_statements.is_empty());
        assert!(diff.altered_statements.is_empty());
        assert_eq!(im.item().claims().len(), 1);
    }

    #[test]
    fn test_merge_mul_label_filters_new_labels() {
        // If the base item has a "mul" (multilingual) label, new labels with the same value
        // should not be added.
        let mut base = ItemEntity::new_empty();
        base.labels_mut().push(LocaleString::new("mul", "UniLabel"));

        let mut other = ItemEntity::new_empty();
        other.labels_mut().push(LocaleString::new("en", "UniLabel")); // same value as mul
        other
            .labels_mut()
            .push(LocaleString::new("fr", "Different")); // different value

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        // "UniLabel" must be filtered out (matches mul label), "Different" must be added
        assert!(
            diff.labels.iter().all(|l| l.value() != "UniLabel"),
            "Label matching mul value must not be added"
        );
        assert!(
            diff.labels.iter().any(|l| l.value() == "Different"),
            "Non-matching label must still be added"
        );
    }

    // ── check_new_claim_for_dates ──────────────────────────────────────────

    fn make_date_claim(prop: &str, time: &str, precision: u64) -> Statement {
        Statement::new_normal(
            Snak::new(
                SnakDataType::Time,
                prop,
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Time,
                    Value::Time(TimeValue::new(
                        0,
                        0,
                        "http://www.wikidata.org/entity/Q1985727",
                        precision,
                        time,
                        0,
                    )),
                )),
            ),
            vec![],
            vec![],
        )
    }

    #[test]
    fn test_check_new_claim_for_dates_non_date_prop_unchanged() {
        let base = ItemEntity::new_empty();
        let im = ItemMerger::new(base);

        let mut claim = make_date_claim("P31", "+1900-00-00T00:00:00Z", 9);
        im.check_new_claim_for_dates(&mut claim);
        // P31 is not P569/P570, rank must remain Normal
        assert_eq!(*claim.rank(), StatementRank::Normal);
    }

    #[test]
    fn test_check_new_claim_for_dates_lower_precision_deprecated() {
        // Base already has P569 at precision 11 (day). A new P569 at precision 9 (year)
        // must be marked deprecated.
        let mut base = ItemEntity::new_empty();
        base.add_claim(make_date_claim("P569", "+1900-05-10T00:00:00Z", 11));

        let im = ItemMerger::new(base);
        let mut new_claim = make_date_claim("P569", "+1900-00-00T00:00:00Z", 9);
        im.check_new_claim_for_dates(&mut new_claim);
        assert_eq!(*new_claim.rank(), StatementRank::Deprecated);
    }

    #[test]
    fn test_check_new_claim_for_dates_higher_precision_not_deprecated() {
        // Base has P570 at precision 9, new one at precision 11 — keep Normal.
        let mut base = ItemEntity::new_empty();
        base.add_claim(make_date_claim("P570", "+1900-00-00T00:00:00Z", 9));

        let im = ItemMerger::new(base);
        let mut new_claim = make_date_claim("P570", "+1900-05-10T00:00:00Z", 11);
        im.check_new_claim_for_dates(&mut new_claim);
        assert_eq!(*new_claim.rank(), StatementRank::Normal);
    }

    #[test]
    fn test_check_new_claim_for_dates_equal_precision_not_deprecated() {
        let mut base = ItemEntity::new_empty();
        base.add_claim(make_date_claim("P569", "+1900-00-00T00:00:00Z", 9));

        let im = ItemMerger::new(base);
        let mut new_claim = make_date_claim("P569", "+1901-00-00T00:00:00Z", 9);
        im.check_new_claim_for_dates(&mut new_claim);
        assert_eq!(*new_claim.rank(), StatementRank::Normal);
    }

    #[test]
    fn test_check_new_claim_for_dates_no_existing_claim() {
        // No existing P569 → precision comparison gives 0 → new claim is never deprecated.
        let base = ItemEntity::new_empty();
        let im = ItemMerger::new(base);
        let mut new_claim = make_date_claim("P569", "+1900-00-00T00:00:00Z", 9);
        im.check_new_claim_for_dates(&mut new_claim);
        assert_eq!(*new_claim.rank(), StatementRank::Normal);
    }

    // ── add_claim with external IDs ────────────────────────────────────────

    #[test]
    fn test_add_claim_external_id_not_duplicated() {
        // External-ID claims that already exist must not have references added —
        // the function returns None and the item count stays at 1.
        let mut base = ItemEntity::new_empty();
        base.add_claim(Statement::new_normal(
            Snak::new_external_id("P214", "123456"),
            vec![],
            vec![],
        ));

        let new_claim = Statement::new_normal(
            Snak::new_external_id("P214", "123456"),
            vec![],
            vec![Reference::new(vec![Snak::new_url(
                "P854",
                "http://viaf.org",
            )])],
        );

        let mut im = ItemMerger::new(base);
        let result = im.add_claim(new_claim);
        assert!(
            result.is_none(),
            "Existing external-ID claim must not be altered"
        );
        assert_eq!(im.item().claims().len(), 1);
        assert!(im.item().claims()[0].references().is_empty());
    }

    // ── Regression test: plain-snak references (no ext ID, no URL) should be deduplicated correctly
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
        assert_eq!(im.item().claims().len(), 1);
        assert_eq!(
            im.item().claims()[0].references().len(),
            1,
            "Identical plain-snak reference should not be duplicated"
        );
    }

    // ── sitelinks ─────────────────────────────────────────────────────────

    #[test]
    fn test_merge_sitelinks_added_when_base_has_sitelink_list() {
        // Sitelinks from `other` must be appended when the base item already has a
        // (possibly empty) sitelinks Vec.
        let mut base = ItemEntity::new_empty();
        base.sitelinks_mut().get_or_insert_with(Vec::new);

        let mut other = ItemEntity::new_empty();
        other
            .sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Test article", vec![]));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert_eq!(diff.sitelinks.len(), 1);
        assert_eq!(diff.sitelinks[0].site(), "enwiki");
        assert_eq!(im.item().sitelinks().clone().unwrap().len(), 1);
        assert_eq!(
            im.item().sitelinks().clone().unwrap()[0].title(),
            "Test article"
        );
    }

    #[test]
    fn test_merge_sitelinks_not_duplicated() {
        // A sitelink already present in the base must not be added again.
        let mut base = ItemEntity::new_empty();
        base.sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Test article", vec![]));

        let mut other = ItemEntity::new_empty();
        other
            .sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Test article", vec![]));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert!(
            diff.sitelinks.is_empty(),
            "duplicate sitelink must not appear in diff"
        );
        assert_eq!(im.item().sitelinks().clone().unwrap().len(), 1);
    }

    #[test]
    fn test_merge_multiple_sitelinks_only_new_ones_added() {
        // Only sites not already present in base should be merged in.
        let mut base = ItemEntity::new_empty();
        base.sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Shared", vec![]));

        let mut other = ItemEntity::new_empty();
        let sl = other.sitelinks_mut().get_or_insert_with(Vec::new);
        sl.push(SiteLink::new("enwiki", "Shared", vec![])); // already in base
        sl.push(SiteLink::new("dewiki", "Neu", vec![])); // new

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert_eq!(diff.sitelinks.len(), 1);
        assert_eq!(diff.sitelinks[0].site(), "dewiki");
        assert_eq!(im.item().sitelinks().clone().unwrap().len(), 2);
    }

    // ── description filters ───────────────────────────────────────────────

    #[test]
    fn test_merge_description_not_added_when_equals_existing_alias() {
        // A new description whose (language, value) pair matches an existing alias
        // must be silently dropped.
        let mut base = ItemEntity::new_empty();
        base.aliases_mut()
            .push(LocaleString::new("en", "alias value"));

        let mut other = ItemEntity::new_empty();
        other
            .descriptions_mut()
            .push(LocaleString::new("en", "alias value"));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert!(
            diff.descriptions.is_empty(),
            "description matching an existing alias must not be added"
        );
        assert!(im.item().descriptions().is_empty());
    }

    // ── qualifier helpers ─────────────────────────────────────────────────

    #[test]
    fn test_are_qualifiers_identical_different_values() {
        // Same property, different value — must not be treated as identical.
        let q1 = vec![Snak::new_string("P1", "foo")];
        let q2 = vec![Snak::new_string("P1", "bar")];
        assert!(!ItemMerger::are_qualifiers_identical(&q1, &q2));
    }

    #[test]
    fn test_are_qualifiers_identical_different_properties() {
        // Same value, different property — must not be treated as identical.
        let q1 = vec![Snak::new_string("P1", "val")];
        let q2 = vec![Snak::new_string("P2", "val")];
        assert!(!ItemMerger::are_qualifiers_identical(&q1, &q2));
    }

    // ── add_claim edge-cases ──────────────────────────────────────────────

    #[test]
    fn test_add_claim_external_id_different_value_is_added() {
        // Two ExternalId claims for the same property but different IDs are distinct
        // and both should be present after the merge.
        let mut base = ItemEntity::new_empty();
        base.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::ExternalId,
                "P214",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::StringType,
                    Value::StringValue("111".to_string()),
                )),
            ),
            vec![],
            vec![],
        ));

        let mut other = ItemEntity::new_empty();
        other.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::ExternalId,
                "P214",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::StringType,
                    Value::StringValue("222".to_string()),
                )),
            ),
            vec![],
            vec![],
        ));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert_eq!(
            diff.added_statements.len(),
            1,
            "different external ID must be added"
        );
        assert_eq!(im.item().claims().len(), 2);
    }

    #[test]
    fn test_add_claim_new_qualifier_appended_to_existing() {
        // Qualifier merging only fires when the property is in
        // properties_ignore_qualifier_match (otherwise a non-identical qualifier
        // set causes the incoming claim to be treated as a brand-new statement).
        // Here we opt P1 into the ignore list so both claims match and the extra
        // qualifier from the incoming claim gets merged into the existing one.
        let mut base = ItemEntity::new_empty();
        let mut stmt = Statement::new_normal(
            Snak::new_string("P1", "value"),
            vec![Snak::new_string("P2", "existing-qual")],
            vec![],
        );
        stmt.set_id("Q1$abc");
        base.add_claim(stmt);

        let mut other = ItemEntity::new_empty();
        other.add_claim(Statement::new_normal(
            Snak::new_string("P1", "value"),
            vec![
                Snak::new_string("P2", "existing-qual"), // already present
                Snak::new_string("P3", "new-qual"),      // new
            ],
            vec![],
        ));

        let mut im = ItemMerger::new(base);
        // Allow qualifier mismatches for P1 so the merger considers them the same claim.
        im.set_properties_ignore_qualifier_match(vec!["P1".to_string()]);
        let diff = im.merge(&other);

        assert!(
            !diff.altered_statements.is_empty(),
            "statement must appear in diff when a new qualifier is merged in"
        );
        assert_eq!(
            im.item().claims()[0].qualifiers().len(),
            2,
            "both qualifiers must be present after merge"
        );
    }

    #[test]
    fn test_add_claim_different_qualifiers_without_ignore_adds_new_claim() {
        // Without the ignore list, a claim with a different qualifier set is treated
        // as a distinct statement and added alongside the original.
        let mut base = ItemEntity::new_empty();
        base.add_claim(Statement::new_normal(
            Snak::new_string("P1", "value"),
            vec![Snak::new_string("P2", "qual-a")],
            vec![],
        ));

        let mut other = ItemEntity::new_empty();
        other.add_claim(Statement::new_normal(
            Snak::new_string("P1", "value"),
            vec![Snak::new_string("P2", "qual-b")], // different qualifier value
            vec![],
        ));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert_eq!(
            diff.added_statements.len(),
            1,
            "claim with different qualifiers must be added as a new statement"
        );
        assert_eq!(im.item().claims().len(), 2);
    }

    #[test]
    fn test_add_claim_bare_not_duplicated_against_qualified() {
        // Regression test for https://github.com/magnusmanske/auth2wd/issues/10 :
        // an existing statement carrying an extra qualifier (e.g. P1810) must not gain a
        // duplicate when a source supplies the same value with no qualifiers.
        let mut base = ItemEntity::new_empty();
        let mut stmt = Statement::new_normal(
            Snak::new_external_id("P691", "jn19990210001"),
            vec![Snak::new_string("P1810", "Some Name")],
            vec![],
        );
        stmt.set_id("Q234888$existing");
        base.add_claim(stmt);

        let mut other = ItemEntity::new_empty();
        other.add_claim(Statement::new_normal(
            Snak::new_external_id("P691", "jn19990210001"),
            vec![],
            vec![],
        ));

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert!(
            diff.added_statements.is_empty(),
            "bare statement must not be added as a duplicate"
        );
        assert_eq!(im.item().claims().len(), 1);
        assert_eq!(im.item().claims()[0].qualifiers().len(), 1);
    }

    #[test]
    fn test_are_qualifiers_compatible() {
        let a = vec![Snak::new_string("P1", "a")];
        let ab = vec![Snak::new_string("P1", "a"), Snak::new_string("P2", "b")];
        let empty: Vec<Snak> = vec![];
        // empty is a subset of anything
        assert!(ItemMerger::are_qualifiers_compatible(&empty, &a));
        assert!(ItemMerger::are_qualifiers_compatible(&a, &empty));
        // subset in either direction is compatible
        assert!(ItemMerger::are_qualifiers_compatible(&a, &ab));
        assert!(ItemMerger::are_qualifiers_compatible(&ab, &a));
        // non-overlapping qualifiers are not compatible
        let c = vec![Snak::new_string("P1", "c")];
        assert!(!ItemMerger::are_qualifiers_compatible(&a, &c));
    }

    #[test]
    fn test_into_item_consumes_merger_and_returns_merged_entity() {
        let mut base = ItemEntity::new_empty();
        base.labels_mut().push(LocaleString::new("en", "kept"));

        let mut other = ItemEntity::new_empty();
        other.labels_mut().push(LocaleString::new("de", "neu"));

        let mut im = ItemMerger::new(base);
        let _ = im.merge(&other);

        let merged = im.into_item();
        assert_eq!(merged.labels().len(), 2);
        assert!(merged.labels().iter().any(|l| l.language() == "en"));
        assert!(merged.labels().iter().any(|l| l.language() == "de"));
    }

    // --- Date precision and claim ranking ---

    /// A `Statement` whose main snak is a time value of the given precision.
    fn time_statement(prop: &str, time: &str, precision: u64) -> Statement {
        Statement::new_normal(
            Snak::new(
                SnakDataType::Time,
                prop,
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Time,
                    Value::Time(TimeValue::new(
                        0,
                        0,
                        "http://www.wikidata.org/entity/Q1985727",
                        precision,
                        time,
                        0,
                    )),
                )),
            ),
            vec![],
            vec![],
        )
    }

    #[test]
    fn test_check_new_claim_for_dates_deprecates_lower_precision() {
        // The item already knows the birth date to the day (precision 11); a new,
        // vaguer year-only claim (precision 9) must be added as deprecated so it
        // does not compete with the better one.
        let mut base = ItemEntity::new_empty();
        base.add_claim(time_statement("P569", "+1952-03-11T00:00:00Z", 11));

        let im = ItemMerger::new(base);
        let mut new_claim = time_statement("P569", "+1952-00-00T00:00:00Z", 9);
        im.check_new_claim_for_dates(&mut new_claim);

        assert_eq!(*new_claim.rank(), StatementRank::Deprecated);
    }

    #[test]
    fn test_check_new_claim_for_dates_keeps_equal_precision_normal() {
        // Boundary: the check is `<`, not `<=`, so an equally precise claim keeps
        // its rank.
        let mut base = ItemEntity::new_empty();
        base.add_claim(time_statement("P570", "+2001-05-11T00:00:00Z", 11));

        let im = ItemMerger::new(base);
        let mut new_claim = time_statement("P570", "+2001-05-12T00:00:00Z", 11);
        im.check_new_claim_for_dates(&mut new_claim);

        assert_eq!(*new_claim.rank(), StatementRank::Normal);
    }

    #[test]
    fn test_check_new_claim_for_dates_keeps_higher_precision_normal() {
        let mut base = ItemEntity::new_empty();
        base.add_claim(time_statement("P569", "+1952-00-00T00:00:00Z", 9));

        let im = ItemMerger::new(base);
        let mut new_claim = time_statement("P569", "+1952-03-11T00:00:00Z", 11);
        im.check_new_claim_for_dates(&mut new_claim);

        assert_eq!(*new_claim.rank(), StatementRank::Normal);
    }

    #[test]
    fn test_check_new_claim_for_dates_ignores_non_time_main_snak() {
        // A P569 claim carrying a string rather than a time must be left alone.
        let im = ItemMerger::new(ItemEntity::new_empty());
        let mut new_claim =
            Statement::new_normal(Snak::new_string("P569", "not a date"), vec![], vec![]);
        im.check_new_claim_for_dates(&mut new_claim);

        assert_eq!(*new_claim.rank(), StatementRank::Normal);
    }

    #[test]
    fn test_check_new_claim_for_dates_ignores_non_time_existing_claims() {
        // Existing P569 claims that are not time values contribute no precision,
        // so `best_existing_precision` falls back to 0 and nothing is deprecated.
        let mut base = ItemEntity::new_empty();
        base.add_claim(Statement::new_normal(
            Snak::new_string("P569", "junk"),
            vec![],
            vec![],
        ));

        let im = ItemMerger::new(base);
        let mut new_claim = time_statement("P569", "+1952-00-00T00:00:00Z", 9);
        im.check_new_claim_for_dates(&mut new_claim);

        assert_eq!(*new_claim.rank(), StatementRank::Normal);
    }

    #[test]
    fn test_is_data_value_identical_compares_time_values_by_precision() {
        // Reaching the truncated-precision comparison through the data-value
        // entry point, not just via is_time_value_identical directly.
        let dv = |time: &str, precision: u64| {
            Some(DataValue::new(
                DataValueType::Time,
                Value::Time(TimeValue::new(
                    0,
                    0,
                    "http://www.wikidata.org/entity/Q1985727",
                    precision,
                    time,
                    0,
                )),
            ))
        };

        // Precision 9 (year): month/day are noise and must be ignored.
        assert!(ItemMerger::is_data_value_identical(
            &dv("+1650-00-00T00:00:00Z", 9),
            &dv("+1650-12-29T00:00:00Z", 9)
        ));
        assert!(!ItemMerger::is_data_value_identical(
            &dv("+1650-00-00T00:00:00Z", 9),
            &dv("+1651-00-00T00:00:00Z", 9)
        ));
        // Precision 11 (day) falls through to the exact-equality arm.
        assert!(ItemMerger::is_data_value_identical(
            &dv("+1650-12-29T00:00:00Z", 11),
            &dv("+1650-12-29T00:00:00Z", 11)
        ));
        assert!(!ItemMerger::is_data_value_identical(
            &dv("+1650-12-29T00:00:00Z", 11),
            &dv("+1650-12-28T00:00:00Z", 11)
        ));
    }

    #[test]
    fn test_is_data_value_identical_non_time_values_use_equality() {
        let a = Some(DataValue::new(
            DataValueType::StringType,
            Value::StringValue("foo".to_string()),
        ));
        let b = Some(DataValue::new(
            DataValueType::StringType,
            Value::StringValue("bar".to_string()),
        ));
        assert!(ItemMerger::is_data_value_identical(&a, &a.clone()));
        assert!(!ItemMerger::is_data_value_identical(&a, &b));
        assert!(ItemMerger::is_data_value_identical(&None, &None));
        assert!(!ItemMerger::is_data_value_identical(&a, &None));
    }

    #[test]
    fn test_is_time_value_identical_unhandled_precision_uses_exact_equality() {
        // Precisions other than 9 and 10 take the `_` arm, which compares the
        // whole TimeValue rather than a truncated string.
        let t = |time: &str| {
            TimeValue::new(
                0,
                0,
                "http://www.wikidata.org/entity/Q1985727",
                7, // century — neither of the special-cased precisions
                time,
                0,
            )
        };
        assert!(ItemMerger::is_time_value_identical(
            &t("+1900-00-00T00:00:00Z"),
            &t("+1900-00-00T00:00:00Z")
        ));
        assert!(!ItemMerger::is_time_value_identical(
            &t("+1900-00-00T00:00:00Z"),
            &t("+1800-00-00T00:00:00Z")
        ));
    }

    #[test]
    fn test_merge_sitelinks_into_item_with_existing_sitelinks() {
        // Only sites the base item does not already have are added.
        let mut base = ItemEntity::new_empty();
        base.sitelinks_mut()
            .replace(vec![SiteLink::new("enwiki", "Foo", vec![])]);

        let mut other = ItemEntity::new_empty();
        other.sitelinks_mut().replace(vec![
            SiteLink::new("enwiki", "Foo (different title, ignored)", vec![]),
            SiteLink::new("dewiki", "Foo", vec![]),
        ]);

        let mut im = ItemMerger::new(base);
        let diff = im.merge(&other);

        assert_eq!(diff.sitelinks.len(), 1);
        assert_eq!(diff.sitelinks[0].site(), "dewiki");
        assert_eq!(im.item().sitelinks().as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_merge_creates_sitelink_list_when_base_item_has_none() {
        // A fresh base item has `sitelinks() == None`; the incoming links must
        // still reach both the diff and the merged item.
        let mut other = ItemEntity::new_empty();
        other.sitelinks_mut().replace(vec![
            SiteLink::new("enwiki", "Foo", vec![]),
            SiteLink::new("dewiki", "Foo", vec![]),
        ]);

        let mut im = ItemMerger::new(ItemEntity::new_empty());
        let diff = im.merge(&other);

        assert_eq!(diff.sitelinks.len(), 2);
        let merged = im
            .item()
            .sitelinks()
            .as_ref()
            .expect("list must be created");
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|s| s.site() == "enwiki"));
        assert!(merged.iter().any(|s| s.site() == "dewiki"));
    }

    #[test]
    fn test_merge_leaves_none_sitelinks_alone_when_other_has_none_to_add() {
        // Merging an item that has an empty sitelink list must not materialise
        // one on the base item.
        let mut other = ItemEntity::new_empty();
        other.sitelinks_mut().replace(vec![]);

        let mut im = ItemMerger::new(ItemEntity::new_empty());
        let diff = im.merge(&other);

        assert!(diff.sitelinks.is_empty());
        assert!(im.item().sitelinks().is_none());
    }

    #[test]
    fn test_compare_snak_same_property_orders_by_value() {
        // Same property falls through to comparing the serialised data values,
        // which is what makes qualifier-list sorting deterministic.
        let a = Snak::new_string("P31", "aaa");
        let b = Snak::new_string("P31", "bbb");
        assert_eq!(ItemMerger::compare_snak(&a, &b), Ordering::Less);
        assert_eq!(ItemMerger::compare_snak(&b, &a), Ordering::Greater);
        assert_eq!(ItemMerger::compare_snak(&a, &a.clone()), Ordering::Equal);

        // Different properties are ordered by property alone -- and lexically,
        // not numerically, so "P31" sorts *after* "P279". That is fine for the
        // deterministic qualifier sort this backs, but is worth pinning so the
        // ordering is not mistaken for numeric.
        let c = Snak::new_string("P279", "aaa");
        assert_eq!(ItemMerger::compare_snak(&a, &c), Ordering::Greater);
        assert_eq!(ItemMerger::compare_snak(&c, &a), Ordering::Less);
    }

    #[test]
    fn test_get_external_ids_from_reference_skips_non_string_values() {
        // An ExternalId-typed snak whose value is not a string must be skipped
        // rather than producing a bogus ExternalId.
        let reference = Reference::new(vec![
            Snak::new(
                SnakDataType::ExternalId,
                "P214",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::EntityId,
                    Value::Entity(EntityValue::new(EntityType::Item, "Q42")),
                )),
            ),
            Snak::new_external_id("P227", "12345"),
        ]);

        let ids = ItemMerger::get_external_ids_from_reference(&reference);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], ExternalId::new(227, "12345"));
    }

    #[test]
    fn test_get_reference_urls_skips_non_string_values() {
        // A Url-typed snak whose value is not a string must be skipped.
        let reference = Reference::new(vec![
            Snak::new(
                SnakDataType::Url,
                "P854",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::EntityId,
                    Value::Entity(EntityValue::new(EntityType::Item, "Q42")),
                )),
            ),
            Snak::new_url("P854", "http://example.com"),
        ]);

        let urls = ItemMerger::get_reference_urls_from_reference(&reference);
        assert_eq!(urls, vec!["http://example.com".to_string()]);
    }
    #[test]
    fn test_all_static_regexes_compile() {
        // The statics degrade to exact comparison rather than panicking, so
        // assert here that they are in fact available.
        assert!(YEAR_FIX.is_some());
        assert!(MONTH_FIX.is_some());
    }
}
