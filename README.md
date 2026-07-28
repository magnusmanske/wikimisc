# wikimisc
This is a Rust crate with MediaWiki-related functionality I find useful.
Primarily developing this for myself to use in my tools, but PRs welcome!

## Features

There are **no default features**. Every module sits behind a feature flag, so
you pull in only the code — and only the dependencies — you actually use:

```toml
[dependencies]
wikimisc = { version = "0.1", default-features = false, features = ["item-merger", "wikidata"] }
```

| Feature | Modules | Enables | Extra dependencies |
| --- | --- | --- | --- |
| `date` | `date`, `timestamp` | | `chrono`, `regex`, `thiserror` |
| `file-storage` | `file_hash`, `file_vec`, `file_error` | | `serde`, `serde_json`, `tempfile`, `thiserror` |
| `lat-lon` | `lat_lon` | | `serde`, `thiserror` |
| `seppuku` | `seppuku` | | `tokio` |
| `toolforge` | `toolforge_app`, re-export of `toolforge` | | `toolforge` |
| `wikibase` | re-exports of `wikibase` and `wikibase::mediawiki` | | `wikibase` |
| `sparql` | `sparql_value`, `sparql_results` | `lat-lon` | `regex`, `serde`, `serde_json`, `urlencoding` |
| `sparql-table` | `sparql_table`, `sparql_table_trait`, `sparql_table_vec` | `sparql`, `file-storage` | `thiserror` |
| `site-matrix` | `site_matrix` | `wikibase` | `serde_json`, `thiserror` |
| `wikidata` | `wikidata` | `wikibase` | `csv`, `reqwest`, `tempfile`, `thiserror` |
| `external-id` | `external_id` | `wikibase` | `chrono`, `regex`, `serde` |
| `item-merger` | `item_merger`, `merge_diff` | `external-id` | `regex`, `serde`, `serde_json` |
| `database` | `toolforge_db`, re-export of `mysql_async` | `toolforge` | `mysql_async`, `serde_json`, `thiserror` |
| `full` | everything above | all | all |

Note that `external-id` and `wikidata` interact: enabling both additionally
exposes the Wikidata-search methods on `ExternalId`
(`search_wikidata_single_item`, `get_item_for_external_id_value`, …), which need
the `Wikidata` API client.

## Errors

Fallible APIs return concrete `thiserror` enums, **one per feature**, rather than
a single crate-wide error type. A feature's error enum only ever names types from
that feature's own dependencies, so enabling or disabling a feature never changes
the shape of another feature's error type:

| Feature | Error type |
| --- | --- |
| `date` | `date::DateError` |
| `lat-lon` | `lat_lon::LatLonError` |
| `file-storage` | `file_error::FileError` (shared by `FileHash` and `FileVec`) |
| `sparql-table` | `sparql_table::SparqlTableError` |
| `site-matrix` | `site_matrix::SiteMatrixError` |
| `wikidata` | `wikidata::WikidataError` |
| `database` | `toolforge_db::DatabaseError` |

There is one bridge between them: `SparqlTableError::Storage` wraps a `FileError`,
because the disk-backed `SparqlTable` pushes rows through `FileVec`.

All of these implement `std::error::Error`, so a downstream crate that uses
several features can fold them into its own error type with
`#[from]`/`#[source]`, or erase them behind `Box<dyn Error>` or `anyhow`.

Nothing in this crate panics on a non-test code path: no `unwrap`, `expect`,
`panic!`, out-of-bounds indexing, or arithmetic that can underflow. Fallible
work returns the error types above, and operations with a sensible empty result
return `Option`. This is enforced by
`#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used, clippy::panic))]`
plus `-D warnings` in CI.

## Development

```bash
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

CI also builds and tests each feature in isolation, so a module that only
compiles when a neighbouring feature happens to be enabled will fail the build.
