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
//!   Blazegraph with a Java stack trace, and Wikimedia's front end with an
//!   HTML page. [`SparqlError`] reads all three, and tells a query timeout
//!   (which QLever reports as HTTP 429) from rate limiting (which WDQS
//!   reports as HTTP 429).
//!
//! ```rust,no_run
//! use wikimisc::sparql_client::{SparqlClient, SparqlError};
//! use wikimisc::sparql_endpoint::SparqlEndpoint;
//!
//! async fn humans() -> Result<usize, SparqlError> {
//!     let client = SparqlClient::new(SparqlEndpoint::qlever_wikidata())?;
//!     let table = client
//!         .query_table("SELECT ?item WHERE { ?item wdt:P31 wd:Q5 } LIMIT 10")
//!         .await?;
//!     Ok(table.len())
//! }
//! ```

use crate::sparql_endpoint::SparqlEndpoint;
use crate::sparql_results::SparqlApiResult;
use crate::sparql_table::{SparqlTableError, SparqlTableVec};
use regex::Regex;
use reqwest::header::{HeaderMap, ACCEPT, RETRY_AFTER};
use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::sync::LazyLock;
use std::time::Duration;
use tempfile::tempfile;
use thiserror::Error;

const DEFAULT_USER_AGENT: &str = concat!(
    "wikimisc/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const ACCEPT_JSON: &str = "application/sparql-results+json";
const ACCEPT_CSV: &str = "text/csv";
/// How much of an unparseable response body an error quotes.
const SNIPPET_CHARS: usize = 200;
/// How much of an endpoint's error message an error keeps.
const MESSAGE_CHARS: usize = 500;

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

    /// The endpoint gave up on the query because it ran too long. Retrying the
    /// same query is unlikely to help.
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
        let timed_out =
            message.to_lowercase().contains("timed out") || body.contains("TimeoutException");
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
}

/// Sends SPARQL queries to one [`SparqlEndpoint`].
///
/// Queries are POSTed form-encoded, so their length is not limited by the
/// maximum URL length, and the configured prefixes are added first.
#[derive(Debug, Clone)]
pub struct SparqlClient {
    endpoint: SparqlEndpoint,
    client: Client,
}

impl SparqlClient {
    /// A client with a generic `wikimisc` user agent and a 60-second timeout.
    ///
    /// Wikimedia's [user-agent policy](https://foundation.wikimedia.org/wiki/Policy:Wikimedia_Foundation_User-Agent_Policy)
    /// asks every tool to identify itself, so for WDQS prefer
    /// [`Self::with_client`] with a client carrying the tool's own user agent
    /// — for example `Wikidata::reqwest_client()`.
    pub fn new(endpoint: SparqlEndpoint) -> Result<Self, SparqlError> {
        let client = Client::builder()
            .user_agent(DEFAULT_USER_AGENT)
            .timeout(DEFAULT_TIMEOUT)
            .build()?;
        Ok(Self::with_client(endpoint, client))
    }

    /// A client sending its requests through `client`, whose user agent,
    /// timeout and other settings are used as they are.
    pub fn with_client(endpoint: SparqlEndpoint, client: Client) -> Self {
        Self { endpoint, client }
    }

    pub fn endpoint(&self) -> &SparqlEndpoint {
        &self.endpoint
    }

    /// Runs a SELECT or ASK query and parses the result.
    pub async fn query(&self, sparql: &str) -> Result<SparqlApiResult, SparqlError> {
        parse_json(&self.fetch_json(sparql).await?)
    }

    /// Runs a query and returns the result as untyped JSON, for callers that
    /// process the bindings themselves.
    pub async fn query_json(&self, sparql: &str) -> Result<serde_json::Value, SparqlError> {
        parse_json(&self.fetch_json(sparql).await?)
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
        let mut response = self.send(sparql, ACCEPT_CSV).await?;
        let mut file = tempfile()?;
        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk)?;
        }
        file.seek(SeekFrom::Start(0))?;
        Ok(csv::ReaderBuilder::new()
            .flexible(true)
            .has_headers(true)
            .from_reader(file))
    }

    /// The raw response body. Kept as bytes and parsed from there: decoding
    /// it into a `String` first would hold a large result in memory twice.
    async fn fetch_json(
        &self,
        sparql: &str,
    ) -> Result<impl std::ops::Deref<Target = [u8]>, SparqlError> {
        Ok(self.send(sparql, ACCEPT_JSON).await?.bytes().await?)
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
        // The status already says the query failed; an unreadable body only
        // costs the explanation, so it is not an error of its own.
        let body = response.text().await.unwrap_or_default();
        Err(SparqlError::from_error_response(
            status.as_u16(),
            retry_after,
            &body,
        ))
    }
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
        SparqlClient::new(endpoint_for(server)).unwrap()
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
        let client = SparqlClient::new(endpoint).unwrap();
        client
            .query("SELECT ?item { VALUES ?item { wd:Q64 } }")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_query_sends_a_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("user-agent", DEFAULT_USER_AGENT))
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
        let client = SparqlClient::new(SparqlEndpoint::new("http://127.0.0.1:1/sparql")).unwrap();
        assert!(matches!(
            client.query("ASK {}").await,
            Err(SparqlError::Http(_))
        ));
    }

    #[tokio::test]
    #[ignore = "queries the live QLever endpoint"]
    async fn test_live_qlever_wikidata() {
        let client = SparqlClient::new(SparqlEndpoint::qlever_wikidata()).unwrap();
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
