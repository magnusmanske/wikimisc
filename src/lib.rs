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

#[cfg(feature = "title")]
pub mod commons;
#[cfg(feature = "sparql-client")]
mod content_type;
#[cfg(feature = "date")]
pub mod date;
#[cfg(feature = "entity-id")]
pub mod entity_id;
#[cfg(feature = "escape")]
pub mod escape;
#[cfg(feature = "external-id")]
pub mod external_id;
#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "item-merger")]
pub mod item_merger;
#[cfg(feature = "jsonp")]
pub mod jsonp;
#[cfg(feature = "labels")]
pub mod labels;
#[cfg(feature = "lat-lon")]
pub mod lat_lon;
#[cfg(feature = "logging")]
pub mod logging;
#[cfg(feature = "item-merger")]
pub mod merge_diff;
#[cfg(feature = "process-stats")]
pub mod process_stats;
#[cfg(feature = "retry")]
pub mod retry;
#[cfg(feature = "seppuku")]
pub mod seppuku;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "shutdown")]
pub mod shutdown;
#[cfg(feature = "site-matrix")]
pub mod site_matrix;
#[cfg(feature = "sparql-client")]
pub mod sparql_client;
#[cfg(feature = "sparql")]
pub mod sparql_endpoint;
#[cfg(feature = "sparql")]
pub(crate) mod sparql_lexer;
#[cfg(feature = "sparql")]
pub mod sparql_results;
#[cfg(feature = "sparql-client")]
pub mod sparql_retry;
#[cfg(feature = "sparql")]
pub mod sparql_rewrite;
#[cfg(feature = "sparql-table")]
pub mod sparql_table;
#[cfg(feature = "sparql-table")]
pub mod sparql_table_trait;
#[cfg(feature = "sparql-table")]
pub mod sparql_table_vec;
#[cfg(feature = "sparql")]
pub mod sparql_tsv;
#[cfg(feature = "sparql")]
pub mod sparql_util;
#[cfg(feature = "sparql")]
pub mod sparql_value;
#[cfg(feature = "statements")]
pub mod statements;
#[cfg(feature = "telemetry")]
pub mod telemetry;
#[cfg(feature = "telemetry")]
mod telemetry_recorder;
#[cfg(feature = "date")]
pub mod timestamp;
#[cfg(feature = "title")]
pub mod title;
#[cfg(feature = "toolforge")]
pub mod toolforge_app;
#[cfg(feature = "wikidata")]
pub mod wikidata;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(feature = "telemetry")]
pub use metrics;
#[cfg(feature = "toolforge")]
pub use toolforge;
#[cfg(feature = "database")]
pub use toolforge::pool::mysql_async;
// The direct `mediawiki` dependency has the same version requirement as
// `wikibase`'s, so this is the very crate `wikibase` is built on.
#[cfg(feature = "mediawiki")]
pub use mediawiki;
#[cfg(feature = "wikibase")]
pub use wikibase;
