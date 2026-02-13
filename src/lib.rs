//! Provides useful functionality for working with MediaWiki and WikiBase.
//! based on the `wikibase` and `mediawiki` crates.

#[macro_use]
extern crate lazy_static;

pub mod date;
pub mod disk_free;
pub mod external_id;
pub mod file_hash;
pub mod file_vec;
pub mod item_merger;
pub mod lat_lon;
pub mod merge_diff;
pub mod seppuku;
pub mod site_matrix;
pub mod sparql_results;
pub mod sparql_table;
pub mod sparql_table_vec;
pub mod sparql_value;
pub mod timestamp;
pub mod toolforge_app;
pub mod toolforge_db;
pub mod wikidata;

pub use mysql_async;
pub use toolforge;
pub use wikibase;
pub use wikibase::mediawiki;
