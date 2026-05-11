//! Backwards-compatibility re-export. The in-memory SPARQL table now lives in
//! [`crate::sparql_table`] as a type alias `SparqlTableVec = SparqlTable<Vec<SparqlRow>>`.

pub use crate::sparql_table::SparqlTableVec;
