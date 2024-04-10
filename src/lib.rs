//! Provides useful functionality for working with MediaWiki and WikiBase.
//! based on the `wikibase` and `mediawiki` crates.

#[macro_use]
extern crate lazy_static;

pub mod date;
pub mod external_id;
pub mod item_merger;
pub mod merge_diff;

pub use wikibase;
pub use wikibase::mediawiki;
