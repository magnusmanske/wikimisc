# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - Unreleased

### Added
- `entity-id` feature: typed entity IDs (`entity_id::{ItemId, PropertyId,
  LexemeId, MediaInfoId, EntitySchemaId, FormId, SenseId, EntityId,
  EntityKind}`) with strict (`FromStr`), lenient (`parse_lenient`) and URI
  (`from_uri`) parsing, string serde (and `serde_numeric` for numbers), and
  numeric `mysql_async` binding with `database`.
- `retry` feature: `RetryPolicy<E>` for any error type (`Retryable`, `run`,
  `run_if`, jitter, clamped `Retry-After`).
- `http` feature: `user_agent!()`, `client_builder`, `client`, `retry_after`
  (seconds or HTTP date), `HttpError`, `HttpRetryPolicy::send`,
  `read_body_capped`, `snippet`.
- `title` feature: `title` (DB-key/display forms, `normalize`, `ucfirst`,
  `strip_namespace`, `wiki_urlencode`, `page_url`, `index_url`, `permalink`,
  `title_from_url`, percent coding) and `commons` (FilePath, Redirect, upload
  and thumbnail URLs, file names from URLs).
- `escape` feature: `escape::{html, html_text, xml, unescape_html}`.
- `logging` feature: `logging::Logging` / `logging::init()` (`tracing`
  subscriber plus `log` bridge; stderr by default or `.stdout()`; colour only
  on a terminal or as set with `.ansi()`; an explicit `.filter()` beats the
  environment variable, `.default_filter()` is the fallback).
- `shutdown` feature: `shutdown::{shutdown_signal, ShutdownSignal}` without a
  web framework; `jsonp` feature: `jsonp::{is_valid_callback, wrap,
  CallbackRules, CONTENT_TYPE}`. `server` enables both and re-exports them.
- `server` feature: `server::{shutdown_signal, listen_addr, serve,
  serve_with_connect_info}`, router layers (`standard_layers`, `catch_panic`,
  `compression`, `trace`, `timeout`, `cors_any`, `cors_wikimedia`),
  `is_wikimedia_origin` (`cors_wikimedia` also admits the `Api-User-Agent`
  header gadgets send), `server::health`, `server::jsonp` (adds the axum
  `Jsonp<T>` response); with `telemetry`, `request_metrics`.
- `labels` feature: label/description lookup with language fallback,
  `base_language`, `RankFilter`, best-ranked statements, for raw entity JSON
  (needs only `serde_json`) and, with `wikibase`, for `wikibase` entities.
- `statements` feature: typed snak builders (times with calendar models,
  coordinates with precision, …) and the usual references (`stated_in_reference`,
  `retrieved_today`, `reference_url`, `imported_from_revision`).
- `mediawiki` feature: the `mediawiki` re-export without `wikibase`.
- `sparql_util`: `values_clause`, `entity_term`, `string_literal`,
  `label_pattern`, `not_deprecated`; `SparqlValue::entity_id`.
- `Date`: `new`, `new_in`, `from_wikibase`, `from_qs`, `today`, parts,
  `Calendar`, `with_calendar`, `with_precision`, `describe`.
- `TimeStamp::{naive, ago, days_ago, start_of_today, rfc3339}`.
- `LatLon::{try_new, from_wkt, to_wkt, distance_km, distance_m}`,
  `lat_lon::{BoundingBox, parse_distance_m}`; `FromStr` accepts `lat/lon` and `@…`.
- `Wikidata::{get_entities, items_for_titles, search, search_items,
  search_single_item, set_concurrency, user_agent}` and `wikidata::CirrusQuery`.
- `SiteMatrix::{get_api_url_for_wiki, get_api_for_wiki_with_builder,
  cached_api_for_wiki, languages, from_value}`; `SiteMatrixLanguage`.
- `ExternalId::{is_case_insensitive, property_id, search_query}`.
- `Seppuku::watch` with `Verdict` (probe-based watchdog);
  `process_stats::MemoryStats`; `TelemetryBuilder::{component, install_or_log,
  install_alongside_or_log}`.

### Changed
- **Breaking:** `Wikidata::new` takes the user agent, and `Wikidata` no longer
  implements `Default`. The old default (`wikimisc-wikidata/0.1.0`) did not
  follow Wikimedia's user-agent policy and shared one rate-limit budget across
  every tool.
- `Wikidata::api()` and the `Api`s `SiteMatrix` builds now actually send the
  configured user agent (the `mediawiki` crate overrides the client's header
  per request unless `Api::set_user_agent` is called).
- **Breaking:** `sparql_retry::RetryPolicy` and `sparql_retry::Retry` are now
  aliases of `retry::RetryPolicy<SparqlError>` / `retry::Retry<'_, SparqlError>`;
  existing calls compile unchanged.
- `SparqlClient` honours a `Retry-After` given as an HTTP date, and its default
  client adds a 10 s connect timeout and gzip.
- `ExternalId` normalises more values: P244 (`+`, `%20`), P1207 (`+`), P1368
  (`LNC10-` prefix), P8034 (`_` → `/`). `ExternalId::as_reference` is built by
  `statements::stated_in_reference`.
- `SparqlApiResult` accepts a response without `head`.
- **Breaking:** the `sparql` feature enables `entity-id`; `external-id`
  enables `entity-id` and `statements`; the `wikibase` re-exports now come with
  the `mediawiki` feature (same crate as before).
- Requires `wikibase` 0.7.9, whose `Snak::new_monolingual_text` no longer
  swaps language and text.

### Deprecated
- The `ExternalId` search methods without a `Wikidata` argument; use the
  `_with` variants so requests carry your user agent.

## [0.2.0] - 2026-10-19

### Removed
- **Breaking:** the `toolforge_db` and `toolforge_credentials` modules
  (`ToolforgeDB`, `DbCluster`, `DbServerGroup`, `DbSection`, `HostSchema`,
  `PoolSettings`, `DatabaseError`, `Credentials`, `DbService`). Database
  access now comes from the [`toolforge`](https://crates.io/crates/toolforge)
  crate (6.1), re-exported as `wikimisc::toolforge`: `WikiPool`,
  `ToolsDbPool` and `CustomPool` in `toolforge::pool`, extension-database
  routing in `toolforge::db::ExtensionDb`, and errors as `toolforge::Error`.
  Mapping: `DbServerGroup` → `toolforge::db::Cluster`, `DbCluster` →
  `Option<toolforge::db::ExtensionDb>`, `ToolforgeDB::add_mysql_pool` →
  `CustomPool::builder_from_url`, `ToolforgeDB::fix_wiki_db_name` →
  `toolforge::db::normalize_dbname`.

### Changed
- **Breaking:** the `database` feature now enables `toolforge`'s `wikipool`
  feature, and `wikimisc::mysql_async` re-exports `toolforge::pool::mysql_async`,
  which is `mysql_async` **0.37** (was 0.36), built without TLS and without the
  `derive` macros.
- **Breaking:** the error enums `DateError`, `LatLonError`,
  `SparqlTableError`, `SiteMatrixError`, `WikidataError` and `TelemetryError`
  are `#[non_exhaustive]`, like `SparqlError` already was, so matches on them
  need a `_` arm; future variants won't be breaking changes.
- **Breaking:** `RetryPolicy::on_retry` accepts any
  `Fn(&Retry) + Send + Sync + 'static`, including closures that capture state;
  plain functions work as before. `RetryPolicy` is no longer `Copy`.
- `Wikidata::item2qs` skips values QuickStatements would reject or misread
  (units other than Wikidata items, asymmetric quantity bounds, calendars
  other than Gregorian/Julian, non-zero timezone/before/after, non-Earth or
  altitude-carrying coordinates, text containing tabs, line breaks, `||` or
  `/*`) instead of emitting them. A skipped main value drops the statement, a
  skipped qualifier or reference snak only that snak.
- `Date::from_str` rejects days that don't exist in their month (e.g.
  `1999-02-29`, `1999-04-31`) and month `00` at month precision (`1999-00`).
- `TimeStamp::str2naive` / `str2utc` accept only 14-digit timestamps and
  reject a seconds value of 60.
- `MergeDiff::apply` adds an altered statement whose ID belongs to the item
  but which the item doesn't have yet, as Wikibase does.
- Panic locations sent to the telemetry dashboard no longer contain absolute
  build paths: they start at the crate's directory (e.g.
  `serde_json-1.0.154/src/value/index.rs`).

### Security
- `rewrite_named_subqueries` no longer builds unbounded output or overflows
  the stack on crafted queries: a query whose inlined form would be more than
  16 times its length (and over 1 MiB), or that nests named subqueries more
  than 64 deep, is left unchanged with the new reason
  `UnsupportedNamedSubquery::TooLarge`. A 1.3 KB query used to expand to
  117 MB, and deep `INCLUDE` chains aborted the process.
- `rewrite_label_service` runs in linear time for queries with very many
  `SELECT` keywords or many label services on the same entity, which used to
  take minutes.

### Fixed
- `Wikidata::item2qs` put item, property and lexeme values in quotes, which
  QuickStatements reads as strings; IDs are now bare. It also dropped quantity
  units and tolerances and the Julian calendar; it now emits `U<n>`,
  `~<tolerance>` and `/J`.
- `ItemMerger` kept another entity's statement IDs on merged-in claims, so
  Wikibase rejected the whole `wbeditentity` edit and `MergeDiff::apply`
  dropped the statement; such claims are now added as new statements without
  an ID.
- `ItemMerger` could add a label equal to the existing description in the
  same language, which Wikibase rejects.
- `MergeDiff` dropped sitelink badges from the payload.
- `rewrite_named_subqueries` reported invalid SPARQL as rewritten: nested
  `WITH … AS %name` definitions and `INCLUDE %name JOIN ON (…)` hints are left
  unchanged with the new reasons `UnsupportedNamedSubquery::Nested` and
  `UnsupportedNamedSubquery::JoinHint`.
- `SparqlEndpoint::prepare_query` mistook a local name ending in `-prefix` /
  `.prefix` (e.g. `vcard:honorific-prefix`), or `PREFIX` inside a comment,
  string or IRI, for a prefix declaration, and left out a prefix the query
  needed.
- A Commons file IRI whose percent-encoding isn't valid UTF-8 is kept as
  `SparqlValue::Uri` instead of failing deserialization of the whole result.
- `SparqlTable::from_api_result` accepts results with zero variables (e.g. a
  matching `SELECT * { wd:Q42 wdt:P31 wd:Q5 }`) instead of failing with
  `HeaderNotSet`.
- Telemetry: an event queued while a push was in flight, with the queue
  full, could make the acknowledgement drop an event that was never sent.
- Telemetry: the final push was lost when a regular push was still in flight
  on the runtime the exiting thread drives (e.g. a panic in the main future of
  a `current_thread` runtime). It now waits at most a second for the regular
  push, then sends anyway; a regular push that gets its turn after shutdown
  began no longer sends.

## [0.1.4] - 2026-10-18

### Added
- `sparql_retry`: `RetryPolicy` with exponential backoff, honouring
  `Retry-After`, enabled with `SparqlClient::with_retries` /
  `with_retry_policy`; `SparqlError::is_retryable` decides what is retried.
- `SparqlClient::ask` and `SparqlClient::query_tsv`, with `sparql_tsv`
  (`SparqlTsv`, `decode_cell`).
- Endpoint presets `SparqlEndpoint::wikidata_main()` /
  `wikidata_scholarly()` (WDQS v2) and `wdqs_scholarly()` (v1).
- `sparql_rewrite::rewrite_named_subqueries`: inlines Blazegraph's named
  subqueries (`WITH { … } AS %name` / `INCLUDE %name`) for other engines.

### Changed
- **Breaking:** `SparqlClient::new` takes the user agent as a required
  argument.

### Removed
- **Breaking:** SPARQL methods on `Wikidata` (`load_sparql_csv`,
  `sparql_url`, `set_sparql_url`); use `SparqlClient`.

## [0.1.3] - 2026-10-17

### Changed
- Telemetry reports non-reclaimable resident memory (`RssAnon` + `RssShmem`)
  as `process_resident_memory_bytes`, so pages of mmap'd files don't look like
  memory pressure.

## [0.1.2] - 2026-10-17

### Added
- Feature `sparql-client`: `SparqlClient`, an HTTP client for WDQS
  (Blazegraph), QLever and any other SPARQL 1.1 endpoint, returning a
  `SparqlApiResult`, a `SparqlTableVec`, raw JSON or a CSV reader.
  `SparqlError` tells apart timeouts, rate limiting (with the requested wait),
  endpoint errors with the endpoint's own message, invalid responses, and
  responses over the limit set with `with_max_response_bytes`.
- `sparql_endpoint` (feature `sparql`): `SparqlEndpoint`, a URL plus the
  prefixes to prepend, with `wdqs()` and `qlever_wikidata()` presets.
- `sparql_rewrite` (feature `sparql`): rewrites Blazegraph's
  `SERVICE wikibase:label` into standard SPARQL 1.1 for QLever / WDQS v2,
  preserving the query's formatting and comments.
- `SparqlApiResult::boolean()` for ASK results.
- Feature `telemetry` (plus a `metrics` re-export): pushes process readings,
  metrics, events and exit reasons to the Buggregator service dashboard.
- Feature `process-stats`: memory, CPU, thread and file-descriptor readings of
  the current process and its cgroup. Memory comes as full RSS and as
  non-reclaimable resident memory (`RssAnon` + `RssShmem`); telemetry reports
  the latter, so pages of mmap'd files don't look like memory pressure.
- `Seppuku` sends a final `watchdog` push before exiting when `telemetry` is
  running.

### Changed
- Minimum supported Rust version is now 1.89 (required by `uuid`, a
  dependency of `mysql_async`).
- Requires `wikibase` 0.7.8, which fixes the serialization of repeated
  qualifiers and quantity bounds (see `ItemMerger` below) and parsing of
  coordinates with a numeric altitude, and `tokio` 1.41.
- `SparqlValue` parses QLever's output as well as WDQS': `POINT(…)` as well as
  `Point(…)`, and dates beyond four-digit years without a trailing `Z`. A
  recognised value without a typed representation (a WKT polygon, a coordinate
  on another globe) now becomes a `Literal` instead of failing the whole
  result.
- `Date` accepts Wikidata's canonical form with a leading `+`
  (`+1776-07-04T00:00:00Z/11`).
- `add_mysql_pool` falls back to the `ToolforgeDB`'s pool settings (as set
  with `set_pool_settings`) for keys missing from the JSON, as documented, and
  keeps pool parameters given in the connection URL (`pool_min`, `pool_max`,
  `inactive_connection_ttl`, `reset_connection`, …) unless the JSON sets them.
- A `max_statement_time` of zero means no limit (`None`); limits under a
  microsecond are rounded up instead of becoming unlimited.
- `Wikidata::load_sparql_csv` rejects a successful response that is not CSV
  (e.g. a proxy's HTML page) with `WikidataError::Io` (`InvalidData`) instead
  of parsing it into rows.

### Fixed
- **Security:** `Debug` for `ToolforgeDB` printed database passwords (via
  `mysql_async::Pool`); it now lists only pool names and settings.
- **Security:** wiki names containing anything but `[a-z0-9_]` are rejected
  instead of being put into a replica host name, so a user-supplied wiki
  cannot send credentials to another host.
- `max_statement_time` applied only to a pooled connection's first use: the
  pool's connection reset cleared it. It is now a `setup` statement, re-run
  after every reset.
- Wiki names are lowercased and stripped of `_p` before aliasing
  (`be-taraskwiki_p` → `be_x_oldwiki`, `CommonsWiki` → `commonswiki`).
- Empty `TOOL_*_USER`/`TOOL_*_PASSWORD`, `TOOL_DATA_DIR` and `HOME` values are
  ignored; an empty directory no longer means the current one. The
  `replica.my.cnf` parser accepts a UTF-8 BOM and a comment after a section
  header.
- `ItemMerger` / `MergeDiff` could emit one new statement twice when a later
  claim, in the same or a later merge, added references or qualifiers to it;
  Wikidata would then create two statements.
- `ItemMerger` treated `novalue` and `somevalue` snaks as identical, silently
  dropping one of them (main snaks, qualifiers and references).
- `ItemMerger` did not record labels demoted to aliases on the merged item, so
  merging the same source again emitted them again.
- `Seppuku::arm()` outside a tokio runtime warns and stays unarmed instead of
  panicking.

Fixes to code added in this release, listed for anyone who tracked `main`:
- `SparqlClient` reserved memory for a response's declared Content-Length; a
  huge value could panic or abort the process. At most 1 MiB is reserved up
  front, and at most 64 KiB of an error response is read.
- `SparqlClient::query_csv` rejects a successful response that is not CSV
  with the new `SparqlError::UnexpectedContentType`.
- `sparql_rewrite`: removing a label service could glue the triples on either
  side together into invalid SPARQL; labels that decide the result rows
  (`GROUP BY ?xLabel` or `SELECT DISTINCT ?xLabel` without `?x`) were dropped;
  unbalanced brackets give `Unsupported(Syntax)`; and the rewrite now runs in
  roughly linear time instead of cubic.
- `telemetry`: `Debug` for `TelemetryConfig` and `TelemetryBuilder` showed the
  token; a panic caught inside a tokio task (e.g. on a `current_thread`
  runtime) was reported as the process exiting and stopped all further pushes;
  `install()` outside a runtime left the global recorder installed, so a retry
  failed; redirects were followed (a redirected push looked successful); the
  dashboard URL must now be https, or http to loopback (new errors
  `InvalidUrl`, `InsecureUrl`); NaN and infinite values are left out instead
  of being sent as `null`.

## [0.1.1] - 2026-09-14

### Added
- `toolforge_credentials`: replica and ToolsDB credentials from the
  environment or `replica.my.cnf`.
- `DbCluster` and `db_host_and_schema_for_wiki_tables`: host names for
  extension databases split off a wiki's core cluster (Commons' `links`,
  Wikidata's `termstore`).

### Changed
- Wiki Replica and ToolsDB host names follow the current Toolforge
  documentation; pools default to the Toolforge connection handling policy.

## [0.1.0] - 2026-07-29

Initial release.

[0.2.0]: https://crates.io/crates/wikimisc/0.2.0
[0.1.4]: https://crates.io/crates/wikimisc/0.1.4
[0.1.3]: https://crates.io/crates/wikimisc/0.1.3
[0.1.2]: https://crates.io/crates/wikimisc/0.1.2
[0.1.1]: https://crates.io/crates/wikimisc/0.1.1
[0.1.0]: https://crates.io/crates/wikimisc/0.1.0
