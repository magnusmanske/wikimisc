//! Provides useful functionality for working with MediaWiki and WikiBase.
//! based on the `wikibase` and `mediawiki` crates.

#[macro_use]
extern crate lazy_static;

pub mod date;
pub mod entity_file_cache;
pub mod external_id;
pub mod file_hash;
pub mod item_merger;
pub mod lat_lon;
pub mod merge_diff;
pub mod site_matrix;
pub mod sparql_results;
pub mod sparql_value;
pub mod timestamp;
pub mod toolforge_app;
pub mod toolforge_db;
pub mod wikidata;

pub use mysql_async;
pub use toolforge;
pub use wikibase;
pub use wikibase::mediawiki;
