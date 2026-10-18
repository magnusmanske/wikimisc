//! HTTP client for SPARQL endpoints: WDQS (Blazegraph), QLever, or any other
//! that speaks the SPARQL 1.1 protocol.
//!
//! The engines agree on the protocol and on the JSON and CSV result formats.
//! Where they differ, the client adapts to what it is talking to rather than
//! being told which engine that is, so an endpoint not named here works too:
//!
//! - **Prefixes.** QLever predeclares none; [`SparqlEndpoint`] adds them.
//! - **Values.** Coordinates, dates and so on have different lexical forms;
//!   [`crate::sparql_value::SparqlValue`] parses both.
//! - **Errors.** QLever answers with a JSON object carrying an `exception`,
//!   WDQS v2 with one carrying `errorType` and `message`, Blazegraph with a
//!   Java stack trace, and Wikimedia's front end with an HTML page.
//!   [`SparqlError`] reads all of them, and tells a query timeout (which
//!   QLever reports as HTTP 429, WDQS v2 as 504) from rate limiting (which
//!   WDQS reports as HTTP 429).
//!
//! Failures that may go away on their own can be retried; see
//! [`SparqlClient::with_retries`].
//!
//! ```rust,no_run
//! use wikimisc::sparql_client::{SparqlClient, SparqlError};
//! use wikimisc::sparql_endpoint::SparqlEndpoint;
//!
//! async fn humans() -> Result<usize, SparqlError> {
//!     let client = SparqlClient::new(
//!         SparqlEndpoint::wikidata_main(),
//!         "MyTool/1.0 (https://example.org/mytool; mytool@example.org)",
//!     )?
//!     .with_retries(2);
//!     let table = client
//!         .query_table("SELECT ?item WHERE { ?item wdt:P31 wd:Q5 } LIMIT 10")
//!         .await?;
//!     Ok(table.len())
//! }
//! ```

use crate::content_type::{may_be_csv, may_be_tsv};
use crate::sparql_endpoint::SparqlEndpoint;
use crate::sparql_results::SparqlApiResult;
use crate::sparql_retry::{Retry, RetryPolicy};
use crate::sparql_table::{SparqlTableError, SparqlTableVec};
use crate::sparql_tsv::SparqlTsv;
use regex::Regex;
use reqwest::header::{HeaderMap, ACCEPT, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;
use std::fs::File;
use std::future::Future;
use std::io::{Seek, SeekFrom, Write};
use std::sync::LazyLock;
use std::time::Duration;
use tempfile::tempfile;
use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const ACCEPT_JSON: &str = "application/sparql-results+json";
const ACCEPT_CSV: &str = "text/csv";
const ACCEPT_TSV: &str = "text/tab-separated-values";
/// How much of an unparseable response body an error quotes.
const SNIPPET_CHARS: usize = 200;
/// How much of an endpoint's error message an error keeps.
const MESSAGE_CHARS: usize = 500;
/// How much of an error response is read. The message is near the start, and
/// the status already says the query failed, so the rest is not worth waiting
/// for.
const ERROR_BODY_BYTES: usize = 64 * 1024;
/// The most memory reserved up front for a response, whatever size it
/// declares: the declaration is the server's claim, not data received.
const MAX_PREALLOCATION: usize = 1024 * 1024;

// As in `sparql_value`, the patterns are literals stored as `Option<Regex>` so
// that one which somehow failed to compile degrades to "never matches" instead
// of panicking; `test_all_static_regexes_compile` catches a broken literal.
static RE_HTML_P: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?is)<p\b[^>]*>(.*?)</p>").ok());
static RE_HTML_TITLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?is)<title\b[^>]*>(.*?)</title>").ok());
static RE_HTML_TAG: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"<[^>]*>").ok());

/// Failure modes of [`SparqlClient`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SparqlError {
    /// The request could not be sent or the response not received, including
    /// the client-side timeout expiring.
    #[error(transparent)]
    Http(#[from] reqwest::Error),

    /// Buffering a CSV response to a temporary file failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The endpoint, or the gateway in front of it (HTTP 504), gave up on the
    /// query because it ran too long. Retrying the same query is unlikely to
    /// help.
    #[error("SPARQL query timed out (HTTP {status}): {message}")]
    Timeout { status: u16, message: String },

    /// The endpoint is rate-limiting this client. `retry_after` is the wait it
    /// asked for, if it named one.
    #[error("SPARQL endpoint is rate-limiting requests: {message}")]
    RateLimited {
        retry_after: Option<Duration>,
        message: String,
    },

    /// Any other error response — most often a malformed query (HTTP 400),
    /// or the endpoint being unavailable (HTTP 5xx). `message` is the
    /// endpoint's own explanation, extracted from whatever format it used.
    #[error("SPARQL endpoint returned HTTP {status}: {message}")]
    Endpoint { status: u16, message: String },

    /// The endpoint reported success but the body is not a SPARQL JSON
    /// result — typically an error page from a proxy, or a result truncated
    /// by a timeout part-way through streaming it.
    #[error("SPARQL endpoint did not return a valid JSON result; response began: {snippet:?}")]
    InvalidResponse {
        #[source]
        source: serde_json::Error,
        snippet: String,
    },

    /// The endpoint reported success but sent `content_type` where
    /// `expected` (CSV or TSV) was asked for — typically an HTML error page
    /// from a proxy. `message` is that page's explanation, as for
    /// [`Self::Endpoint`].
    #[error("SPARQL endpoint sent {content_type:?} where {expected} was expected: {message}")]
    UnexpectedContentType {
        content_type: String,
        expected: &'static str,
        message: String,
    },

    /// [`SparqlClient::ask`] got a result without a `boolean` — the query
    /// was not an ASK query, or the endpoint answered something else.
    #[error("SPARQL result has no boolean; was the query an ASK query?")]
    NotBoolean,

    /// The response is larger than the limit set with
    /// [`SparqlClient::with_max_response_bytes`]; reading it was abandoned.
    /// Retrying the same query is pointless.
    #[error("SPARQL result is larger than the {limit}-byte limit")]
    ResponseTooLarge { limit: usize },

    /// The result could not be turned into a table.
    #[error(transparent)]
    Table(#[from] SparqlTableError),
}

impl SparqlError {
    /// Classifies an error response. This goes by the body as much as the
    /// status, because the status alone is ambiguous: QLever reports a
    /// timeout as 429, which WDQS uses for rate limiting.
    fn from_error_response(status: u16, retry_after: Option<Duration>, body: &str) -> Self {
        let message = error_message(body);
        // QLever says "Operation timed out."; Blazegraph throws a
        // `java.util.concurrent.TimeoutException` or a `QueryTimeoutException`.
        // WDQS v2 answers 504 with `errorType` "gateway_timeout" or
        // "backend_timeout", whose message need not say "timed out".
        let timed_out = status == StatusCode::GATEWAY_TIMEOUT.as_u16()
            || message.to_lowercase().contains("timed out")
            || body.contains("TimeoutException");
        if timed_out {
            Self::Timeout { status, message }
        } else if status == StatusCode::TOO_MANY_REQUESTS.as_u16() {
            Self::RateLimited {
                retry_after,
                message,
            }
        } else {
            Self::Endpoint { status, message }
        }
    }

    /// Whether the same query may succeed if sent again: rate limiting, an
    /// error status of 500 or above other than a timeout, or a request that
    /// failed on the way (but not by running into the client's own timeout,
    /// which a slow query would hit again). [`RetryPolicy`] retries these.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::Endpoint { status, .. } => *status >= 500,
            Self::Http(error) => !error.is_timeout() && !error.is_builder(),
            _ => false,
        }
    }
}

/// Sends SPARQL queries to one [`SparqlEndpoint`].
///
/// Queries are POSTed form-encoded, so their length is not limited by the
/// maximum URL length, and the configured prefixes are added first.
#[derive(Debug, Clone)]
pub struct SparqlClient {
    endpoint: SparqlEndpoint,
    client: Client,
    max_response_bytes: Option<usize>,
    retry_policy: RetryPolicy,
}

impl SparqlClient {
    /// A client sending `user_agent`, with a 60-second timeout.
    ///
    /// Wikimedia's [user-agent policy](https://foundation.wikimedia.org/wiki/Policy:Wikimedia_Foundation_User-Agent_Policy)
    /// asks every tool to identify itself *with contact information*, in the
    /// form `ToolName/1.0 (https://tool.example; tool@example.org)`, and WDQS
    /// may refuse requests without it. Admission to WDQS v2 may additionally
    /// depend on the user agent being known to Wikimedia.
    pub fn new(endpoint: SparqlEndpoint, user_agent: &str) -> Result<Self, SparqlError> {
        let client = Client::builder()
            .user_agent(user_agent)
            .timeout(DEFAULT_TIMEOUT)
            .build()?;
        Ok(Self::with_client(endpoint, client))
    }

    /// A client sending its requests through `client`, whose user agent,
    /// timeout and other settings are used as they are. The user agent
    /// should carry contact information; see [`Self::new`].
    pub fn with_client(endpoint: SparqlEndpoint, client: Client) -> Self {
        Self {
            endpoint,
            client,
            max_response_bytes: None,
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Retries a failed query up to `retries` times, if the failure may go
    /// away on its own; see [`RetryPolicy`]. Without this, nothing is retried.
    #[must_use]
    pub fn with_retries(self, retries: u32) -> Self {
        self.with_retry_policy(RetryPolicy::new(retries))
    }

    /// Retries failed queries as `policy` says.
    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Refuses responses larger than `limit` bytes with
    /// [`SparqlError::ResponseTooLarge`], instead of reading them whole.
    ///
    /// A query with a runaway join can make an endpoint stream hundreds of
    /// megabytes before its own timeout cuts the result off — WDQS then
    /// sends a truncated, invalid result with status 200 — and parsing that
    /// takes several times its size in memory.
    #[must_use]
    pub fn with_max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = Some(limit);
        self
    }

    pub fn endpoint(&self) -> &SparqlEndpoint {
        &self.endpoint
    }

    /// Runs a SELECT or ASK query and parses the result.
    pub async fn query(&self, sparql: &str) -> Result<SparqlApiResult, SparqlError> {
        parse_json(&self.fetch_json(sparql).await?)
    }

    /// Runs an ASK query. A result without a boolean is an error
    /// ([`SparqlError::NotBoolean`]), never `false`.
    pub async fn ask(&self, sparql: &str) -> Result<bool, SparqlError> {
        self.query(sparql)
            .await?
            .boolean()
            .ok_or(SparqlError::NotBoolean)
    }

    /// Runs a query and returns the result as untyped JSON, for callers that
    /// process the bindings themselves.
    pub async fn query_json(&self, sparql: &str) -> Result<serde_json::Value, SparqlError> {
        parse_json(&self.fetch_json(sparql).await?)
    }

    /// Runs a SELECT query and returns the result decoded from TSV — a
    /// third the size of JSON, for very large results. Values are plain
    /// strings, as described in [`crate::sparql_tsv`].
    pub async fn query_tsv(&self, sparql: &str) -> Result<SparqlTsv, SparqlError> {
        let body = self
            .retrying(|| async {
                let response = self
                    .send_text(sparql, ACCEPT_TSV, "TSV", may_be_tsv)
                    .await?;
                self.read_all(response).await
            })
            .await?;
        Ok(SparqlTsv::parse(&String::from_utf8_lossy(&body)))
    }

    /// Runs a SELECT query and returns the result as a table.
    pub async fn query_table(&self, sparql: &str) -> Result<SparqlTableVec, SparqlError> {
        Ok(SparqlTableVec::from_api_result(self.query(sparql).await?)?)
    }

    /// Runs a query and returns a CSV reader over the result, which is
    /// buffered to a temporary file rather than held in memory — the better
    /// choice for a large result. Values are the engine's raw lexical forms
    /// (`Point(…)` from WDQS, `POINT(…)` from QLever) with IRIs unabbreviated.
    pub async fn query_csv(&self, sparql: &str) -> Result<csv::Reader<File>, SparqlError> {
        let mut file = self
            .retrying(|| async {
                let response = self
                    .send_text(sparql, ACCEPT_CSV, "CSV", may_be_csv)
                    .await?;
                let mut file = tempfile()?;
                self.read_body(response, |chunk| file.write_all(chunk))
                    .await?;
                Ok(file)
            })
            .await?;
        file.seek(SeekFrom::Start(0))?;
        Ok(csv::ReaderBuilder::new()
            .flexible(true)
            .has_headers(true)
            .from_reader(file))
    }

    /// The raw response body. Kept as bytes and parsed from there: decoding
    /// it into a `String` first would hold a large result in memory twice.
    async fn fetch_json(&self, sparql: &str) -> Result<Vec<u8>, SparqlError> {
        self.retrying(|| async { self.read_all(self.send(sparql, ACCEPT_JSON).await?).await })
            .await
    }

    /// The whole response body, within the configured maximum size.
    async fn read_all(&self, response: Response) -> Result<Vec<u8>, SparqlError> {
        let declared = self.declared_length(&response)?;
        let mut body = Vec::with_capacity(declared.min(MAX_PREALLOCATION));
        self.read_body(response, |chunk| {
            body.extend_from_slice(chunk);
            Ok(())
        })
        .await?;
        Ok(body)
    }

    /// Runs `attempt`, and again as long as the retry policy says so.
    async fn retrying<T, F, Fut>(&self, mut attempt: F) -> Result<T, SparqlError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, SparqlError>>,
    {
        let mut retries_done = 0;
        loop {
            let error = match attempt().await {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            let Some(wait) = self.retry_policy.delay(&error, retries_done) else {
                return Err(error);
            };
            retries_done += 1;
            self.retry_policy.notify(&Retry {
                endpoint: self.endpoint.url(),
                error: &error,
                retry: retries_done,
                wait,
            });
            tokio::time::sleep(wait).await;
        }
    }

    /// Sends the query asking for a text format (`expected`, e.g. "CSV"),
    /// and refuses a successful response whose content type `accepts` does
    /// not allow.
    async fn send_text(
        &self,
        sparql: &str,
        accept: &str,
        expected: &'static str,
        accepts: fn(&str) -> bool,
    ) -> Result<Response, SparqlError> {
        let response = self.send(sparql, accept).await?;
        if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
            let content_type = String::from_utf8_lossy(content_type.as_bytes()).into_owned();
            if !accepts(&content_type) {
                let message = error_message(&read_error_body(response).await);
                return Err(SparqlError::UnexpectedContentType {
                    content_type,
                    expected,
                    message,
                });
            }
        }
        Ok(response)
    }

    /// Feeds the response body to `sink` chunk by chunk, giving up as soon
    /// as it exceeds the configured maximum size.
    async fn read_body(
        &self,
        response: Response,
        sink: impl FnMut(&[u8]) -> std::io::Result<()>,
    ) -> Result<(), SparqlError> {
        self.declared_length(&response)?;
        let limit = self.response_limit();
        if read_capped(response, limit, sink).await? {
            Ok(())
        } else {
            Err(SparqlError::ResponseTooLarge { limit })
        }
    }

    /// The body size the response declares (0 if it declares none), refused
    /// if it is over the configured maximum before anything is read.
    fn declared_length(&self, response: &Response) -> Result<usize, SparqlError> {
        let limit = self.response_limit();
        let declared = response
            .content_length()
            .map_or(0, |len| usize::try_from(len).unwrap_or(usize::MAX));
        if declared > limit {
            return Err(SparqlError::ResponseTooLarge { limit });
        }
        Ok(declared)
    }

    fn response_limit(&self) -> usize {
        self.max_response_bytes.unwrap_or(usize::MAX)
    }

    /// Sends the prepared query, turning an error status into a [`SparqlError`].
    async fn send(&self, sparql: &str, accept: &str) -> Result<Response, SparqlError> {
        let query = self.endpoint.prepare_query(sparql);
        let response = self
            .client
            .post(self.endpoint.url())
            .header(ACCEPT, accept)
            .form(&[("query", query.as_str())])
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let retry_after = retry_after(response.headers());
        let body = read_error_body(response).await;
        Err(SparqlError::from_error_response(
            status.as_u16(),
            retry_after,
            &body,
        ))
    }
}

/// Feeds `sink` at most `limit` bytes of the response body. Returns whether
/// that was the whole body; if not, the rest is left unread.
async fn read_capped(
    mut response: Response,
    limit: usize,
    mut sink: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> Result<bool, SparqlError> {
    let mut remaining = limit;
    while let Some(mut chunk) = response.chunk().await? {
        let complete = chunk.len() <= remaining;
        chunk.truncate(remaining);
        sink(&chunk)?;
        if !complete {
            return Ok(false);
        }
        remaining -= chunk.len();
    }
    Ok(true)
}

/// The start of an error response's body, for its explanation. The status
/// already says the request failed; an unreadable body only costs the
/// explanation, so it is not an error of its own.
async fn read_error_body(response: Response) -> String {
    let mut body = Vec::new();
    let _ = read_capped(response, ERROR_BODY_BYTES, |chunk| {
        body.extend_from_slice(chunk);
        Ok(())
    })
    .await;
    String::from_utf8_lossy(&body).into_owned()
}

/// Parses a JSON response body.
fn parse_json<T: DeserializeOwned>(body: &[u8]) -> Result<T, SparqlError> {
    serde_json::from_slice(body).or_else(|source| {
        // Endpoints occasionally emit raw control characters inside string
        // literals, which JSON forbids. In valid JSON they can only be
        // whitespace between tokens, so replacing every one with a space
        // repairs the strings without changing anything else.
        let repaired: String = String::from_utf8_lossy(body)
            .chars()
            .map(|c| if c < ' ' { ' ' } else { c })
            .collect();
        serde_json::from_str(&repaired).map_err(|_| SparqlError::InvalidResponse {
            source,
            snippet: repaired.trim().chars().take(SNIPPET_CHARS).collect(),
        })
    })
}

/// The `Retry-After` header, if it gives a number of seconds. (The HTTP-date
/// form is not used by either engine.)
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

/// The human-readable part of an error response body, whichever engine (or
/// proxy in front of it) produced it.
fn error_message(body: &str) -> String {
    let message = json_error(body)
        .or_else(|| java_exception(body))
        .or_else(|| html_text(body))
        .unwrap_or_else(|| collapse_whitespace(body));
    if message.is_empty() {
        "(no details)".to_string()
    } else {
        message.chars().take(MESSAGE_CHARS).collect()
    }
}

/// A JSON error object, as returned by
/// - QLever: `{"exception": "Invalid SPARQL query: …", "status": "ERROR", …}`;
/// - WDQS v2, QLever behind Wikimedia's front end:
///   `{"errorType": "parse_error", "message": "Encountered …", "details": {…}}`.
fn json_error(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let message = json.get("exception").or_else(|| json.get("message"))?;
    Some(message.as_str()?.trim().to_string())
}

/// Blazegraph: the query echoed back, then a Java exception chain on one line,
/// then the stack trace:
///
/// ```text
/// SPARQL-QUERY: queryStr=…
/// java.util.concurrent.ExecutionException: org.openrdf.query.MalformedQueryException: Encountered " "limit" "LIMIT "" at line 1, column 37.
///     at java.util.concurrent.FutureTask.report(FutureTask.java:122)
/// ```
///
/// The message is whatever the innermost exception says, or the exception's
/// name if it says nothing.
fn java_exception(body: &str) -> Option<String> {
    let line = body.lines().map(str::trim).find(|line| {
        line.contains("Exception") && !line.starts_with("at ") && !line.starts_with("SPARQL-QUERY:")
    })?;
    let message = line
        .rsplit_once("Exception: ")
        .map_or(line, |(_, message)| message);
    Some(message.trim().to_string())
}

/// An HTML error page, as served by Wikimedia's front end (rate limiting,
/// user-agent policy, upstream down): its first paragraph, else its title.
fn html_text(body: &str) -> Option<String> {
    if !body.trim_start().starts_with('<') {
        return None;
    }
    [&RE_HTML_P, &RE_HTML_TITLE]
        .into_iter()
        .filter_map(|re| re.as_ref()?.captures(body)?.get(1))
        .map(|inner| {
            let text = match RE_HTML_TAG.as_ref() {
                Some(tag) => tag.replace_all(inner.as_str(), " "),
                None => inner.as_str().into(),
            };
            collapse_whitespace(&text)
        })
        .find(|text| !text.is_empty())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql_value::SparqlValue;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const SPARQL_PATH: &str = "/sparql";
    const TEST_USER_AGENT: &str = "wikimisc-test/1.0 (https://github.com/magnusmanske/wikimisc)";

    // ── fixtures (captured from the live endpoints) ─────────────────────────

    const QLEVER_RESULT: &str = r#"{"head":{"vars":["item","coord"]},"results":{"bindings":[{"item":{"type":"uri","value":"http://www.wikidata.org/entity/Q64"},"coord":{"datatype":"http://www.opengis.net/ont/geosparql#wktLiteral","type":"literal","value":"POINT(13.383333 52.516667)"}}]},"meta":{"query-time-ms":9,"result-size-total":1}}"#;

    const QLEVER_SYNTAX_ERROR: &str = r#"{
        "exception": "Invalid SPARQL query: Prefix wdt was not registered using a PREFIX declaration",
        "metadata": {"line": 1, "positionInLine": 21, "startIndex": 21, "stopIndex": 27},
        "query": "SELECT ?x WHERE { ?x wdt:P31 wd:Q5 } LIMIT 1",
        "resultsize": 0,
        "status": "ERROR",
        "time": {"computeResult": 0, "total": 0}
    }"#;

    const QLEVER_TIMEOUT: &str = r#"{
        "exception": "Operation timed out.",
        "query": "SELECT (COUNT(*) AS ?n) WHERE { ?x wdt:P31 ?y }",
        "resultsize": 0,
        "status": "ERROR",
        "time": {"computeResult": 10, "total": 10}
    }"#;

    /// WDQS v2 timeouts, both HTTP 504: when QLever gives up (reconstructed
    /// from its documented shape), and when the backend does not answer
    /// within 60 s (captured 2026-10-17).
    const WDQS_V2_GATEWAY_TIMEOUT: &str =
        r#"{"errorType":"gateway_timeout","message":"Operation timed out.","details":{}}"#;
    const WDQS_V2_BACKEND_TIMEOUT: &str = r#"{"errorType":"backend_timeout","message":"The timeout period of 60000ms has been exceeded while executing POST /sparql/sparql?timeout=60000ms for server wdqs-main-external-qlever-service:7001"}"#;

    const WDQS_V2_FEDERATION_ERROR: &str = r#"{"errorType":"federation_error","message":"Endpoint http://wikiba.se/ontology#label is not allowed for query federation.","details":{"rejectedEndpoint":"http://wikiba.se/ontology#label"}}"#;

    const BLAZEGRAPH_SYNTAX_ERROR: &str = "SPARQL-QUERY: queryStr=SELECT ?x WHERE { ?x wdt:P31 wd:Q5  LIMIT 1\n\
java.util.concurrent.ExecutionException: org.openrdf.query.MalformedQueryException: Encountered \" \"limit\" \"LIMIT \"\" at line 1, column 37.\n\
Was expecting one of:\n    \"(\" ...\n\
\tat java.util.concurrent.FutureTask.report(FutureTask.java:122)\n\
\tat java.util.concurrent.FutureTask.get(FutureTask.java:206)\n";

    const BLAZEGRAPH_TIMEOUT: &str = "SPARQL-QUERY: queryStr=SELECT * { ?a ?b ?c }\n\
java.util.concurrent.TimeoutException\n\
\tat java.util.concurrent.FutureTask.get(FutureTask.java:205)\n";

    const WIKIMEDIA_RATE_LIMIT: &str = r#"<!DOCTYPE html>
<html lang="en">
<meta charset="utf-8">
<title>Wikimedia Error</title>
<style>
p { margin: 0.7em 0 1em 0; }
</style>
<div class="content" role="main">
<div class="content-text">
<h1>Error</h1>

<p>Aggressively rate-limiting to 1 req / min - this rule was created during active wdqs outage (d7f65f1)</p>
</div>
</div>"#;

    // ── helpers ─────────────────────────────────────────────────────────────

    fn endpoint_for(server: &MockServer) -> SparqlEndpoint {
        SparqlEndpoint::new(format!("{}{}", server.uri(), SPARQL_PATH))
    }

    fn client_for(server: &MockServer) -> SparqlClient {
        SparqlClient::new(endpoint_for(server), TEST_USER_AGENT).unwrap()
    }

    async fn mount(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path(SPARQL_PATH))
            .respond_with(response)
            .mount(server)
            .await;
    }

    fn json_response(status: u16, body: &str) -> ResponseTemplate {
        ResponseTemplate::new(status)
            .set_body_string(body.to_string())
            .insert_header("content-type", "application/json")
    }

    /// The `query` form field of a request, decoded.
    fn sent_query(request: &Request) -> String {
        let body = String::from_utf8_lossy(&request.body);
        body.split('&')
            .find_map(|pair| pair.strip_prefix("query="))
            .map(|q| {
                urlencoding::decode(&q.replace('+', " "))
                    .unwrap()
                    .into_owned()
            })
            .unwrap_or_default()
    }

    async fn error_for(response: ResponseTemplate) -> SparqlError {
        let server = MockServer::start().await;
        mount(&server, response).await;
        let client = client_for(&server);
        client.query("ASK {}").await.unwrap_err()
    }

    // ── requests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_query_posts_prepared_query_and_asks_for_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(SPARQL_PATH))
            .and(header("accept", ACCEPT_JSON))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(|request: &Request| {
                sent_query(request)
                    == "PREFIX wd: <http://www.wikidata.org/entity/>\nSELECT ?item { VALUES ?item { wd:Q64 } }"
            })
            .respond_with(json_response(200, QLEVER_RESULT))
            .expect(1)
            .mount(&server)
            .await;

        let endpoint = endpoint_for(&server).with_prefix("wd", "http://www.wikidata.org/entity/");
        let client = SparqlClient::new(endpoint, TEST_USER_AGENT).unwrap();
        client
            .query("SELECT ?item { VALUES ?item { wd:Q64 } }")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_query_sends_a_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("user-agent", TEST_USER_AGENT))
            .respond_with(json_response(200, QLEVER_RESULT))
            .expect(1)
            .mount(&server)
            .await;
        let client = client_for(&server);
        client.query("SELECT * {}").await.unwrap();
    }

    #[tokio::test]
    async fn test_with_client_uses_the_given_client() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("user-agent", "my-tool/1.0"))
            .respond_with(json_response(200, QLEVER_RESULT))
            .expect(1)
            .mount(&server)
            .await;
        let client = SparqlClient::with_client(
            endpoint_for(&server),
            Client::builder().user_agent("my-tool/1.0").build().unwrap(),
        );
        client.query("SELECT * {}").await.unwrap();
    }

    // ── result formats ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_query_parses_qlever_result() {
        let server = MockServer::start().await;
        mount(&server, json_response(200, QLEVER_RESULT)).await;
        let client = client_for(&server);

        let result = client.query("SELECT * {}").await.unwrap();
        assert_eq!(result.head()["vars"], vec!["item", "coord"]);
        assert!(matches!(
            result.bindings()[0]["coord"],
            SparqlValue::Location(_)
        ));
    }

    #[tokio::test]
    async fn test_query_ask() {
        let server = MockServer::start().await;
        mount(&server, json_response(200, r#"{"boolean":true,"head":{}}"#)).await;
        let client = client_for(&server);
        assert_eq!(client.query("ASK {}").await.unwrap().boolean(), Some(true));
    }

    #[tokio::test]
    async fn test_ask() {
        let server = MockServer::start().await;
        mount(
            &server,
            json_response(200, r#"{"boolean":false,"head":{}}"#),
        )
        .await;
        assert!(!client_for(&server).ask("ASK {}").await.unwrap());
    }

    #[tokio::test]
    async fn test_ask_without_boolean_is_an_error() {
        let server = MockServer::start().await;
        mount(&server, json_response(200, QLEVER_RESULT)).await;
        assert!(matches!(
            client_for(&server).ask("SELECT * {}").await,
            Err(SparqlError::NotBoolean)
        ));
    }

    #[tokio::test]
    async fn test_query_tsv() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(SPARQL_PATH))
            .and(header("accept", ACCEPT_TSV))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "?item\t?label\n<http://www.wikidata.org/entity/Q64>\t\"Berlin\"@en\n",
                "text/tab-separated-values",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let tsv = client_for(&server).query_tsv("SELECT * {}").await.unwrap();
        assert_eq!(tsv.vars(), ["item", "label"]);
        assert_eq!(
            tsv.rows(),
            [["http://www.wikidata.org/entity/Q64", "Berlin"]]
        );
    }

    #[tokio::test]
    async fn test_query_tsv_rejects_html_served_as_success() {
        let server = MockServer::start().await;
        mount(
            &server,
            ResponseTemplate::new(200).set_body_raw(WIKIMEDIA_RATE_LIMIT, "text/html"),
        )
        .await;
        assert!(matches!(
            client_for(&server).query_tsv("SELECT * {}").await,
            Err(SparqlError::UnexpectedContentType {
                expected: "TSV",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn test_query_table() {
        let server = MockServer::start().await;
        mount(&server, json_response(200, QLEVER_RESULT)).await;
        let client = client_for(&server);

        let table = client.query_table("SELECT * {}").await.unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(table.get_var_index("coord"), Some(1));
        assert_eq!(
            table.get_row_col(0, 0),
            Some(SparqlValue::Entity("Q64".to_string()))
        );
    }

    #[tokio::test]
    async fn test_query_json() {
        let server = MockServer::start().await;
        mount(&server, json_response(200, QLEVER_RESULT)).await;
        let client = client_for(&server);

        let json = client.query_json("SELECT * {}").await.unwrap();
        assert_eq!(json["head"]["vars"][0], "item");
        assert_eq!(json["meta"]["result-size-total"], 1);
    }

    #[tokio::test]
    async fn test_query_repairs_raw_control_characters() {
        let body = "{\"head\":{\"vars\":[\"x\"]},\"results\":{\"bindings\":[{\"x\":{\"type\":\"literal\",\"value\":\"a\u{1}b\"}}]}}";
        let server = MockServer::start().await;
        mount(&server, json_response(200, body)).await;
        let client = client_for(&server);

        let result = client.query("SELECT * {}").await.unwrap();
        assert_eq!(
            result.bindings()[0]["x"],
            SparqlValue::Literal("a b".to_string())
        );
    }

    #[tokio::test]
    async fn test_query_csv() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(SPARQL_PATH))
            .and(header("accept", ACCEPT_CSV))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(
                        "x,c\r\nhttp://www.wikidata.org/entity/Q64,POINT(13.38 52.51)\r\n",
                    )
                    .insert_header("content-type", "text/csv"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = client_for(&server);

        let mut reader = client.query_csv("SELECT * {}").await.unwrap();
        let rows: Vec<_> = reader.records().map(Result::unwrap).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(&rows[0][1], "POINT(13.38 52.51)");
    }

    #[tokio::test]
    async fn test_response_within_limit_is_read() {
        let server = MockServer::start().await;
        mount(&server, json_response(200, QLEVER_RESULT)).await;
        let client = client_for(&server).with_max_response_bytes(QLEVER_RESULT.len());
        assert_eq!(
            client.query("SELECT * {}").await.unwrap().bindings().len(),
            1
        );
    }

    #[tokio::test]
    async fn test_response_over_declared_limit_is_refused() {
        let server = MockServer::start().await;
        mount(&server, json_response(200, QLEVER_RESULT)).await;
        let client = client_for(&server).with_max_response_bytes(QLEVER_RESULT.len() - 1);
        assert!(matches!(
            client.query("SELECT * {}").await,
            Err(SparqlError::ResponseTooLarge { .. })
        ));
        assert!(matches!(
            client.query_csv("SELECT * {}").await,
            Err(SparqlError::ResponseTooLarge { .. })
        ));
    }

    /// Serves one request with `response`, written to the socket verbatim,
    /// so it can say things wiremock would correct — a Content-Length the
    /// body does not match, say. Unless `close` is set the connection then
    /// stays open, as from a server still sending.
    async fn raw_server(response: String, close: bool) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            // The request is small: wait until its form body has arrived.
            while !String::from_utf8_lossy(&request).contains("query=") {
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..n]);
            }
            let _ = socket.write_all(response.as_bytes()).await;
            if !close {
                std::future::pending::<()>().await;
            }
        });
        format!("http://{address}{SPARQL_PATH}")
    }

    /// Serves one request with `body` in chunked encoding, without a
    /// Content-Length — as WDQS streams its results.
    async fn chunked_server(body: &'static str) -> String {
        let mut response = String::from(
            "HTTP/1.1 200 OK\r\nContent-Type: application/sparql-results+json\r\nTransfer-Encoding: chunked\r\n\r\n",
        );
        for chunk in body.as_bytes().chunks(16) {
            response += &format!(
                "{:x}\r\n{}\r\n",
                chunk.len(),
                String::from_utf8_lossy(chunk)
            );
        }
        response += "0\r\n\r\n";
        raw_server(response, true).await
    }

    /// A response declaring a Content-Length far beyond its actual body.
    async fn overclaiming_server(content_length: u64) -> String {
        raw_server(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/sparql-results+json\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n{QLEVER_RESULT}"
            ),
            true,
        )
        .await
    }

    #[tokio::test]
    async fn test_huge_declared_length_without_limit_is_not_preallocated() {
        // Reserving the declared 2^63 bytes would panic with a capacity
        // overflow; the body then ends early, which is an HTTP error.
        let url = overclaiming_server(1 << 63).await;
        let client = SparqlClient::new(SparqlEndpoint::new(url), TEST_USER_AGENT).unwrap();
        assert!(matches!(
            client.query("SELECT * {}").await,
            Err(SparqlError::Http(_))
        ));
    }

    #[tokio::test]
    async fn test_huge_declared_length_over_limit_is_refused_unread() {
        let url = overclaiming_server(1 << 63).await;
        let client = SparqlClient::new(SparqlEndpoint::new(url), TEST_USER_AGENT)
            .unwrap()
            .with_max_response_bytes(1 << 30);
        assert!(matches!(
            client.query("SELECT * {}").await,
            Err(SparqlError::ResponseTooLarge { limit }) if limit == 1 << 30
        ));
    }

    #[tokio::test]
    async fn test_error_body_is_read_only_up_to_the_cap() {
        // The body declares more than it sends and the connection stays
        // open, so reading it whole would wait for the client timeout.
        let padding = " ".repeat(ERROR_BODY_BYTES * 2);
        let url = raw_server(
            format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\n\r\nUpstream down{padding}",
                ERROR_BODY_BYTES * 100
            ),
            false,
        )
        .await;
        let client = SparqlClient::new(SparqlEndpoint::new(url), TEST_USER_AGENT).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(10), client.query("ASK {}"))
            .await
            .expect("the error body is not read to its end");
        match result {
            Err(SparqlError::Endpoint { status, message }) => {
                assert_eq!(status, 503);
                assert_eq!(message, "Upstream down");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_streamed_response_over_limit_is_abandoned() {
        let url = chunked_server(QLEVER_RESULT).await;
        let client = SparqlClient::new(SparqlEndpoint::new(url), TEST_USER_AGENT)
            .unwrap()
            .with_max_response_bytes(100);
        assert!(matches!(
            client.query("SELECT * {}").await,
            Err(SparqlError::ResponseTooLarge { limit: 100 })
        ));
    }

    #[tokio::test]
    async fn test_streamed_response_within_limit_is_read() {
        let url = chunked_server(QLEVER_RESULT).await;
        let client = SparqlClient::new(SparqlEndpoint::new(url), TEST_USER_AGENT)
            .unwrap()
            .with_max_response_bytes(QLEVER_RESULT.len());
        assert_eq!(
            client.query("SELECT * {}").await.unwrap().bindings().len(),
            1
        );
    }

    #[tokio::test]
    async fn test_query_csv_rejects_html_served_as_success() {
        let server = MockServer::start().await;
        mount(
            &server,
            // `set_body_raw`, as `set_body_string` would make it `text/plain`.
            ResponseTemplate::new(200)
                .set_body_raw(WIKIMEDIA_RATE_LIMIT, "text/html; charset=utf-8"),
        )
        .await;
        let client = client_for(&server);
        match client.query_csv("SELECT * {}").await {
            Err(SparqlError::UnexpectedContentType {
                content_type,
                expected,
                message,
            }) => {
                assert_eq!(content_type, "text/html; charset=utf-8");
                assert_eq!(expected, "CSV");
                assert!(
                    message.starts_with("Aggressively rate-limiting"),
                    "{message}"
                );
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_csv_accepts_wdqs_content_type() {
        let server = MockServer::start().await;
        mount(
            &server,
            ResponseTemplate::new(200).set_body_raw("x\r\nv\r\n", "text/csv;charset=utf-8"),
        )
        .await;
        let client = client_for(&server);
        let mut reader = client.query_csv("SELECT * {}").await.unwrap();
        assert_eq!(reader.records().count(), 1);
    }

    #[tokio::test]
    async fn test_query_csv_error_status_is_an_error() {
        let server = MockServer::start().await;
        mount(&server, json_response(400, QLEVER_SYNTAX_ERROR)).await;
        let client = client_for(&server);
        assert!(matches!(
            client.query_csv("SELECT * {}").await,
            Err(SparqlError::Endpoint { status: 400, .. })
        ));
    }

    // ── errors ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_qlever_syntax_error() {
        match error_for(json_response(400, QLEVER_SYNTAX_ERROR)).await {
            SparqlError::Endpoint { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(
                    message,
                    "Invalid SPARQL query: Prefix wdt was not registered using a PREFIX declaration"
                );
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_wdqs_v2_error() {
        match error_for(json_response(400, WDQS_V2_FEDERATION_ERROR)).await {
            SparqlError::Endpoint { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(
                    message,
                    "Endpoint http://wikiba.se/ontology#label is not allowed for query federation."
                );
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_qlever_timeout_is_not_rate_limiting() {
        // QLever reports a timeout as HTTP 429.
        match error_for(json_response(429, QLEVER_TIMEOUT)).await {
            SparqlError::Timeout { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(message, "Operation timed out.");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_wdqs_v2_timeouts() {
        for (body, expected) in [
            (WDQS_V2_GATEWAY_TIMEOUT, "Operation timed out."),
            (
                WDQS_V2_BACKEND_TIMEOUT,
                "The timeout period of 60000ms has been exceeded",
            ),
        ] {
            match error_for(json_response(504, body)).await {
                SparqlError::Timeout { status, message } => {
                    assert_eq!(status, 504);
                    assert!(message.starts_with(expected), "{message}");
                }
                other => panic!("got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn test_blazegraph_syntax_error() {
        let response = ResponseTemplate::new(400).set_body_string(BLAZEGRAPH_SYNTAX_ERROR);
        match error_for(response).await {
            SparqlError::Endpoint { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(
                    message,
                    r#"Encountered " "limit" "LIMIT "" at line 1, column 37."#
                );
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_blazegraph_timeout() {
        let response = ResponseTemplate::new(500).set_body_string(BLAZEGRAPH_TIMEOUT);
        match error_for(response).await {
            SparqlError::Timeout { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "java.util.concurrent.TimeoutException");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_wdqs_rate_limit() {
        let response = ResponseTemplate::new(429)
            .set_body_string(WIKIMEDIA_RATE_LIMIT)
            .insert_header("content-type", "text/html; charset=utf-8")
            .insert_header("retry-after", "1000");
        match error_for(response).await {
            SparqlError::RateLimited {
                retry_after,
                message,
            } => {
                assert_eq!(retry_after, Some(Duration::from_secs(1000)));
                assert_eq!(
                    message,
                    "Aggressively rate-limiting to 1 req / min - this rule was created during active wdqs outage (d7f65f1)"
                );
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_success_status_with_non_json_body() {
        let response = ResponseTemplate::new(200).set_body_string(WIKIMEDIA_RATE_LIMIT);
        match error_for(response).await {
            SparqlError::InvalidResponse { snippet, .. } => {
                assert!(snippet.starts_with("<!DOCTYPE html>"), "{snippet}");
                assert!(snippet.chars().count() <= SNIPPET_CHARS);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_truncated_result_is_invalid_response() {
        let truncated = &QLEVER_RESULT[..QLEVER_RESULT.len() / 2];
        let error = error_for(json_response(200, truncated)).await;
        assert!(matches!(error, SparqlError::InvalidResponse { .. }));
        assert!(
            std::error::Error::source(&error).is_some(),
            "the serde error stays in the source chain"
        );
    }

    #[tokio::test]
    async fn test_empty_error_body() {
        match error_for(ResponseTemplate::new(503)).await {
            SparqlError::Endpoint { status, message } => {
                assert_eq!(status, 503);
                assert_eq!(message, "(no details)");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_failure_is_http_error() {
        // Port 1 (tcpmux) is not served on any machine running the tests, so
        // the connection is refused. (A dropped `MockServer` would not do: it
        // goes back to wiremock's pool and keeps answering.)
        let client = SparqlClient::new(
            SparqlEndpoint::new("http://127.0.0.1:1/sparql"),
            TEST_USER_AGENT,
        )
        .unwrap();
        let error = client.query("ASK {}").await.unwrap_err();
        assert!(matches!(error, SparqlError::Http(_)));
        assert!(error.is_retryable());
    }

    #[tokio::test]
    #[ignore = "queries the live QLever endpoint"]
    async fn test_live_qlever_wikidata() {
        let client = SparqlClient::new(SparqlEndpoint::qlever_wikidata(), TEST_USER_AGENT).unwrap();
        // Written for WDQS: `wd:` and `wdt:` come from the endpoint's prefixes.
        let table = client
            .query_table(
                "SELECT ?item ?coord WHERE { VALUES ?item { wd:Q64 } ?item wdt:P625 ?coord }",
            )
            .await
            .unwrap();
        assert_eq!(
            table.get_row_col(0, 0),
            Some(SparqlValue::Entity("Q64".into()))
        );
        assert!(matches!(
            table.get_row_col(0, 1),
            Some(SparqlValue::Location(_))
        ));

        let ask = client.query("ASK { wd:Q64 wdt:P31 ?class }").await.unwrap();
        assert_eq!(ask.boolean(), Some(true));

        let error = client
            .query("SELECT ?x WHERE { ?x wdt:P31 wd:Q5  LIMIT 1")
            .await
            .unwrap_err();
        assert!(
            matches!(error, SparqlError::Endpoint { status: 400, .. }),
            "{error}"
        );
    }

    // ── retries ─────────────────────────────────────────────────────────────

    /// A client retrying up to twice, without waiting long.
    fn retrying_client_for(server: &MockServer) -> SparqlClient {
        client_for(server)
            .with_retry_policy(RetryPolicy::new(2).with_initial_backoff(Duration::from_millis(1)))
    }

    /// Answers the first request with `first`, any later one with `then`.
    async fn mount_sequence(server: &MockServer, first: ResponseTemplate, then: ResponseTemplate) {
        Mock::given(method("POST"))
            .respond_with(first)
            .up_to_n_times(1)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .respond_with(then)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn test_server_error_is_retried() {
        let server = MockServer::start().await;
        mount_sequence(
            &server,
            ResponseTemplate::new(503),
            json_response(200, QLEVER_RESULT),
        )
        .await;
        let result = retrying_client_for(&server).query("SELECT * {}").await;
        assert_eq!(result.unwrap().bindings().len(), 1);
    }

    #[tokio::test]
    async fn test_rate_limit_is_retried() {
        let server = MockServer::start().await;
        let limited = ResponseTemplate::new(429).insert_header("retry-after", "0");
        mount_sequence(
            &server,
            limited,
            ResponseTemplate::new(200).set_body_raw("?x\n<a>\n", "text/tab-separated-values"),
        )
        .await;
        let tsv = retrying_client_for(&server)
            .query_tsv("SELECT * {}")
            .await
            .unwrap();
        assert_eq!(tsv.rows(), [["a"]]);
    }

    #[tokio::test]
    async fn test_retries_run_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(502))
            .expect(3)
            .mount(&server)
            .await;
        assert!(matches!(
            retrying_client_for(&server).query_csv("SELECT * {}").await,
            Err(SparqlError::Endpoint { status: 502, .. })
        ));
    }

    #[tokio::test]
    async fn test_timeouts_and_query_errors_are_not_retried() {
        for response in [
            json_response(504, WDQS_V2_BACKEND_TIMEOUT),
            json_response(400, WDQS_V2_FEDERATION_ERROR),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            assert!(retrying_client_for(&server).query("ASK {}").await.is_err());
        }
    }

    #[tokio::test]
    async fn test_retry_hook_is_called() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static RETRIES: AtomicU32 = AtomicU32::new(0);
        fn record(retry: &Retry<'_>) {
            assert!(retry.endpoint.ends_with(SPARQL_PATH));
            assert!(matches!(
                retry.error,
                SparqlError::Endpoint { status: 500, .. }
            ));
            RETRIES.store(retry.retry, Ordering::SeqCst);
        }
        let server = MockServer::start().await;
        mount_sequence(
            &server,
            ResponseTemplate::new(500),
            json_response(200, r#"{"boolean":true,"head":{}}"#),
        )
        .await;
        let client = client_for(&server).with_retry_policy(
            RetryPolicy::new(1)
                .with_initial_backoff(Duration::from_millis(1))
                .on_retry(record),
        );
        assert!(client.ask("ASK {}").await.unwrap());
        assert_eq!(RETRIES.load(Ordering::SeqCst), 1);
    }

    // ── error-message extraction ────────────────────────────────────────────

    #[test]
    fn test_error_message_plain_text_is_collapsed_and_truncated() {
        assert_eq!(
            error_message("  Service\n  Unavailable  "),
            "Service Unavailable"
        );
        let long = "x".repeat(MESSAGE_CHARS * 2);
        assert_eq!(error_message(&long).chars().count(), MESSAGE_CHARS);
    }

    #[test]
    fn test_error_message_html_without_paragraph_uses_title() {
        let html = "<html><head><title>502 Bad Gateway</title></head><body></body></html>";
        assert_eq!(error_message(html), "502 Bad Gateway");
    }

    #[test]
    fn test_error_message_html_strips_inner_tags() {
        let html = "<html><p>Please <a href=\"x\">set a user agent</a>.</p></html>";
        assert_eq!(error_message(html), "Please set a user agent .");
    }

    #[test]
    fn test_error_message_blazegraph_nested_exceptions_use_innermost() {
        let body = "java.util.concurrent.ExecutionException: java.lang.RuntimeException: com.bigdata.bop.engine.QueryTimeoutException: Query deadline is expired.\n\tat x";
        assert_eq!(error_message(body), "Query deadline is expired.");
        assert!(matches!(
            SparqlError::from_error_response(500, None, body),
            SparqlError::Timeout { .. }
        ));
    }

    #[test]
    fn test_rate_limit_without_retry_after() {
        assert!(matches!(
            SparqlError::from_error_response(429, None, "slow down"),
            SparqlError::RateLimited {
                retry_after: None,
                ..
            }
        ));
    }

    #[test]
    fn test_retry_after_ignores_http_date_form() {
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
        headers.insert(RETRY_AFTER, " 30 ".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_display_includes_status_and_message() {
        let error = SparqlError::from_error_response(400, None, QLEVER_SYNTAX_ERROR);
        assert_eq!(
            error.to_string(),
            "SPARQL endpoint returned HTTP 400: Invalid SPARQL query: Prefix wdt was not registered using a PREFIX declaration"
        );
    }

    #[test]
    fn test_all_static_regexes_compile() {
        assert!(RE_HTML_P.is_some());
        assert!(RE_HTML_TITLE.is_some());
        assert!(RE_HTML_TAG.is_some());
    }
}
