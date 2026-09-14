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
| `lat-lon` | `lat_lon` | | `serde`, `thiserror` |
| `seppuku` | `seppuku` | | `tokio` |
| `toolforge` | `toolforge_app`, re-export of `toolforge` | | `toolforge` |
| `wikibase` | re-exports of `wikibase` and `wikibase::mediawiki` | | `wikibase` |
| `sparql` | `sparql_value`, `sparql_results` | `lat-lon` | `regex`, `serde`, `serde_json`, `urlencoding` |
| `sparql-table` | `sparql_table`, `sparql_table_trait`, `sparql_table_vec` | `sparql` | `thiserror` |
| `site-matrix` | `site_matrix` | `wikibase` | `serde_json`, `thiserror` |
| `wikidata` | `wikidata` | `wikibase` | `csv`, `reqwest`, `tempfile`, `thiserror` |
| `external-id` | `external_id` | `wikibase` | `chrono`, `regex`, `serde` |
| `item-merger` | `item_merger`, `merge_diff` | `external-id` | `regex`, `serde`, `serde_json` |
| `database` | `toolforge_db`, `toolforge_credentials`, re-export of `mysql_async` | `toolforge` | `mysql_async`, `serde_json`, `thiserror` |
| `full` | everything above | all | all |

Note that `external-id` and `wikidata` interact: enabling both additionally
exposes the Wikidata-search methods on `ExternalId`
(`search_wikidata_single_item`, `get_item_for_external_id_value`, …), which need
the `Wikidata` API client.

## Wiki Replica clusters

A handful of wikis have tables split off into an *extension database* on its own
replica cluster, reached under a prefixed host name — Commons' links tables
(`links.commonswiki…`, since September 2026) and Wikidata's `wbt_*` term store
(`termstore.wikidatawiki…`, since 2025). `toolforge_db::DbCluster` knows which
tables sit where, so instead of hard-coding host names you can ask for the host
that can serve a given query:

```rust
use wikimisc::toolforge_db::{DatabaseError, DbCluster, ToolforgeDB};

fn commons_links(db: &ToolforgeDB) -> Result<(), DatabaseError> {
    // `links.commonswiki.web.db.svc.wikimedia.cloud` on Toolforge.
    let links = db.db_host_and_schema_for_wiki_tables("commonswiki", &["page", "pagelinks"])?;

    // The same call picks the core cluster for any wiki without the split.
    let core = db.db_host_and_schema_for_wiki_tables("enwiki", &["page", "pagelinks"])?;

    // Tables on opposite sides of a split cannot be joined in SQL, so this is
    // an error rather than a query that silently returns nothing.
    assert!(db
        .db_host_and_schema_for_wiki_tables("commonswiki", &["actor", "pagelinks"])
        .is_err());

    // Or name the cluster directly, e.g. to pick a pre-registered pool.
    assert_eq!(DbCluster::for_tables("commonswiki", &["pagelinks"])?, DbCluster::Links);
    Ok(())
}
```

`db_host_and_schema_for_wiki` is unchanged and still returns the core cluster,
and `DbServerGroup` selects the `web` (default) or `analytics` service name.

A tool that would otherwise open a connection per wiki can connect per
*section* instead — the docs ask that nothing else do so, because which section
holds a wiki changes. Get the mapping from `meta_p` (`SELECT dbname, slice FROM
meta_p.wiki`) and parse it with `DbSection::from_meta_slice("s3.labsdb")`.

## Connecting

`add_mysql_pool` takes a JSON config with an explicit `url`. The alternative is
to let the crate find the tool's own credentials, from the `TOOL_REPLICA_USER` /
`TOOL_REPLICA_PASSWORD` environment variables (the only source a tool built into
a custom container image has) or from `replica.my.cnf` in `$TOOL_DATA_DIR` or
`$HOME`:

```rust
use std::time::Duration;
use wikimisc::toolforge_db::{DatabaseError, DbSection, PoolSettings, ToolforgeDB};

fn connect() -> Result<ToolforgeDB, DatabaseError> {
    let mut db = ToolforgeDB::default().with_pool_settings(
        // Issues `SET max_statement_time` on every connection in the pool.
        PoolSettings::default().with_max_statement_time(Some(Duration::from_secs(300))),
    );

    // Picks the cluster, the host name and the credentials.
    db.add_replica_pool("commons", "commonswiki", &["page", "pagelinks"])?;

    // One connection for a whole section, for a crosswiki tool. Queries must
    // name the schema: `SELECT page_title FROM enwiki_p.page`.
    if let Some(section) = DbSection::from_meta_slice("s1.labsdb") {
        db.add_section_pool("s1", section)?;
    }

    // The tool's own database, on ToolsDB. The read-only variant is
    // `add_tool_db_pool_readonly`, which long queries should prefer.
    db.add_tool_db_pool("own", "s51234__mytool_p")?;
    Ok(db)
}
```

Mind the Toolforge [connection handling
policy](https://wikitech.wikimedia.org/wiki/Help:Wiki_Replicas#Connection_handling_policy):
pools that hold connections open while unused are not permitted against the Wiki
Replicas or ToolsDB, and idle connections get killed. `PoolSettings::default()`
stays within it — `min_connections: 0`, `max_connections: 10` (the per-tool
limit), `keep_sec: 1` — and is also what `add_mysql_pool` falls back to for
missing keys. `keep_sec: 0` would be *worse* rather than stricter: `mysql_async`
then bounds the pool by `min_connections`, closing every connection as it is
returned, so nothing is reused and every query pays for a new connection and
handshake. One second keeps connections reusable while still letting the pool
reap what has gone idle.

## Errors

Fallible APIs return concrete `thiserror` enums, **one per feature**, rather than
a single crate-wide error type. A feature's error enum only ever names types from
that feature's own dependencies, so enabling or disabling a feature never changes
the shape of another feature's error type:

| Feature | Error type |
| --- | --- |
| `date` | `date::DateError` |
| `lat-lon` | `lat_lon::LatLonError` |
| `sparql-table` | `sparql_table::SparqlTableError` |
| `site-matrix` | `site_matrix::SiteMatrixError` |
| `wikidata` | `wikidata::WikidataError` |
| `database` | `toolforge_db::DatabaseError` |

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
