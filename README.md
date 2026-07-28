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
| `date` | `date`, `timestamp` | | `anyhow`, `chrono`, `regex` |
| `file-storage` | `file_hash`, `file_vec` | | `anyhow`, `serde`, `serde_json`, `tempfile` |
| `lat-lon` | `lat_lon` | | `anyhow`, `serde` |
| `seppuku` | `seppuku` | | `tokio` |
| `toolforge` | `toolforge_app`, re-export of `toolforge` | | `toolforge` |
| `wikibase` | re-exports of `wikibase` and `wikibase::mediawiki` | | `wikibase` |
| `sparql` | `sparql_value`, `sparql_results` | `lat-lon` | `regex`, `serde`, `serde_json`, `urlencoding` |
| `sparql-table` | `sparql_table`, `sparql_table_trait`, `sparql_table_vec` | `sparql`, `file-storage` | `anyhow` |
| `site-matrix` | `site_matrix` | `wikibase` | `anyhow`, `serde_json` |
| `wikidata` | `wikidata` | `wikibase` | `anyhow`, `csv`, `reqwest`, `tempfile` |
| `external-id` | `external_id` | `wikibase` | `chrono`, `regex`, `serde` |
| `item-merger` | `item_merger`, `merge_diff` | `external-id` | `regex`, `serde`, `serde_json` |
| `database` | `toolforge_db`, re-export of `mysql_async` | `toolforge` | `anyhow`, `mysql_async`, `serde_json` |
| `full` | everything above | all | all |

Note that `external-id` and `wikidata` interact: enabling both additionally
exposes the Wikidata-search methods on `ExternalId`
(`search_wikidata_single_item`, `get_item_for_external_id_value`, …), which need
the `Wikidata` API client.

## Development

```bash
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

CI also builds and tests each feature in isolation, so a module that only
compiles when a neighbouring feature happens to be enabled will fail the build.
