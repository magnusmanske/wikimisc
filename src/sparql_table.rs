use crate::{file_vec::FileVec, sparql_results::SparqlApiResult, sparql_value::SparqlValue};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SparqlTable {
    headers: Vec<String>,
    rows: FileVec<Vec<SparqlValue>>,
    main_variable: Option<String>,
}

impl Default for SparqlTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SparqlTable {
    /// Create a new SparqlTable.
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
            rows: FileVec::new(),
            main_variable: None,
        }
    }

    /// Create a new SparqlTable from another SparqlTable, using its headers (not the rows).
    pub fn from_table(other: &SparqlTable) -> Self {
        Self {
            headers: other.headers.clone(),
            rows: FileVec::new(),
            main_variable: other.main_variable.clone(),
        }
    }

    /// Return the number of rows in the table.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Return true if the table is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Get the value of a cell in the table. Returns None if the row or column does not exist.
    pub fn get_row_col(&self, row_id: usize, col_id: usize) -> Option<SparqlValue> {
        self.rows.get(row_id)?.get(col_id).map(|v| v.to_owned())
    }

    /// Get the index of a variable in the table. Case-insensitive.
    pub fn get_var_index(&self, var: &str) -> Option<usize> {
        let var = var.to_lowercase();
        self.headers
            .iter()
            .enumerate()
            .find(|(_num, name)| name.to_lowercase() == var)
            .map(|(num, _)| num)
    }

    /// Push a row to the table.
    pub fn push(&mut self, row: Vec<SparqlValue>) {
        self.rows.push(row);
    }

    /// Get a row from the table. Returns None if the row does not exist.
    pub fn get(&self, row_id: usize) -> Option<Vec<SparqlValue>> {
        self.rows.get(row_id).map(|r| r.to_owned())
    }

    fn push_sparql_result_row(&mut self, row: &HashMap<String, SparqlValue>) {
        if self.headers.is_empty() {
            panic!("Header not set");
            // self.headers = row
            //     .iter()
            //     .map(|(k, _)| k.clone())
            //     .collect();
        }
        let new_row: Vec<SparqlValue> = self
            .headers
            .iter()
            .map(|name| row.get(name).cloned().unwrap_or_else(|| SparqlValue::None))
            .collect();
        self.push(new_row);
    }

    /// Return the main variable of the table, if set.
    pub fn main_variable(&self) -> Option<&String> {
        self.main_variable.as_ref()
    }

    /// Set the main variable of the table.
    pub fn set_main_variable(&mut self, main_variable: Option<String>) {
        self.main_variable = main_variable;
    }

    /// Return the index of the main variable in the table, if set.
    pub fn main_column(&self) -> Option<usize> {
        let mv = self.main_variable.as_ref()?;
        self.headers.iter().position(|header| header == mv)
    }

    pub fn set_headers(&mut self, headers: Vec<String>) {
        self.headers = headers;
    }

    /// Consumes `result`.
    pub fn from_api_result(result: SparqlApiResult) -> Self {
        let mut table = Self::new();
        let headers = result
            .head()
            .get("vars")
            .map(|v| v.to_owned())
            .unwrap_or_default();
        table.set_headers(headers);
        for row in result.bindings() {
            table.push_sparql_result_row(row);
        }
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push() {
        let mut table = SparqlTable::new();
        table.push(vec![SparqlValue::Literal("a".to_string())]);
        table.push(vec![SparqlValue::Literal("b".to_string())]);
        table.push(vec![SparqlValue::Literal("c".to_string())]);
        assert_eq!(table.len(), 3);
        assert_eq!(
            table.get(0),
            Some(vec![SparqlValue::Literal("a".to_string())])
        );
        assert_eq!(
            table.get(1),
            Some(vec![SparqlValue::Literal("b".to_string())])
        );
        assert_eq!(
            table.get(2),
            Some(vec![SparqlValue::Literal("c".to_string())])
        );
    }

    #[test]
    fn test_get_var_index() {
        let mut table = SparqlTable::new();
        table.headers.push("a".to_string());
        table.headers.push("b".to_string());
        assert_eq!(table.get_var_index("a"), Some(0));
        assert_eq!(table.get_var_index("b"), Some(1));
        assert_eq!(table.get_var_index("c"), None);
    }

    #[test]
    fn test_main_column() {
        let mut table = SparqlTable::new();
        table.headers.push("a".to_string());
        table.headers.push("b".to_string());
        table.set_main_variable(Some("b".to_string()));
        assert_eq!(table.main_column(), Some(1));
    }
}
