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
| `process-stats` | `process_stats` | | |
| `toolforge` | `toolforge_app`, re-export of `toolforge` | | `toolforge` |
| `wikibase` | re-exports of `wikibase` and `wikibase::mediawiki` | | `wikibase` |
| `sparql` | `sparql_value`, `sparql_results`, `sparql_tsv`, `sparql_endpoint`, `sparql_rewrite` | `lat-lon` | `regex`, `serde`, `serde_json`, `urlencoding` |
| `sparql-table` | `sparql_table`, `sparql_table_trait`, `sparql_table_vec` | `sparql` | `thiserror` |
| `sparql-client` | `sparql_client`, `sparql_retry` | `sparql-table` | `csv`, `reqwest`, `tempfile`, `thiserror`, `tokio` |
| `site-matrix` | `site_matrix` | `wikibase` | `serde_json`, `thiserror` |
| `wikidata` | `wikidata` | `wikibase` | `reqwest`, `thiserror` |
| `external-id` | `external_id` | `wikibase` | `chrono`, `regex`, `serde` |
| `item-merger` | `item_merger`, `merge_diff` | `external-id` | `regex`, `serde`, `serde_json` |
| `database` | `toolforge`'s `db` and `pool` modules, re-export of `mysql_async` (from `toolforge::pool`) | `toolforge` | `toolforge` (feature `wikipool`) |
| `telemetry` | `telemetry`, re-export of `metrics` | `process-stats` | `metrics`, `metrics-util`, `reqwest`, `serde`, `thiserror`, `tokio` |
| `full` | everything above | all | all |

Note that `external-id` and `wikidata` interact: enabling both additionally
exposes the Wikidata-search methods on `ExternalId`
(`search_wikidata_single_item`, `get_item_for_external_id_value`, …), which need
the `Wikidata` API client.

## Telemetry: the service dashboard

`telemetry` (feature `telemetry`) pushes a long-running service's health to the
[Buggregator service dashboard](https://magnustools.toolforge.org/buggregator/#/dashboard)
once a minute: memory against its limit, CPU, threads, open files, plus
whatever the service records through the re-exported [`metrics`](https://docs.rs/metrics)
macros. When the process ends on SIGTERM/SIGINT, a fatal panic, or a `Seppuku`
watchdog kill, a final push records why, so the dashboard can tell planned
restarts from crashes.

```rust,no_run
use wikimisc::telemetry::{Level, Telemetry};

async fn example() -> Result<(), wikimisc::telemetry::TelemetryError> {
    // Token from the config (`TelemetryConfig`) or $DASHBOARD_TOKEN.
    let telemetry = Telemetry::builder("web")
        .version(env!("CARGO_PKG_VERSION"))
        .install()?;
    wikimisc::metrics::gauge!("queue_length").set(42.0);
    telemetry.event(Level::Warn, "maxlag, backing off");
    Ok(())
}
```

- The component name (`"web"`, `"bot"`, …) separates processes of one service;
  the token decides which service a push belongs to. Register a service and get
  its token with `scripts/buggregator/dashboard/dashboard.php add <name>` in
  magnustools.
- The push URL (`TelemetryConfig::url`, `$DASHBOARD_URL`) must be https, so the
  token never travels in clear text; plain http is accepted only to localhost.
- SIGTERM/SIGINT handling is on by default: final push, then exit with 143/130.
  A service with its own graceful shutdown turns it off with
  `.handle_signals(false)` and calls `telemetry.shutdown(ExitReason::Shutdown(…))`
  when done.
- `record_http_request(status, duration)` gives web services request counts and
  latency from their middleware. `ignore_label("route")` merges series split by
  a label with many values; `install_alongside(prometheus_recorder)` keeps an
  existing `/metrics` endpoint working.
- Telemetry leaves the service: event messages and exit-reason details must not identify
  anyone (no user names or ids, page titles, query strings, IPs, raw error text). A panic is
  reported by its source location only. Keep the specifics in the local log.
- Pushing never blocks or fails the service. Unchanged counters are not resent,
  and a failed push is caught up by the next one.

## SPARQL: WDQS and QLever

`SparqlClient` (feature `sparql-client`) queries any SPARQL 1.1 endpoint — the
Wikidata Query Service (Blazegraph, and QLever as WDQS v2) in particular — and
returns a `SparqlApiResult`, a `SparqlTableVec`, raw JSON, a boolean (`ask`), a
CSV reader, or decoded TSV (`query_tsv`, for very large results). Where it is
sent, and which prefixes go with it, is a `SparqlEndpoint`:

```rust
use wikimisc::sparql_client::{SparqlClient, SparqlError};
use wikimisc::sparql_endpoint::SparqlEndpoint;

// Wikimedia's user-agent policy asks for contact information.
const USER_AGENT: &str = "MyTool/1.0 (https://example.org/mytool; mytool@example.org)";

async fn example() -> Result<(), SparqlError> {
    // Wikidata's main graph on WDQS v2. Scholarly articles are in a graph of
    // their own: `SparqlEndpoint::wikidata_scholarly()`.
    let wikidata = SparqlClient::new(SparqlEndpoint::wikidata_main(), USER_AGENT)?
        .with_retries(2);
    let table = wikidata
        .query_table("SELECT ?item ?coord WHERE { ?item wdt:P625 ?coord } LIMIT 10")
        .await?;

    // Any other endpoint, with prefixes of its own. A prefix the query
    // declares itself is not added again.
    let endpoint = SparqlEndpoint::new("https://example.org/sparql")
        .with_prefix("ex", "http://example.org/ns#");
    let found = SparqlClient::new(endpoint, USER_AGENT)?
        .ask("ASK { ?s ex:p ?o }")
        .await?;
    println!("{} rows; found: {found}", table.len());
    Ok(())
}
```

The presets are named by graph, not by backend, so a host name change at the
v2 cutover is a wikimisc update: `wikidata_main()` and `wikidata_scholarly()`
(WDQS v2), `wdqs()` and `wdqs_scholarly()` (WDQS v1, until it is shut down), and `qlever_wikidata()`
(QLever's own instance in Freiburg, not Wikimedia's).

`with_retries(n)` retries what may go away on its own — rate limiting (after
the `Retry-After` wait, unless that is over a minute), other 5xx errors, failed
connections — and never a timeout or a 4xx. `with_retry_policy` tunes the
backoff and can log each retry.

`SparqlError` tells apart what a caller may want to react to differently: a
query timeout (`Timeout`), rate limiting with the requested wait
(`RateLimited`), any other error response with the endpoint's own message
(`Endpoint` — e.g. a syntax error), a body that is not a valid result
(`InvalidResponse`), and one larger than the limit set with
`with_max_response_bytes` (`ResponseTooLarge`) — a runaway query can make an
endpoint stream hundreds of megabytes before its own timeout cuts it off. The
engines report these differently — QLever as JSON, and a timeout as HTTP 429;
WDQS v2 as JSON, and a timeout as HTTP 504; Blazegraph as a Java stack trace —
and the client reads all of them.

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
query it cannot rewrite unchanged. So far it handles named subqueries
(`rewrite_named_subqueries`, which inlines `WITH { … } AS %name` /
`INCLUDE %name` as subqueries; run it first) and `SERVICE wikibase:label`:

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

## Databases

The `database` feature enables the [`toolforge`](https://docs.rs/toolforge)
crate's `wikipool` feature: connection information (`toolforge::db`), the
extension-database routing (`toolforge::db::ExtensionDb`), and policy-compliant
pools for the Wiki Replicas, ToolsDB and other servers (`toolforge::pool`'s
`WikiPool`, `ToolsDbPool` and `CustomPool`). wikimisc re-exports both
`toolforge` and the `mysql_async` those pools are built on, so use
`wikimisc::mysql_async` rather than a direct dependency, which would have to
match its version exactly:

```rust,no_run
use wikimisc::toolforge::db::Cluster;
use wikimisc::toolforge::pool::WikiPool;

async fn commons_links() -> wikimisc::toolforge::Result<()> {
    let pool = WikiPool::new(Cluster::Web)?;
    // `links.commonswiki.web.db.svc.wikimedia.cloud` on Toolforge.
    let _conn = pool.connect_for_tables("commonswiki", &["page", "pagelinks"]).await?;
    Ok(())
}
```

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
| `database` | `toolforge::Error` (from the `toolforge` crate) |
| `telemetry` | `telemetry::TelemetryError` |

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
