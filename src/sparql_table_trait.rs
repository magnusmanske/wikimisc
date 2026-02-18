//! Shared trait for [`crate::sparql_table::SparqlTable`] and
//! [`crate::sparql_table_vec::SparqlTableVec`], which are identical in interface
//! but differ in their row-storage backend (disk-backed `FileVec` vs in-memory `Vec`).

use crate::sparql_results::SparqlRow;
use crate::sparql_value::SparqlValue;

/// Common interface shared by [`crate::sparql_table::SparqlTable`] and
/// [`crate::sparql_table_vec::SparqlTableVec`].
///
/// This trait exists to allow code to be generic over the two storage backends
/// without duplicating logic. Use `SparqlTableVec` when all data fits in memory;
/// use `SparqlTable` (backed by `FileVec`) for large result sets.
pub trait SparqlTableTrait {
    /// Return the number of rows.
    fn len(&self) -> usize;

    /// Return `true` if there are no rows.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the value of a cell. Returns `None` if the row or column is out of range.
    fn get_row_col(&self, row_id: usize, col_id: usize) -> Option<SparqlValue>;

    /// Get the zero-based column index for a variable name (case-insensitive).
    fn get_var_index(&self, var: &str) -> Option<usize>;

    /// Append a row.
    fn push(&mut self, row: SparqlRow);

    /// Get a row by index. Returns `None` if out of range.
    fn get(&self, row_id: usize) -> Option<SparqlRow>;

    /// Return the main variable name, if set.
    fn main_variable(&self) -> Option<&String>;

    /// Set the main variable name.
    fn set_main_variable(&mut self, main_variable: Option<String>);

    /// Return the column index of the main variable, if set.
    fn main_column(&self) -> Option<usize>;

    /// Replace the header list.
    fn set_headers(&mut self, headers: Vec<String>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql_table::SparqlTable;
    use crate::sparql_table_vec::SparqlTableVec;

    /// Runs the same assertions against any `SparqlTableTrait` implementation,
    /// verifying both backends behave identically.
    fn assert_table_behaviour(table: &mut dyn SparqlTableTrait) {
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
        assert_eq!(table.get(0), None);
        assert_eq!(table.get_row_col(0, 0), None);

        table.set_headers(vec!["item".to_string(), "label".to_string()]);
        table.set_main_variable(Some("item".to_string()));

        assert_eq!(table.get_var_index("item"), Some(0));
        assert_eq!(table.get_var_index("LABEL"), Some(1)); // case-insensitive
        assert_eq!(table.get_var_index("missing"), None);
        assert_eq!(table.main_column(), Some(0));
        assert_eq!(table.main_variable(), Some(&"item".to_string()));

        table.push(vec![
            Some(SparqlValue::Entity("Q1".to_string())),
            Some(SparqlValue::Literal("Earth".to_string())),
        ]);
        table.push(vec![Some(SparqlValue::Entity("Q2".to_string())), None]);

        assert!(!table.is_empty());
        assert_eq!(table.len(), 2);
        assert_eq!(
            table.get_row_col(0, 0),
            Some(SparqlValue::Entity("Q1".to_string()))
        );
        assert_eq!(
            table.get_row_col(0, 1),
            Some(SparqlValue::Literal("Earth".to_string()))
        );
        assert_eq!(table.get_row_col(1, 1), None);
        assert_eq!(table.get_row_col(99, 0), None);
    }

    #[test]
    fn test_sparql_table_trait_vec_backend() {
        let mut table = SparqlTableVec::new();
        assert_table_behaviour(&mut table);
    }

    #[test]
    fn test_sparql_table_trait_file_backend() {
        let mut table = SparqlTable::new();
        assert_table_behaviour(&mut table);
    }
}
