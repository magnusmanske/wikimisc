//! User agents, HTTP clients and retried requests.
//!
//! Wikimedia's [user-agent policy](https://foundation.wikimedia.org/wiki/Policy:Wikimedia_Foundation_User-Agent_Policy)
//! asks every tool to identify itself with contact information, as
//! `ToolName/1.0 (https://tool.example; tool@example.org)`. [`user_agent!`](crate::user_agent)
//! builds that string at compile time from the calling crate's `Cargo.toml`,
//! and [`client_builder`] starts a `reqwest` client with it and with sensible
//! timeouts:
//!
//! ```rust,no_run
//! use wikimisc::http::{self, HttpError, HttpRetryPolicy};
//!
//! const USER_AGENT: &str = wikimisc::user_agent!("https://mytool.toolforge.org; me@example.org");
//!
//! async fn page_html() -> Result<String, HttpError> {
//!     let client = http::client(USER_AGENT)?;
//!     let response = HttpRetryPolicy::new(3)
//!         .send(client.get("https://en.wikipedia.org/wiki/Douglas_Adams"))
//!         .await?;
//!     Ok(response.text().await?)
//! }
//! ```
//!
//! [`RetryPolicy::send`](crate::retry::RetryPolicy::send) retries what can
//! go away on its own — HTTP 408, 429 and 5xx, connection failures, requests
//! that broke on the way, and timeouts — honouring `Retry-After` (in seconds
//! or as an HTTP date), and turns any other error status into
//! [`HttpError::Status`] with the start of the body, so callers no longer
//! need `error_for_status`. [`RetryPolicy::fetch_json`],
//! [`RetryPolicy::fetch_text`] and [`RetryPolicy::fetch_bytes`] retry the
//! whole exchange, including reading the body; [`send`] makes a single
//! attempt.

use crate::retry::{RetryPolicy, Retryable};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Client, ClientBuilder, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use std::future::Future;
use std::time::{Duration, SystemTime};
use thiserror::Error;

/// The total time a request may take, unless set otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The time connecting to a server may take, unless set otherwise.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of an error response is read for [`HttpError::Status`].
const ERROR_BODY_BYTES: usize = 64 * 1024;

/// How much of an error response [`HttpError::Status`] quotes.
const ERROR_SNIPPET_CHARS: usize = 200;

/// The most memory [`read_body_capped`] reserves up front, whatever size the
/// response declares: the declaration is the server's claim, not data.
const MAX_PREALLOCATION: usize = 1024 * 1024;

/// A user agent as Wikimedia's user-agent policy asks for it,
/// `name/version (contact)`, built at compile time from the **calling**
/// crate's `Cargo.toml`. Usable in a `const`.
///
/// - `user_agent!()` — `name/version (repository)`; fails to compile if the
///   crate declares no `repository`.
/// - `user_agent!("https://mytool.toolforge.org; me@example.org")` —
///   `name/version (contact)`.
/// - `user_agent!(name = "PetScan", "https://petscan.wmcloud.org")` — a tool
///   name other than the crate name.
/// - `user_agent!(description = "personal media catalogue")` —
///   `name/version (description; repository)`; fails to compile if the crate
///   declares no `repository`. Also with a name:
///   `user_agent!(name = "PView", description = "…")`.
///
/// ```
/// const UA: &str = wikimisc::user_agent!("https://example.org; me@example.org");
/// assert!(UA.starts_with("wikimisc/"));
/// assert!(UA.ends_with(" (https://example.org; me@example.org)"));
/// ```
#[macro_export]
macro_rules! user_agent {
    (description = $description:literal $(,)?) => {{
        const _: () = ::core::assert!(
            !::core::env!("CARGO_PKG_REPOSITORY").is_empty(),
            "user_agent!(description = …) needs `repository` in Cargo.toml"
        );
        ::core::concat!(
            ::core::env!("CARGO_PKG_NAME"),
            "/",
            ::core::env!("CARGO_PKG_VERSION"),
            " (",
            $description,
            "; ",
            ::core::env!("CARGO_PKG_REPOSITORY"),
            ")"
        )
    }};
    (name = $name:literal, description = $description:literal $(,)?) => {{
        const _: () = ::core::assert!(
            !::core::env!("CARGO_PKG_REPOSITORY").is_empty(),
            "user_agent!(name = …, description = …) needs `repository` in Cargo.toml"
        );
        ::core::concat!(
            $name,
            "/",
            ::core::env!("CARGO_PKG_VERSION"),
            " (",
            $description,
            "; ",
            ::core::env!("CARGO_PKG_REPOSITORY"),
            ")"
        )
    }};
    () => {{
        const _: () = ::core::assert!(
            !::core::env!("CARGO_PKG_REPOSITORY").is_empty(),
            "user_agent!() needs `repository` in Cargo.toml; or pass contact information: user_agent!(\"https://…; me@example.org\")"
        );
        ::core::concat!(
            ::core::env!("CARGO_PKG_NAME"),
            "/",
            ::core::env!("CARGO_PKG_VERSION"),
            " (",
            ::core::env!("CARGO_PKG_REPOSITORY"),
            ")"
        )
    }};
    (name = $name:literal, $contact:literal $(,)?) => {
        ::core::concat!($name, "/", ::core::env!("CARGO_PKG_VERSION"), " (", $contact, ")")
    };
    ($contact:literal $(,)?) => {
        ::core::concat!(
            ::core::env!("CARGO_PKG_NAME"),
            "/",
            ::core::env!("CARGO_PKG_VERSION"),
            " (",
            $contact,
            ")"
        )
    };
}

/// Failure modes of HTTP requests made through this module.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HttpError {
    /// The server answered with an error status (4xx or 5xx). `body` is the
    /// start of the response, for its explanation; `retry_after` the wait the
    /// server asked for, if any.
    #[error("HTTP {status} from {url}{}", body_suffix(.body))]
    Status {
        status: u16,
        url: String,
        retry_after: Option<Duration>,
        body: String,
    },

    /// The request could not be sent or the response not received, including
    /// the client's timeout expiring.
    #[error(transparent)]
    Transport(#[from] reqwest::Error),

    /// The response is larger than the limit passed to
    /// [`read_body_capped`]; reading it was abandoned.
    #[error("response is larger than the {limit}-byte limit")]
    TooLarge { limit: usize },

    /// The response body is not what was expected, e.g. not valid JSON.
    /// Retrying would get the same answer, so this is not retried.
    #[error("unexpected response from {url}: {message}")]
    Decode { url: String, message: String },

    /// The request has a streaming body, which can be sent only once, but a
    /// retry was attempted. [`RetryPolicy::send`] sends such a request once
    /// and does not retry it, so this is not expected in practice.
    #[error("the request to {url} has a streaming body and cannot be sent again")]
    NotRepeatable { url: String },
}

fn body_suffix(body: &str) -> String {
    if body.is_empty() {
        String::new()
    } else {
        format!(": {body}")
    }
}

impl HttpError {
    /// The HTTP status, if the server answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            Self::Transport(error) => error.status().map(|status| status.as_u16()),
            _ => None,
        }
    }
}

impl Retryable for HttpError {
    /// HTTP 408 (request timeout), 429 (rate limited) and 5xx, and requests
    /// that failed to connect, broke while being sent (e.g. on a reused
    /// connection the server had closed), broke while the body was read, or
    /// timed out. Never a malformed request, a redirect loop or an
    /// undecodable body.
    fn is_retryable(&self) -> bool {
        match self {
            Self::Status { status, .. } => {
                *status == StatusCode::REQUEST_TIMEOUT.as_u16()
                    || *status == StatusCode::TOO_MANY_REQUESTS.as_u16()
                    || *status >= 500
            }
            Self::Transport(error) => {
                error.is_timeout()
                    || error.is_connect()
                    || error.is_request()
                    || broke_off_mid_body(error)
            }
            _ => false,
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Whether reading the body failed because the transfer broke off. reqwest
/// reports that as a body error, or — when the body is decoded on the fly,
/// as `text()` and decompression do — as a decode error caused by one; a
/// decode error of its own (bad gzip data, say) would recur on a retry.
fn broke_off_mid_body(error: &reqwest::Error) -> bool {
    if error.is_body() {
        return true;
    }
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        if cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_body)
        {
            return true;
        }
        source = cause.source();
    }
    false
}

/// A [`RetryPolicy`] for HTTP requests; see [`RetryPolicy::send`].
pub type HttpRetryPolicy = RetryPolicy<HttpError>;

/// Retrying sends the request again, so use a retrying policy only for
/// requests that are safe to repeat — any GET, but a POST only if it changes
/// nothing (a query) or repeating it is harmless. A request with a streaming
/// body cannot be repeated and is sent once.
impl RetryPolicy<HttpError> {
    /// Sends `request`, retrying as the policy says. A successful (2xx) or
    /// redirect (3xx) response is returned unread; an error status becomes
    /// [`HttpError::Status`]. Reading the body is up to the caller, and not
    /// retried — see [`Self::fetch_bytes`] and friends for that.
    pub async fn send(&self, request: RequestBuilder) -> Result<Response, HttpError> {
        self.exchange(request, |response| std::future::ready(Ok(response)))
            .await
    }

    /// The body of the response to `request`. Sending and reading are retried
    /// together, so a body that breaks off mid-way is fetched again.
    pub async fn fetch_bytes(&self, request: RequestBuilder) -> Result<Vec<u8>, HttpError> {
        self.exchange(request, |response| async move {
            Ok(response.bytes().await?.to_vec())
        })
        .await
    }

    /// The body of the response to `request` as text, decoded as its
    /// `Content-Type` says (UTF-8 by default). Retried like
    /// [`Self::fetch_bytes`].
    pub async fn fetch_text(&self, request: RequestBuilder) -> Result<String, HttpError> {
        self.exchange(
            request,
            |response| async move { Ok(response.text().await?) },
        )
        .await
    }

    /// The body of the response to `request`, parsed as JSON into `T`.
    /// Retried like [`Self::fetch_bytes`]; a body that does not parse (an
    /// HTML error page with status 200, say) is [`HttpError::Decode`] and is
    /// not retried.
    pub async fn fetch_json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, HttpError> {
        self.exchange(request, |response| async move {
            let url = response.url().to_string();
            let body = response.bytes().await?;
            serde_json::from_slice(&body).map_err(|error| HttpError::Decode {
                url,
                message: error.to_string(),
            })
        })
        .await
    }

    /// Sends `request` and hands the successful response to `read`, retrying
    /// both as the policy says.
    async fn exchange<T, R, Fut>(&self, request: RequestBuilder, read: R) -> Result<T, HttpError>
    where
        R: Fn(Response) -> Fut,
        Fut: Future<Output = Result<T, HttpError>>,
    {
        let Some(probe) = request.try_clone() else {
            return read(send(request).await?).await;
        };
        let url = probe
            .build()
            .map(|built| built.url().to_string())
            .unwrap_or_default();
        let read = &read;
        self.run(&url, || {
            let attempt = request.try_clone();
            let url = &url;
            async move {
                match attempt {
                    Some(attempt) => read(send(attempt).await?).await,
                    None => Err(HttpError::NotRepeatable { url: url.clone() }),
                }
            }
        })
        .await
    }
}

/// Sends `request` once. A successful (2xx) or redirect (3xx) response is
/// returned unread; an error status becomes [`HttpError::Status`], quoting the
/// start of the body and the server's `Retry-After`. For retries, see
/// [`RetryPolicy::send`].
pub async fn send(request: RequestBuilder) -> Result<Response, HttpError> {
    let response = request.send().await?;
    let status = response.status();
    if !status.is_client_error() && !status.is_server_error() {
        return Ok(response);
    }
    let url = response.url().to_string();
    let retry_after = retry_after(response.headers());
    let body = read_body_capped(response, ERROR_BODY_BYTES)
        .await
        .map(|body| snippet(&String::from_utf8_lossy(&body), ERROR_SNIPPET_CHARS))
        // The status already says the request failed; an unreadable body
        // only costs the explanation.
        .unwrap_or_default();
    Err(HttpError::Status {
        status: status.as_u16(),
        url,
        retry_after,
        body,
    })
}

/// A client builder sending `user_agent`, with [`DEFAULT_TIMEOUT`],
/// [`DEFAULT_CONNECT_TIMEOUT`] and gzip compression; adjust as needed before
/// `build()`.
pub fn client_builder(user_agent: &str) -> ClientBuilder {
    Client::builder()
        .user_agent(user_agent)
        .timeout(DEFAULT_TIMEOUT)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .gzip(true)
}

/// A client as made by [`client_builder`].
pub fn client(user_agent: &str) -> Result<Client, HttpError> {
    Ok(client_builder(user_agent).build()?)
}

/// The wait a `Retry-After` header asks for: a number of seconds, or an HTTP
/// date (one in the past means no wait).
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    Some(
        date.duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

/// The whole response body, refused with [`HttpError::TooLarge`] as soon as
/// it is known to exceed `limit` bytes — from the declared length if there
/// is one, else while reading.
pub async fn read_body_capped(mut response: Response, limit: usize) -> Result<Vec<u8>, HttpError> {
    let declared = response
        .content_length()
        .map_or(0, |length| usize::try_from(length).unwrap_or(usize::MAX));
    if declared > limit {
        return Err(HttpError::TooLarge { limit });
    }
    let mut body = Vec::with_capacity(declared.min(MAX_PREALLOCATION));
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(HttpError::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The start of `text`, trimmed, at most `max_chars` characters, with `…`
/// appended if anything was cut — for quoting a response in an error.
pub fn snippet(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", text.get(..cut).unwrap_or(text).trim_end()),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

    const UA: &str = crate::user_agent!("https://example.org; test@example.org");

    /// Answers with the given responses in turn, repeating the last one.
    struct Sequence(Vec<ResponseTemplate>, AtomicU32);

    impl Respond for Sequence {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let n = self.1.fetch_add(1, Ordering::SeqCst) as usize;
            self.0[n.min(self.0.len() - 1)].clone()
        }
    }

    async fn server_with(responses: Vec<ResponseTemplate>) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(Sequence(responses, AtomicU32::new(0)))
            .mount(&server)
            .await;
        server
    }

    fn fast(retries: u32) -> HttpRetryPolicy {
        HttpRetryPolicy::new(retries).with_initial_backoff(Duration::from_millis(1))
    }

    #[test]
    fn test_user_agent_forms() {
        const DEFAULT: &str = crate::user_agent!();
        const NAMED: &str = crate::user_agent!(name = "Tool", "contact");
        const DESCRIBED: &str = crate::user_agent!(description = "test suite");
        const NAMED_AND_DESCRIBED: &str =
            crate::user_agent!(name = "Tool", description = "test suite",);
        assert_eq!(
            DESCRIBED,
            concat!(
                "wikimisc/",
                env!("CARGO_PKG_VERSION"),
                " (test suite; https://github.com/magnusmanske/wikimisc)"
            )
        );
        assert_eq!(
            NAMED_AND_DESCRIBED,
            concat!(
                "Tool/",
                env!("CARGO_PKG_VERSION"),
                " (test suite; https://github.com/magnusmanske/wikimisc)"
            )
        );
        assert_eq!(
            DEFAULT,
            concat!(
                "wikimisc/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/magnusmanske/wikimisc)"
            )
        );
        assert_eq!(
            UA,
            concat!(
                "wikimisc/",
                env!("CARGO_PKG_VERSION"),
                " (https://example.org; test@example.org)"
            )
        );
        assert_eq!(
            NAMED,
            concat!("Tool/", env!("CARGO_PKG_VERSION"), " (contact)")
        );
    }

    #[test]
    fn test_retry_after_seconds_and_dates() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(RETRY_AFTER, " 30 ".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), Some(Duration::ZERO));
        let soon = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(120));
        headers.insert(RETRY_AFTER, soon.parse().unwrap());
        let wait = retry_after(&headers).unwrap();
        assert!(
            wait > Duration::from_secs(100) && wait <= Duration::from_secs(120),
            "{wait:?}"
        );
        headers.insert(RETRY_AFTER, "soon".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn test_snippet() {
        assert_eq!(snippet("  short  ", 10), "short");
        assert_eq!(snippet("abcdef", 3), "abc…");
        assert_eq!(snippet("äöüß", 2), "äö…");
        assert_eq!(snippet("", 2), "");
    }

    #[test]
    fn test_retryable_statuses() {
        let status = |status| HttpError::Status {
            status,
            url: String::new(),
            retry_after: None,
            body: String::new(),
        };
        for code in [408, 429, 500, 502, 503, 504] {
            assert!(status(code).is_retryable(), "{code}");
        }
        for code in [400, 401, 403, 404, 410] {
            assert!(!status(code).is_retryable(), "{code}");
        }
        assert!(!HttpError::TooLarge { limit: 1 }.is_retryable());
        let decode = HttpError::Decode {
            url: String::new(),
            message: String::new(),
        };
        assert!(!decode.is_retryable());
    }

    #[tokio::test]
    async fn test_transport_errors_that_are_retried() {
        let client = client(UA).unwrap();
        // Nothing listens on port 1 of the loopback interface: a connect error.
        let error = send(client.get("http://127.0.0.1:1/")).await.unwrap_err();
        assert!(error.is_retryable(), "{error:?}");
        // A malformed URL fails before anything is sent: not retried.
        let error = send(client.get("not a url")).await.unwrap_err();
        assert!(!error.is_retryable(), "{error:?}");
    }

    #[test]
    fn test_status_display_quotes_the_body_only_if_there_is_one() {
        let error = HttpError::Status {
            status: 404,
            url: "https://x/".into(),
            retry_after: None,
            body: String::new(),
        };
        assert_eq!(error.to_string(), "HTTP 404 from https://x/");
        let error = HttpError::Status {
            status: 503,
            url: "https://x/".into(),
            retry_after: None,
            body: "down".into(),
        };
        assert_eq!(error.to_string(), "HTTP 503 from https://x/: down");
        assert_eq!(error.status(), Some(503));
    }

    #[tokio::test]
    async fn test_send_retries_transient_errors_and_sends_the_user_agent() {
        let server = server_with(vec![
            ResponseTemplate::new(503).set_body_string("busy"),
            ResponseTemplate::new(429).insert_header("Retry-After", "0"),
            ResponseTemplate::new(200).set_body_string("ok"),
        ])
        .await;
        let retries = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&retries);
        let policy = fast(3).on_retry(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let client = client(UA).unwrap();
        let response = policy
            .send(client.get(format!("{}/x", server.uri())))
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        assert_eq!(retries.load(Ordering::SeqCst), 2);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].headers["user-agent"], UA);
    }

    #[tokio::test]
    async fn test_send_returns_a_client_error_at_once_with_its_body() {
        let server = server_with(vec![
            ResponseTemplate::new(404).set_body_string(" no such page ")
        ])
        .await;
        let client = client(UA).unwrap();
        let error = fast(3)
            .send(client.get(format!("{}/x", server.uri())))
            .await
            .unwrap_err();
        match error {
            HttpError::Status {
                status, body, url, ..
            } => {
                assert_eq!(status, 404);
                assert_eq!(body, "no such page");
                assert!(url.ends_with("/x"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_send_gives_up_with_the_last_error() {
        let server = server_with(vec![ResponseTemplate::new(500)]).await;
        let client = client(UA).unwrap();
        let error = fast(2)
            .send(client.get(format!("{}/x", server.uri())))
            .await
            .unwrap_err();
        assert_eq!(error.status(), Some(500));
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_send_makes_a_single_attempt() {
        let server = server_with(vec![
            ResponseTemplate::new(503).insert_header("Retry-After", "7"),
            ResponseTemplate::new(200),
        ])
        .await;
        let client = client(UA).unwrap();
        let error = send(client.get(format!("{}/x", server.uri())))
            .await
            .unwrap_err();
        assert_eq!(error.status(), Some(503));
        assert_eq!(error.retry_after(), Some(Duration::from_secs(7)));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_fetch_json_text_and_bytes() {
        let server = server_with(vec![
            ResponseTemplate::new(502),
            ResponseTemplate::new(200).set_body_string(r#"{"answer": 42}"#),
        ])
        .await;
        let client = client(UA).unwrap();
        let url = format!("{}/x", server.uri());
        let json: serde_json::Value = fast(2).fetch_json(client.get(&url)).await.unwrap();
        assert_eq!(json["answer"], 42);
        assert_eq!(
            fast(0).fetch_text(client.get(&url)).await.unwrap(),
            r#"{"answer": 42}"#
        );
        assert_eq!(
            fast(0).fetch_bytes(client.get(&url)).await.unwrap(),
            br#"{"answer": 42}"#
        );
    }

    #[tokio::test]
    async fn test_fetch_json_does_not_retry_a_body_that_is_not_json() {
        let server = server_with(vec![
            ResponseTemplate::new(200).set_body_string("<html>rate limited</html>")
        ])
        .await;
        let client = client(UA).unwrap();
        let error = fast(3)
            .fetch_json::<serde_json::Value>(client.get(format!("{}/x", server.uri())))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, HttpError::Decode { url, .. } if url.ends_with("/x")),
            "{error:?}"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_fetch_retries_a_body_that_breaks_off() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // A raw server: the first answer promises 100 bytes and hangs up
        // after 5, the second is complete.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for answer in [
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nhello",
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                socket.write_all(answer.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        let client = client(UA).unwrap();
        let text = fast(2)
            .fetch_text(client.get(format!("http://{address}/x")))
            .await
            .unwrap();
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn test_read_body_capped() {
        let server = server_with(vec![
            ResponseTemplate::new(200).set_body_string("0123456789")
        ])
        .await;
        let client = client(UA).unwrap();
        let url = format!("{}/x", server.uri());
        let body = read_body_capped(client.get(&url).send().await.unwrap(), 10)
            .await
            .unwrap();
        assert_eq!(body, b"0123456789");
        let error = read_body_capped(client.get(&url).send().await.unwrap(), 9)
            .await
            .unwrap_err();
        assert!(matches!(error, HttpError::TooLarge { limit: 9 }));
    }
}
