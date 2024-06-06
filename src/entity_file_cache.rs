//! Cache a lot of wikibase items on disk.

use crate::file_hash::FileHash;

pub type EntityFileCache = FileHash<String, String>;
