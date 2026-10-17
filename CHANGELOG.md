# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2] - unreleased

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

[0.1.2]: https://github.com/magnusmanske/wikimisc/compare/7bc0382...HEAD
[0.1.1]: https://crates.io/crates/wikimisc/0.1.1
[0.1.0]: https://crates.io/crates/wikimisc/0.1.0
