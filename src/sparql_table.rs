//! Tabular SPARQL results.
//!
//! [`SparqlTable`] is generic over its row container. The default backend is
//! disk-spilling ([`FileVec<SparqlRow>`]); for fully in-memory tables use the
//! [`SparqlTableVec`] type alias.
//!
//! Choose [`SparqlTableVec`] when results fit comfortably in memory and the
//! per-row allocation cost matters; choose [`SparqlTable`] (disk-backed) when
//! the result set could grow large enough to exhaust RAM.

use crate::{
    file_vec::FileVec,
    sparql_results::{SparqlApiResult, SparqlRow},
    sparql_table_trait::SparqlTableTrait,
    sparql_value::SparqlValue,
};
use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// Row-container abstraction shared by the in-memory and disk-spilling
/// [`SparqlTable`] backends. Implemented for `Vec<SparqlRow>` and
/// `FileVec<SparqlRow>`.
///
/// `push` returns `Result` because disk-backed implementations may fail on I/O.
/// The in-memory `Vec` impl always returns `Ok(())`.
pub trait RowStorage: Default {
    fn push(&mut self, row: SparqlRow) -> Result<()>;
    fn get(&self, idx: usize) -> Option<SparqlRow>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl RowStorage for Vec<SparqlRow> {
    fn push(&mut self, row: SparqlRow) -> Result<()> {
        Vec::push(self, row);
        Ok(())
    }
    fn get(&self, idx: usize) -> Option<SparqlRow> {
        self.as_slice().get(idx).cloned()
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

impl RowStorage for FileVec<SparqlRow> {
    fn push(&mut self, row: SparqlRow) -> Result<()> {
        FileVec::push(self, row)
    }
    fn get(&self, idx: usize) -> Option<SparqlRow> {
        FileVec::get(self, idx)
    }
    fn len(&self) -> usize {
        FileVec::len(self)
    }
}

/// Tabular SPARQL result set. Generic over the row container.
#[derive(Debug, Clone)]
pub struct SparqlTable<S: RowStorage = FileVec<SparqlRow>> {
    headers: Vec<String>,
    rows: S,
    main_variable: Option<String>,
}

/// In-memory alias for [`SparqlTable`] backed by a plain `Vec`.
pub type SparqlTableVec = SparqlTable<Vec<SparqlRow>>;

impl<S: RowStorage> Default for SparqlTable<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: RowStorage> SparqlTable<S> {
    /// Create a new empty table.
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
            rows: S::default(),
            main_variable: None,
        }
    }

    /// Create a new table that copies the headers and main variable from `other` but no rows.
    pub fn from_table<T: RowStorage>(other: &SparqlTable<T>) -> Self {
        Self {
            headers: other.headers.clone(),
            rows: S::default(),
            main_variable: other.main_variable.clone(),
        }
    }

    /// Number of rows in the table.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Get the value of a cell. Returns `None` if the row or column is out of range.
    pub fn get_row_col(&self, row_id: usize, col_id: usize) -> Option<SparqlValue> {
        self.rows
            .get(row_id)?
            .get(col_id)
            .and_then(|v| v.to_owned())
    }

    /// Zero-based column index of a variable name (case-insensitive).
    pub fn get_var_index(&self, var: &str) -> Option<usize> {
        let var = var.to_lowercase();
        self.headers
            .iter()
            .position(|name| name.to_lowercase() == var)
    }

    /// Append a row. Returns `Err` if the underlying [`RowStorage`] fails (e.g. disk I/O).
    pub fn push(&mut self, row: SparqlRow) -> Result<()> {
        self.rows.push(row)
    }

    /// Get a row by index. Returns `None` if out of range.
    pub fn get(&self, row_id: usize) -> Option<SparqlRow> {
        self.rows.get(row_id)
    }

    fn push_sparql_result_row(&mut self, row: &HashMap<String, SparqlValue>) -> Result<()> {
        if self.headers.is_empty() {
            return Err(anyhow!("Header not set"));
        }
        let new_row: SparqlRow = self
            .headers
            .iter()
            .map(|name| row.get(name).cloned())
            .collect();
        self.push(new_row)
    }

    /// The main variable name, if set.
    pub fn main_variable(&self) -> Option<&String> {
        self.main_variable.as_ref()
    }

    /// Set the main variable name.
    pub fn set_main_variable(&mut self, main_variable: Option<String>) {
        self.main_variable = main_variable;
    }

    /// Column index of the main variable, if set.
    pub fn main_column(&self) -> Option<usize> {
        let mv = self.main_variable.as_ref()?;
        self.headers.iter().position(|header| header == mv)
    }

    /// Replace the header list.
    pub fn set_headers(&mut self, headers: Vec<String>) {
        self.headers = headers;
    }

    /// Build a table from a deserialised SPARQL API result. Consumes `result`.
    pub fn from_api_result(result: SparqlApiResult) -> Result<Self> {
        let mut table = Self::new();
        let headers = result
            .head()
            .get("vars")
            .map(|v| v.to_owned())
            .unwrap_or_default();
        table.set_headers(headers);
        for row in result.bindings() {
            table.push_sparql_result_row(row)?;
        }
        Ok(table)
    }
}

impl<S: RowStorage> SparqlTableTrait for SparqlTable<S> {
    fn len(&self) -> usize {
        self.len()
    }

    fn get_row_col(&self, row_id: usize, col_id: usize) -> Option<SparqlValue> {
        self.get_row_col(row_id, col_id)
    }

    fn get_var_index(&self, var: &str) -> Option<usize> {
        self.get_var_index(var)
    }

    fn push(&mut self, row: SparqlRow) -> Result<()> {
        self.push(row)
    }

    fn get(&self, row_id: usize) -> Option<SparqlRow> {
        self.get(row_id)
    }

    fn main_variable(&self) -> Option<&String> {
        self.main_variable()
    }

    fn set_main_variable(&mut self, main_variable: Option<String>) {
        self.set_main_variable(main_variable);
    }

    fn main_column(&self) -> Option<usize> {
        self.main_column()
    }

    fn set_headers(&mut self, headers: Vec<String>) {
        self.set_headers(headers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the same assertions against both backends so they cannot drift.
    fn assert_table_behaviour<S: RowStorage>() {
        let mut table = SparqlTable::<S>::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
        assert_eq!(table.get(0), None);
        assert_eq!(table.get_row_col(0, 0), None);

        table.set_headers(vec!["a".to_string(), "b".to_string()]);
        table.set_main_variable(Some("b".to_string()));

        assert_eq!(table.get_var_index("a"), Some(0));
        assert_eq!(table.get_var_index("b"), Some(1));
        assert_eq!(table.get_var_index("MYVAR"), None);
        assert_eq!(table.main_column(), Some(1));
        assert_eq!(table.main_variable(), Some(&"b".to_string()));

        table
            .push(vec![Some(SparqlValue::Literal("a".to_string()))])
            .unwrap();
        table
            .push(vec![Some(SparqlValue::Literal("b".to_string()))])
            .unwrap();
        table
            .push(vec![Some(SparqlValue::Literal("c".to_string()))])
            .unwrap();
        assert_eq!(table.len(), 3);
        assert!(!table.is_empty());
        assert_eq!(
            table.get(0),
            Some(vec![Some(SparqlValue::Literal("a".to_string()))])
        );
        assert_eq!(
            table.get(2),
            Some(vec![Some(SparqlValue::Literal("c".to_string()))])
        );
    }

    #[test]
    fn test_table_behaviour_filevec_backend() {
        assert_table_behaviour::<FileVec<SparqlRow>>();
    }

    #[test]
    fn test_table_behaviour_vec_backend() {
        assert_table_behaviour::<Vec<SparqlRow>>();
    }

    #[test]
    fn test_get_var_index_case_insensitive() {
        let mut table = SparqlTableVec::new();
        table.set_headers(vec!["MyVar".to_string()]);
        assert_eq!(table.get_var_index("myvar"), Some(0));
        assert_eq!(table.get_var_index("MYVAR"), Some(0));
    }

    #[test]
    fn test_get_row_col_out_of_bounds() {
        let mut table = SparqlTableVec::new();
        table
            .push(vec![Some(SparqlValue::Literal("a".to_string()))])
            .unwrap();
        assert_eq!(table.get_row_col(99, 0), None);
        assert_eq!(table.get_row_col(0, 99), None);
    }

    #[test]
    fn test_main_column_not_set() {
        let table = SparqlTableVec::new();
        assert_eq!(table.main_column(), None);
    }

    #[test]
    fn test_from_table_copies_headers_not_rows() {
        let mut original = SparqlTableVec::new();
        original.set_headers(vec!["x".to_string(), "y".to_string()]);
        original.set_main_variable(Some("x".to_string()));
        original
            .push(vec![Some(SparqlValue::Literal("a".to_string()))])
            .unwrap();

        let copy: SparqlTableVec = SparqlTable::from_table(&original);
        assert_eq!(copy.headers, original.headers);
        assert_eq!(copy.main_variable, original.main_variable);
        assert_eq!(copy.len(), 0);
    }

    #[test]
    fn test_from_table_cross_backend() {
        // Headers/main_variable must copy even across backends.
        let mut src: SparqlTableVec = SparqlTableVec::new();
        src.set_headers(vec!["foo".to_string()]);
        src.set_main_variable(Some("foo".to_string()));
        let copy: SparqlTable = SparqlTable::from_table(&src);
        assert_eq!(copy.main_variable(), Some(&"foo".to_string()));
        assert_eq!(copy.get_var_index("foo"), Some(0));
    }

    // ── from_api_result ──────────────────────────────────────────────────────

    fn make_api_result(json: serde_json::Value) -> SparqlApiResult {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn test_from_api_result_basic() {
        let result = make_api_result(serde_json::json!({
            "head": {"vars": ["item", "label"]},
            "results": {"bindings": [
                {
                    "item":  {"type": "uri",     "value": "http://www.wikidata.org/entity/Q42"},
                    "label": {"type": "literal", "value": "Douglas Adams"}
                }
            ]}
        }));

        let table: SparqlTable = SparqlTable::from_api_result(result).unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(table.get_var_index("item"), Some(0));
        assert_eq!(
            table.get_row_col(0, 0),
            Some(SparqlValue::Entity("Q42".to_string()))
        );
    }

    #[test]
    fn test_from_api_result_empty_bindings_both_backends() {
        let json = serde_json::json!({
            "head": {"vars": ["x", "y"]},
            "results": {"bindings": []}
        });
        let r1 = make_api_result(json.clone());
        let r2 = make_api_result(json);
        let t1: SparqlTable = SparqlTable::from_api_result(r1).unwrap();
        let t2: SparqlTableVec = SparqlTable::from_api_result(r2).unwrap();
        assert!(t1.is_empty());
        assert!(t2.is_empty());
        assert_eq!(t1.get_var_index("x"), Some(0));
        assert_eq!(t2.get_var_index("x"), Some(0));
    }

    #[test]
    fn test_from_api_result_no_vars_in_head_is_err() {
        let result = make_api_result(serde_json::json!({
            "head": {},
            "results": {"bindings": [
                {"x": {"type": "literal", "value": "v"}}
            ]}
        }));
        assert!(SparqlTable::<FileVec<SparqlRow>>::from_api_result(result).is_err());
    }

    #[test]
    fn test_from_api_result_missing_variable_in_row_becomes_none() {
        let result = make_api_result(serde_json::json!({
            "head": {"vars": ["item", "label"]},
            "results": {"bindings": [
                {"item": {"type": "uri", "value": "http://www.wikidata.org/entity/Q1"}}
            ]}
        }));
        let table: SparqlTableVec = SparqlTable::from_api_result(result).unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.get_row_col(0, 0),
            Some(SparqlValue::Entity("Q1".to_string()))
        );
        assert_eq!(table.get_row_col(0, 1), None);
    }
}
