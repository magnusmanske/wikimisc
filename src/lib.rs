//! Provides useful functionality for working with MediaWiki and WikiBase.
//! based on the `wikibase` and `mediawiki` crates.
//!
//! The crate has **no default features**: every module is gated, so a consumer
//! selects only the functionality — and dependency tree — it actually needs.
//! See the feature table in `README.md`, or enable `full` to get everything.

// A library must not take its caller's process down, so production code paths
// do not panic: fallible operations return one of the per-feature error types
// instead. The few unavoidable exceptions carry an explicit `#[allow]` plus a
// comment justifying why there is no way to continue. Test code may panic
// freely — that is what an assertion failure is — hence `not(test)`.
#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

#[cfg(feature = "date")]
pub mod date;
#[cfg(feature = "external-id")]
pub mod external_id;
#[cfg(feature = "item-merger")]
pub mod item_merger;
#[cfg(feature = "lat-lon")]
pub mod lat_lon;
#[cfg(feature = "item-merger")]
pub mod merge_diff;
#[cfg(feature = "seppuku")]
pub mod seppuku;
#[cfg(feature = "site-matrix")]
pub mod site_matrix;
#[cfg(feature = "sparql")]
pub mod sparql_results;
#[cfg(feature = "sparql-table")]
pub mod sparql_table;
#[cfg(feature = "sparql-table")]
pub mod sparql_table_trait;
#[cfg(feature = "sparql-table")]
pub mod sparql_table_vec;
#[cfg(feature = "sparql")]
pub mod sparql_value;
#[cfg(feature = "date")]
pub mod timestamp;
#[cfg(feature = "toolforge")]
pub mod toolforge_app;
#[cfg(feature = "database")]
pub mod toolforge_db;
#[cfg(feature = "wikidata")]
pub mod wikidata;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(feature = "database")]
pub use mysql_async;
#[cfg(feature = "toolforge")]
pub use toolforge;
#[cfg(feature = "wikibase")]
pub use wikibase;
#[cfg(feature = "wikibase")]
pub use wikibase::mediawiki;
