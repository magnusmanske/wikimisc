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
| `sparql` | `sparql_value`, `sparql_results`, `sparql_endpoint`, `sparql_rewrite` | `lat-lon` | `regex`, `serde`, `serde_json`, `urlencoding` |
| `sparql-table` | `sparql_table`, `sparql_table_trait`, `sparql_table_vec` | `sparql` | `thiserror` |
| `sparql-client` | `sparql_client` | `sparql-table` | `csv`, `reqwest`, `tempfile`, `thiserror` |
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

## SPARQL: WDQS and QLever

`SparqlClient` (feature `sparql-client`) queries any SPARQL 1.1 endpoint — the
Wikidata Query Service (Blazegraph) and QLever in particular — and returns a
`SparqlApiResult`, a `SparqlTableVec`, raw JSON, or a CSV reader. Where it is
sent, and which prefixes go with it, is a `SparqlEndpoint`:

```rust
use wikimisc::sparql_client::{SparqlClient, SparqlError};
use wikimisc::sparql_endpoint::SparqlEndpoint;

async fn example() -> Result<(), SparqlError> {
    // QLever declares no prefixes, so this preset prepends the ones WDQS
    // declares implicitly; queries written for WDQS run unchanged.
    let qlever = SparqlClient::new(SparqlEndpoint::qlever_wikidata())?;
    let table = qlever
        .query_table("SELECT ?item ?coord WHERE { ?item wdt:P625 ?coord } LIMIT 10")
        .await?;

    // Any other endpoint, with prefixes of its own. A prefix the query
    // declares itself is not added again.
    let endpoint = SparqlEndpoint::new("https://example.org/sparql")
        .with_prefix("ex", "http://example.org/ns#");
    let result = SparqlClient::new(endpoint)?.query("ASK { ?s ex:p ?o }").await?;
    assert!(result.boolean().is_some());
    Ok(())
}
```

For WDQS, pass a client carrying your tool's own user agent, as Wikimedia's
policy asks: `SparqlClient::with_client(SparqlEndpoint::wdqs(), wikidata.reqwest_client()?)`.

`SparqlError` tells apart what a caller may want to react to differently: a
query timeout (`Timeout`), rate limiting with the requested wait
(`RateLimited`), any other error response with the endpoint's own message
(`Endpoint` — e.g. a syntax error), and a body that is not a valid result
(`InvalidResponse`). The two engines report these differently — QLever as JSON,
and a timeout as HTTP 429; Blazegraph as a Java stack trace — and the client
reads both.

Values parse the same from either engine (`Point(…)` and `POINT(…)`, a missing
`Z` on very large years, and so on). Literals are otherwise passed through as
the engine wrote them, so a decimal can read `3700577` from WDQS and
`3700577.0` from QLever.

### Rewriting Blazegraph queries

Queries written for WDQS v1 may use Blazegraph-only features that QLever (and
so WDQS v2) rejects. `sparql_rewrite` (feature `sparql`) rewrites them into
standard SPARQL 1.1, following Wikimedia's
[migration guide](https://www.wikidata.org/wiki/Wikidata:SPARQL_query_service/WDQS_backend_update/Migrate).
It splices the query text, so formatting and comments survive, and leaves a
query it cannot rewrite unchanged. So far it handles `SERVICE wikibase:label`:

```rust
use wikimisc::sparql_rewrite::{LabelServiceRewrite, rewrite_label_service};

let query = "SELECT ?item ?itemLabel WHERE {
  ?item wdt:P31 wd:Q146 .
  SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" . }
}";
// Say which outputs you read from the results; others are not computed.
match rewrite_label_service(query, |variable| variable == "itemLabel") {
    LabelServiceRewrite::Rewritten(rewritten) => assert!(!rewritten.contains("SERVICE")),
    LabelServiceRewrite::Unchanged | LabelServiceRewrite::Unsupported(_) => unreachable!(),
}
```

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
| `sparql-client` | `sparql_client::SparqlError` |
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
