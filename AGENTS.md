# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.


## Hard rules

- **Do not add the Claude attribution to commit messages.** No `Co-Authored-By: Claude …` line.
- **No panics in production code.** This is a library: it must not take a caller's process down. No `unwrap`, `expect`, `panic!`, `unreachable!`, slice indexing that can go out of bounds, or arithmetic that can underflow on any non-test path — return one of the per-feature error types (or `Option`) instead. `src/lib.rs` sets `#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used, clippy::panic))]` to enforce this; CI runs clippy with `-D warnings`, so a new one fails the build. If an exception is genuinely unavoidable, add an `#[allow]` with a comment explaining why there is no way to continue. Test code may panic freely.
- Always keep **code readability and long-term maintenance** in mind.
- Adhere to **SOLID and DRY principles**.
- Use **best practices** and **language standards**.
- **Keep the code simple** and elegant.
- **Write tests** where it makes sense.

## Project

A Rust library crate providing MediaWiki / Wikibase / Wikidata / Toolforge helpers, built on top of the `wikibase`, `mediawiki`, `mysql_async`, and `toolforge` crates. `src/lib.rs` re-exports `mysql_async`, `toolforge`, `wikibase`, and `wikibase::mediawiki` so downstream consumers don't have to depend on them directly — keep this surface stable when refactoring.

### Feature gating
The crate has **no default features**: every module is behind a `#[cfg(feature = …)]` gate in `src/lib.rs`, and each feature in `Cargo.toml` declares its own dependencies (see the feature table in `README.md`). Consequences when working here:

- A bare `cargo build` / `cargo test` compiles almost nothing. Always pass `--all-features` (or a specific feature) locally.
- Adding a module means adding a feature (or assigning it to an existing one), gating it in `src/lib.rs`, listing its direct dependencies in that feature, and adding it to `full` and to the CI feature matrix in `.github/workflows/ci.yml`.
- A feature lists every dependency its own modules use directly, even when a feature it enables already provides it. Don't "optimise" these away.
- Test-only dependencies (`serde_json`, `tempfile`, `tokio` macros, `wiremock`) are `[dev-dependencies]`, so they're available in `#[cfg(test)]` code regardless of features.
- Cross-feature optional API: `ExternalId`'s Wikidata-search methods live in a `#[cfg(feature = "wikidata")] impl` block in `external_id.rs`.

### Error handling
Errors use `thiserror`, with **one concrete error enum per feature** — there is no crate-wide error type and `anyhow` is not a dependency. See the table in `README.md` for the full list (`DateError`, `EntityIdError`, `HttpError`, `LatLonError`, `SparqlTableError`, `SparqlError`, `SiteMatrixError`, `WikidataError`, `TelemetryError`; `database` uses `toolforge::Error`).

- A feature's error enum must only name types from that feature's own dependencies. This is what keeps error types stable across feature selections — never add a `#[cfg(feature = …)]` variant to one.
- Signatures spell out `Result<T, SomeError>` in full. Don't introduce `pub type Result<T> = …` aliases: `date` and `lat_lon` implement `FromStr`, whose `Result<Self, Self::Err>` a one-parameter alias would silently break.
- `SparqlError` is the one error type that bridges features: `sparql-client` enables `sparql-table`, and wraps `SparqlTableError` with an explicit `#[from]`. A new bridge should do the same rather than letting the conversion happen implicitly.
- Prefer wrapping a foreign error with `#[error(transparent)] #[from]` over stringifying it; keep `#[source]` chains intact.
- Every public error enum is `#[non_exhaustive]` (since 0.2.0); a new one should be too, so adding variants stays non-breaking.

## Commands

```bash
cargo build --all-features                                # build
cargo test --all-features                                 # run all tests
cargo test --all-features <name>                          # run tests matching <name> (e.g. `cargo test item_merger`)
cargo test --all-features --test <module> -- --nocapture   # see println! output
cargo clippy --all-targets --all-features -- -D warnings  # lint
cargo fmt                                                 # format
```

Tests are colocated as `#[cfg(test)] mod tests` inside each source file. Fixtures for `item_merger` / Wikibase entity tests live in `test_data/` (`item1.json`, `item2.json`).

## Architecture

### SPARQL table: pluggable row storage behind one trait
`SparqlTable<S>` (`sparql_table.rs`) is generic over a `RowStorage` backend; `SparqlTableTrait` (`sparql_table_trait.rs`) is the object-safe interface it implements for every `S`. `RowStorage` is currently implemented only for `Vec<SparqlRow>`, exposed as the `SparqlTableVec` alias (`sparql_table_vec.rs` re-exports it for backwards compatibility).

`RowStorage::push` returns `Result` so a fallible backend (e.g. one doing I/O) can be added without changing `SparqlTable`; the `Vec` impl always returns `Ok`. When adding methods, add them to both `SparqlTableTrait` and `SparqlTable`, and extend the shared test in `sparql_table_trait.rs`.

### Wikibase item merging
`ItemMerger` (`item_merger.rs`) merges one `ItemEntity` into another and returns a `MergeDiff` (`merge_diff.rs`) shaped for the MediaWiki `wbeditentity` API action. Current scope is **additions and alterations only** — removed statements are intentionally not emitted. Date-normalization regexes (`YEAR_FIX`, `MONTH_FIX`) handle Wikidata's truncated-precision timestamps; preserve that behaviour when touching statement comparison.

The serialized payload shape is pinned by tests in `merge_diff.rs` — keep them in sync when touching `Serialize for MergeDiff`:

- Keys are `labels`, `aliases`, `descriptions`, `sitelinks`, `claims`. Empty sections are omitted entirely rather than emitted as null, so a diff never clears existing terms.
- `aliases` map each language to a **list** (`serialize_aliases`), because an entity has many aliases per language and `ItemMerger` demotes clashing labels into aliases. Labels and descriptions map to a single object per language (`serialize_labels`). Don't merge the two functions.
- `serialize_labels` also emits an `"add": ""` key for labels/descriptions. That is the alias-append idiom rather than anything documented for labels; it is long-standing behaviour, kept deliberately, and asserted by tests.
- A fresh `ItemEntity` has `sitelinks() == None`. Both `merge` and `MergeDiff::apply` create the list via `get_or_insert_with(Vec::new)` when there is something to add, and leave `None` alone when there is not — don't reintroduce an `if let Some(...) = sitelinks_mut()` guard around the write-back, which silently dropped incoming sitelinks.
- A claim merged in from another entity must not keep that entity's statement ID (`merge_diff::statement_belongs_to`): it is added as a new statement without an ID, because Wikibase rejects the whole edit for a foreign ID. IDs of the base entity are kept, so re-merging a fresh copy of the same item still updates its statements.
- New labels equal to the existing description in that language are not sent (Wikibase rejects label == description); they go through the alias path, whose filter drops them.
- A statement `ItemMerger` added earlier and then extended with more references/qualifiers comes back as "altered" but still has no ID. `MergeDiff::add_statement` / `extend` / `apply` therefore *replace* an unsaved statement that the incoming one extends (`supersedes`) instead of pushing a second copy, which Wikibase would save as a duplicate statement.
- Snak matching (`is_snak_identical`) compares the snak type too: `novalue` and `somevalue` both have no data value and must not count as equal.
- Altered statements are sent whole, with their ID, so anything lost in `wikibase`'s serialization is deleted on Wikidata. That is why `wikibase` must be at least 0.7.8 (repeated qualifiers, quantity bounds, numeric coordinate altitude); the round-trip tests in `merge_diff.rs` pin it. The floor is 0.7.9, the first version whose `Snak::new_monolingual_text` does not swap language and text.

### External identifiers
`ExternalId` (`external_id.rs`) pairs a numeric property ID with a string value (`P213:0000...`). `fix_property_value` contains per-property normalization (e.g. P213/ISNI strips spaces); when handling a new external-ID quirk, add it there rather than at call sites.

### Toolforge integration
- `ToolforgeApp::is_on_toolforge()` (`toolforge_app.rs`) detects the Toolforge environment from either the `TOOL_TOOLFORGE_API_URL` env var (the documented signal, and the only one a build-service container has) or the `/etc/wmcs-project` marker file (which also covers a bastion shell, but is really "somewhere on WMCS"). Keep both: dropping the env check breaks tools in custom-built images, dropping the file check breaks anything started outside the jobs framework.
- Database access is the `toolforge` crate's (feature `database` enables its `wikipool` feature): wikimisc has no database code of its own. `wikimisc::mysql_async` re-exports `toolforge::pool::mysql_async`, so downstream tools use the exact `mysql_async` the pools are built on — don't add a direct `mysql_async` dependency here. Missing database functionality belongs in `toolforge` (`~/rust/toolforge`), not in a wikimisc module.
- `Seppuku` (`seppuku.rs`) is an inactivity watchdog: call `arm()` on a long-running tool (from within a tokio runtime; outside one it warns on stderr and stays unarmed), `alive()` on each request; the process is killed if no `alive()` arrives within `max_seconds`. Designed for Toolforge web services that should self-terminate when idle.

### Telemetry
`telemetry.rs` (feature `telemetry`) pushes a JSON snapshot (wire format v1, defined by `DashboardPayload.php` in magnustools' `scripts/buggregator/dashboard/`) to the Buggregator dashboard. Keep the two in sync: the server enforces the component/uid patterns, 500 metrics and 50 events per push, and a 15 s minimum between regular pushes.

- `TelemetryRecorder` (`telemetry_recorder.rs`) is a `metrics` recorder over a `metrics_util` registry. Histograms are drained on each snapshot and expanded into `_count`/`_sum` counters (running totals) and `_p50`/`_p95`/`_max` gauges (this interval only). Ignored labels are stripped at registration, so their series merge.
- Counters are sent as totals since process start, and only when changed since the last *acknowledged* push; the server derives deltas. Don't switch to sending deltas: a push that fails would lose counts.
- Process readings (`process_stats.rs`, feature `process-stats`) come first in the metric list so truncation never drops them. The dashboard reads `process_resident_memory_bytes` and `process_memory_limit_bytes` by name for its OOM inference — don't rename them. The former carries non-reclaimable memory (`RssAnon` + `RssShmem`, full RSS only on kernels without the breakdown), not full RSS: services that mmap large files (rustcci) would otherwise sit near their limit permanently.
- The instance host name matters: on Kubernetes it is the pod name, which a container restarted in place keeps, and the dashboard uses that to tell crashes from redeploys.
- The panic hook sends a final push only for panics that end the process — main-thread panics outside a tokio task (the `#[tokio::main]` main future counts; a task on a `current_thread` runtime does not, as tokio catches its panic), or any panic when built with `panic = "abort"` (several tools are, e.g. petscan, listeria, wdrc) — from a separate thread with its own runtime and a fresh `reqwest::Client`, because the panicking thread may be driving the service's runtime. The dashboard answers 409 to every push after an exit push, so a panic that does not end the process must never send one.
- Pushes don't follow redirects (a redirected POST becomes a GET that looks successful); a 3xx is a failed push. The URL must be https except to loopback, checked in `build()`. `TelemetryConfig`/`TelemetryBuilder` have hand-written `Debug` that redacts the token. Non-finite gauge values and histogram observations are dropped (JSON would turn them into `null`).
- The reported version gets a short hash of the running executable appended (`/proc/self/exe`), so rebuilds show up as deploys without services bumping their crate version. Don't switch to file times: buildpack images reset them to 1980.
- Panic locations go through `anonymous_path`, which cuts absolute build paths down to the crate directory, so a build user's home directory never reaches the public dashboard.
- Events are acknowledged by sequence number, not position, because `event()` can drop the oldest queued event while a push is in flight. The final push waits at most `FINAL_PUSH_LOCK_WAIT` for `push_lock` and then sends anyway: the lock may be held by a task on the very runtime the exiting thread blocks.
- Telemetry must not carry anything that identifies a person: event messages and `ExitReason` details (shown publicly on the dashboard) get no user names/ids, titles, query strings or raw error text. The panic hook reports only the panic's source location, never its message — keep it that way.
- `Seppuku` sends a `watchdog` final push before exiting when `telemetry` is enabled and running.

### Wikidata client conventions
`Wikidata` (`wikidata.rs`) wraps construction of `mediawiki::Api` and `reqwest::Client` with the caller's user agent (a required argument of `Wikidata::new` — Wikimedia's policy asks for one per tool; don't add a default) and a 60s timeout. New HTTP calls against the Wikidata API should go through this struct so the UA and timeout are consistent; SPARQL goes through `SparqlClient` (see below).

- `mediawiki` 0.5 sends `Api::user_agent_full()` as a per-request header, which overrides the client builder's default. Every `Api` wikimisc builds (`Wikidata::api`, `SiteMatrix`) must call `set_user_agent`.
- The batched/plain calls (`get_entities`, `items_for_titles`, `search*`) use one shared `reqwest::Client` and form POSTs, not an `Api`: building an `Api` fetches siteinfo first.

### Shared tool helpers
These exist so tools stop carrying their own copies; keep them general, and keep tool policy (thresholds, caches, per-tool quirks) in the tools.

- `entity_id`: parsing has three deliberate tiers — strict `FromStr` (canonical only), `from_uri`, and `parse_lenient` (what users type). Keep them separate; don't make `FromStr` lenient. `0` is never parsed, but `new(0)` is allowed (tools use it as "none" in numeric columns). `Display` is the canonical form, which is what makes typed IDs safe to splice into SPARQL/URLs.
- `http`: `user_agent!` is a macro so `env!` expands in the *calling* crate. `HttpRetryPolicy::send` returns 2xx/3xx unread and turns 4xx/5xx into `HttpError::Status` with a body snippet.
- `title`/`commons`: MediaWiki semantics (`wfUrlencode`, `ucfirst` that keeps `ß`, md5 upload paths). Byte-exact legacy encoders (ruwex, PHP-compat, OAuth signing) stay in their tools.
- `server`: helpers take and return `Router` rather than exposing `tower-http` types, so tools on other `tower-http` versions don't clash. `serve` drains on SIGINT/SIGTERM and then calls `telemetry::shutdown`, so telemetry must be built with `handle_signals(false)` there. Never rename metrics or add a route label to `http_requests_total`.
- `statements`: typed builders; `coordinate_snak` always sets a precision (`wbeditentity` rejects a coordinate without one).
- `date::Date` built by `new`/`from_wikibase` uses canonical padding (`-00` for unknown parts); `FromStr` keeps its historical behaviour (rejects BCE and future years).

### SPARQL value parsing
`SparqlValue` (`sparql_value.rs`) is the typed enum SPARQL bindings deserialize into. Detection relies on regexes (`RE_ENTITY`, `RE_FILE`, `RE_POINT`, `RE_DATE`) against URI/literal values; extending value types means extending both the enum and the parser regexes.

- It must parse what both WDQS (Blazegraph) and QLever emit: `Point(…)` vs `POINT(…)`, and QLever's missing `Z` on years beyond four digits. The tests in `sparql_results.rs` deserialize a verbatim response from each engine; keep them passing.
- A `None` from `new_from_json` fails deserialization of the **whole** `SparqlApiResult`, so a value that is recognised but has no typed representation (a WKT polygon, a coordinate on another globe) falls back to `Literal` with its raw text rather than `None`.
- Adding a variant to `SparqlValue` breaks downstream exhaustive `match`es; don't do it casually.

### SPARQL endpoints and client
`SparqlEndpoint` (`sparql_endpoint.rs`, feature `sparql`, no I/O) is a URL plus the prefixes to prepend; `prepare_query` skips prefixes the query declares itself, because Blazegraph rejects a duplicate declaration. `SparqlClient` (`sparql_client.rs`, feature `sparql-client`) POSTs prepared queries form-encoded.

- There is deliberately no "engine" setting. Engine differences are handled by looking at the response, so the client also works with endpoints that are neither engine.
- Error classification in `SparqlError::from_error_response` goes by body *and* status: QLever reports a query timeout as HTTP 429, which WDQS uses for rate limiting. Messages are extracted from QLever's JSON `exception`, WDQS v2's JSON `message` (QLever behind Wikimedia's front end: `{"errorType", "message", "details"}`), Blazegraph's Java exception line, or the first `<p>` of a Wikimedia HTML error page.
- Don't send QLever's `timeout` parameter by default: the public instance answers HTTP 403 for anything above its 30 s cap unless an access token is given.
- `sparql_rewrite` turns Blazegraph-only features into SPARQL 1.1 for QLever / WDQS v2, following Wikimedia's rewriter (gitlab.wikimedia.org/repos/wikidata-platform/wikidata-query-rewriter) where it can. It works on tokens from the crate-private `sparql_lexer` and **splices the original text** instead of parsing and reserialising, so untouched parts (comments, formatting) survive — that matters for rewriting queries stored in wikitext. Anything it does not support returns the query unchanged with a reason. Tests check every rewritten query parses with `spargebra` (a dev-dependency).
  - Label service: Wikimedia's guarded `OPTIONAL` chain + `BIND`, with `mul` fallback for labels. Deliberate differences: `[AUTO_LANGUAGE]` is dropped (it matches nothing outside the WDQS UI), outputs the caller does not need are not computed, and aliases are unsupported.
  - Named subqueries (`sparql_rewrite/named_subqueries.rs`): each `INCLUDE %name` becomes `{ body }` (recursively), the `WITH { … } AS %name` blocks are removed. The label service rewrite refuses named subqueries, so callers run this one first. Queries are untrusted input: the output size is computed before anything is built and capped (`MAX_GROWTH` × input, at least `MIN_LENGTH_LIMIT`), nesting is capped at `MAX_DEPTH`, and nested `WITH` or `JOIN ON` hints are refused — keep the rewriters linear and non-recursive in input size.
- Endpoint presets are named by graph, not backend: `wikidata_main()` / `wikidata_scholarly()` point at WDQS v2 (`query-next` / `query-scholarly-next`), `wdqs()` / `wdqs_scholarly()` at v1 until it is shut down (v1 has the same main/scholarly split), `qlever_wikidata()` at Freiburg's qlever.dev. When Wikimedia moves v2 to other host names, change the URL constants only, so that every tool follows with `cargo update`.
- `SparqlClient::new` takes the user agent as a required argument: Wikimedia asks for contact information, and WDQS v2 admits only known user agents for now. Don't add a default.
- `query_csv` and `query_tsv` reject a 2xx response whose content type is not CSV-/TSV-like (a proxy's HTML page), via the crate-private `content_type` checks, which take a `&str` so they couple to no HTTP crate.
- Classification goes by status as well as body: WDQS v2 reports a timeout as HTTP 504 with `errorType` `gateway_timeout` *or* `backend_timeout`, and the latter's message does not say "timed out". Any 504 is a `Timeout`.
- Retries (`sparql_retry::RetryPolicy`, off unless `with_retries`/`with_retry_policy` is used) are decided by `SparqlError::is_retryable`: rate limiting, 5xx other than a timeout, and `Http` errors other than the client's own timeout. Never retry a `Timeout` or a 4xx. A `Retry-After` longer than the maximum wait returns the error instead of retrying early. The retry machinery is the generic `retry::RetryPolicy<E>` (`sparql_retry::RetryPolicy` is an alias); retries are not logged by wikimisc — callers log them through the `on_retry` hook. (`log` is a dependency only of `telemetry` and `server`, for their startup/shutdown messages.)
- There is no SPARQL method on `Wikidata`; all SPARQL goes through `SparqlClient`.
- `SparqlClient` never trusts a declared Content-Length for allocation (at most 1 MiB is reserved up front), and reads at most 64 KiB of an error body.
- Response parsing is CPU-heavy and must not run inline in the async fn: `query`/`query_json`/`query_table`/`query_tsv` hand the parse (and `from_api_result`) to the crate-private `blocking::run`, which uses `spawn_blocking` when a tokio runtime is present and falls back to inline outside one. `http::RetryPolicy::fetch_json` and `Wikidata::api_post` (`wbgetentities` answers are several MB) do the same. A blocking task that does not finish becomes `SparqlError::ParseTask` / a `Decode` error, never a panic.
