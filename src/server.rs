//! Glue for tools that serve HTTP with [axum]: the listen address from the
//! environment, graceful shutdown on SIGTERM/SIGINT, the usual middleware,
//! CORS for Wikimedia sites, health checks and JSONP.
//!
//! ```no_run
//! use axum::{routing::get, Router};
//! use wikimisc::server;
//!
//! # async fn example() -> std::io::Result<()> {
//! let router = Router::new()
//!     .route("/", get(|| async { "hello" }))
//!     .route("/healthz", get(server::health::ok));
//! let router = server::standard_layers(router);
//! let listener = tokio::net::TcpListener::bind(server::listen_addr("MY_TOOL", 8000)).await?;
//! server::serve(listener, router).await
//! # }
//! ```
//!
//! The middleware helpers take and return a [`Router`] rather than handing
//! out `tower-http` layer types, so a tool on another `tower-http` version
//! does not run into mismatched types.
//!
//! With the `telemetry` feature, [`request_metrics`] records every request
//! for the dashboard, and [`serve`] sends the final push once it has drained.
//! Build the telemetry with
//! [`handle_signals(false)`](crate::telemetry::TelemetryBuilder::handle_signals)
//! then: its own signal handler would exit before the drain.

pub mod health;
pub mod jsonp;

pub use crate::shutdown::{shutdown_signal, ShutdownSignal};

use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode};
use axum::Router;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// The address to listen on: `{prefix}_ADDRESS` (an IPv4 or IPv6 address,
/// default `0.0.0.0`) and `{prefix}_PORT` (default `default_port`). An
/// invalid value is logged and the default used instead.
///
/// `listen_addr("GEOHACK", 8000)` reads `GEOHACK_ADDRESS` and `GEOHACK_PORT`.
pub fn listen_addr(prefix: &str, default_port: u16) -> SocketAddr {
    let address = env_or(
        &format!("{prefix}_ADDRESS"),
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    );
    let port = env_or(&format!("{prefix}_PORT"), default_port);
    SocketAddr::new(address, port)
}

fn env_or<T: std::str::FromStr + std::fmt::Display>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Err(_) => default,
        Ok(value) => value.trim().parse().unwrap_or_else(|_| {
            log::warn!("ignoring invalid {name}='{value}', using {default}");
            default
        }),
    }
}

/// Serves `router` until SIGTERM or SIGINT, then stops accepting
/// connections and waits for the requests in flight. With the `telemetry`
/// feature, the final push (reason: the signal) follows the drain.
pub async fn serve(listener: TcpListener, router: Router) -> std::io::Result<()> {
    let signal = Arc::new(OnceLock::new());
    axum::serve(listener, router)
        .with_graceful_shutdown(drain_on_signal(signal.clone()))
        .await?;
    after_drain(&signal).await;
    Ok(())
}

/// [`serve`] for routers whose handlers take
/// [`ConnectInfo<SocketAddr>`](axum::extract::ConnectInfo), e.g. to rate
/// limit by client address.
pub async fn serve_with_connect_info(listener: TcpListener, router: Router) -> std::io::Result<()> {
    let signal = Arc::new(OnceLock::new());
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(drain_on_signal(signal.clone()))
    .await?;
    after_drain(&signal).await;
    Ok(())
}

async fn drain_on_signal(slot: Arc<OnceLock<ShutdownSignal>>) {
    let signal = shutdown_signal().await;
    log::info!("{signal} received, draining");
    let _ = slot.set(signal);
}

#[cfg_attr(not(feature = "telemetry"), allow(unused_variables))]
async fn after_drain(signal: &OnceLock<ShutdownSignal>) {
    #[cfg(feature = "telemetry")]
    if let Some(signal) = signal.get() {
        crate::telemetry::shutdown(crate::telemetry::ExitReason::Shutdown(signal.to_string()))
            .await;
    }
}

/// Turns a panicking handler into a 500 response instead of a dropped
/// connection.
pub fn catch_panic<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router.layer(CatchPanicLayer::new())
}

/// Compresses responses (gzip, brotli) for clients that accept it.
pub fn compression<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router.layer(CompressionLayer::new())
}

/// Traces each request with `tracing` (see [`crate::logging`]).
pub fn trace<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router.layer(TraceLayer::new_for_http())
}

/// Answers `status` when a request takes longer than `timeout`: 408 (the
/// client's request took too long) or 504 (the service's work did) are the
/// usual choices.
pub fn timeout<S: Clone + Send + Sync + 'static>(
    router: Router<S>,
    timeout: Duration,
    status: StatusCode,
) -> Router<S> {
    router.layer(TimeoutLayer::with_status_code(status, timeout))
}

/// Tracing, compression and panic catching, the latter innermost so a panic
/// still goes through the other two (and is counted by [`request_metrics`]
/// when that is added afterwards).
pub fn standard_layers<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    trace(compression(catch_panic(router)))
}

/// Allows cross-origin requests from anywhere, with any method and header.
/// For public read-only APIs.
pub fn cors_any<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router.layer(CorsLayer::permissive())
}

/// Which origins [`cors_wikimedia`] admits, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WikimediaCors {
    /// Whether cookies may be sent along (`Access-Control-Allow-Credentials`).
    pub credentials: bool,
    /// Whether local development origins (`localhost`, `127.0.0.1`, `[::1]`,
    /// any port, http or https) are admitted as well.
    pub allow_localhost: bool,
}

impl Default for WikimediaCors {
    /// No credentials; local origins only when not running on Toolforge.
    fn default() -> Self {
        Self {
            credentials: false,
            allow_localhost: !crate::toolforge_app::ToolforgeApp::is_on_toolforge(),
        }
    }
}

/// Allows cross-origin requests from Wikimedia sites (see
/// [`is_wikimedia_origin`]): GET, POST and OPTIONS, with the `Content-Type`,
/// `X-Requested-With` and `Api-User-Agent` headers (the last is how gadgets
/// identify themselves, as browsers do not let scripts set `User-Agent`).
/// For APIs called by user scripts and gadgets.
pub fn cors_wikimedia<S: Clone + Send + Sync + 'static>(
    router: Router<S>,
    config: WikimediaCors,
) -> Router<S> {
    let allow_localhost = config.allow_localhost;
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin: &HeaderValue, _| {
            origin
                .to_str()
                .is_ok_and(|origin| is_wikimedia_origin(origin, allow_localhost))
        }))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static("x-requested-with"),
            HeaderName::from_static("api-user-agent"),
        ])
        .allow_credentials(config.credentials);
    router.layer(cors)
}

/// The domains (and their subdomains) [`is_wikimedia_origin`] admits.
const WIKIMEDIA_DOMAINS: &[&str] = &[
    "mediawiki.org",
    "toolforge.org",
    "wikibooks.org",
    "wikidata.org",
    "wikimedia.org",
    "wikinews.org",
    "wikipedia.org",
    "wikiquote.org",
    "wikisource.org",
    "wikiversity.org",
    "wikivoyage.org",
    "wiktionary.org",
    "wmcloud.org",
];

/// Whether `origin` (an `Origin` header: `scheme://host[:port]`) is an https
/// Wikimedia site or tool: one of the project domains, Toolforge or Cloud
/// VPS, or a subdomain. With `allow_localhost`, local development origins
/// count too.
pub fn is_wikimedia_origin(origin: &str, allow_localhost: bool) -> bool {
    let (scheme, authority) = match origin.split_once("://") {
        Some(parts) => parts,
        None => return false,
    };
    if allow_localhost && matches!(scheme, "http" | "https") && is_local(authority) {
        return true;
    }
    if scheme != "https" {
        return false;
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    }
    .to_ascii_lowercase();
    WIKIMEDIA_DOMAINS.iter().any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.'))
    })
}

fn is_local(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // IPv6: `[::1]` or `[::1]:8000`.
        match rest.split_once(']') {
            Some((host, port)) if port.is_empty() || port.starts_with(':') => host,
            _ => return false,
        }
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Records each request for the telemetry dashboard
/// ([`record_http_request`](crate::telemetry::record_http_request)), except
/// those whose path is in `skip_paths` (health checks, `/metrics`). Add it
/// after [`standard_layers`], so it also counts panics and timeouts.
#[cfg(feature = "telemetry")]
pub fn request_metrics<S: Clone + Send + Sync + 'static>(
    router: Router<S>,
    skip_paths: &'static [&'static str],
) -> Router<S> {
    router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| async move {
            if skip_paths.contains(&request.uri().path()) {
                return next.run(request).await;
            }
            record_request_metrics(request, next).await
        },
    ))
}

/// The middleware behind [`request_metrics`], for
/// [`axum::middleware::from_fn`]: times the request and records it.
#[cfg(feature = "telemetry")]
pub async fn record_request_metrics(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    crate::telemetry::record_http_request(response.status().as_u16(), started.elapsed());
    response
}

#[cfg(test)]
pub(crate) mod test_util {
    use axum::body::Body;
    use axum::http::{Request, Response};
    use axum::Router;
    use tower::ServiceExt;

    /// Sends one request through `router`.
    pub async fn call(router: Router, request: Request<Body>) -> Response<Body> {
        router.oneshot(request).await.unwrap()
    }

    pub fn get(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    pub async fn body_text(response: Response<Body>) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::{body_text, call, get};
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get as get_route;

    #[test]
    fn test_listen_addr() {
        // Variables unique to this test, as tests run in parallel.
        std::env::remove_var("WIKIMISC_TEST_LA_ADDRESS");
        std::env::remove_var("WIKIMISC_TEST_LA_PORT");
        assert_eq!(
            listen_addr("WIKIMISC_TEST_LA", 8000),
            "0.0.0.0:8000".parse().unwrap()
        );
        std::env::set_var("WIKIMISC_TEST_LA_ADDRESS", "::1");
        std::env::set_var("WIKIMISC_TEST_LA_PORT", " 9000 ");
        assert_eq!(
            listen_addr("WIKIMISC_TEST_LA", 8000),
            "[::1]:9000".parse().unwrap()
        );
        std::env::set_var("WIKIMISC_TEST_LA_ADDRESS", "not an address");
        std::env::set_var("WIKIMISC_TEST_LA_PORT", "99999");
        assert_eq!(
            listen_addr("WIKIMISC_TEST_LA", 8000),
            "0.0.0.0:8000".parse().unwrap()
        );
    }

    #[test]
    fn test_is_wikimedia_origin() {
        for origin in [
            "https://www.wikidata.org",
            "https://wikidata.org",
            "https://de.wikipedia.org",
            "https://de.m.wikipedia.org",
            "https://commons.wikimedia.org",
            "https://mix-n-match.toolforge.org",
            "https://petscan.wmcloud.org",
            "https://WWW.WIKIDATA.ORG",
            "https://www.wikidata.org:443",
        ] {
            assert!(is_wikimedia_origin(origin, false), "{origin}");
        }
        for origin in [
            "http://www.wikidata.org",
            "https://evilwikidata.org",
            "https://wikidata.org.evil.com",
            "https://www.wikidata.org.evil.com:443",
            "https://example.org",
            "wikidata.org",
            "https://localhost:8000",
            "null",
        ] {
            assert!(!is_wikimedia_origin(origin, false), "{origin}");
        }
        for origin in [
            "http://localhost:8000",
            "https://localhost",
            "http://127.0.0.1:3000",
            "http://[::1]:8000",
            "http://[::1]",
        ] {
            assert!(is_wikimedia_origin(origin, true), "{origin}");
            assert!(!is_wikimedia_origin(origin, false), "{origin}");
        }
        assert!(!is_wikimedia_origin("http://localhost.evil.com", true));
        assert!(!is_wikimedia_origin("ftp://localhost", true));
    }

    fn app() -> Router {
        Router::new()
            .route("/", get_route(|| async { "hello" }))
            .route(
                "/panic",
                get_route(|| async {
                    if true {
                        panic!("intentional handler panic (expected in test output)");
                    }
                    "unreachable"
                }),
            )
            .route(
                "/slow",
                get_route(|| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "slow"
                }),
            )
    }

    #[tokio::test]
    async fn test_standard_layers_turn_panics_into_500() {
        let router = standard_layers(app());
        assert_eq!(
            body_text(call(router.clone(), get("/")).await).await,
            "hello"
        );
        let response = call(router, get("/panic")).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_timeout_answers_with_the_given_status() {
        let router = timeout(
            app(),
            Duration::from_millis(10),
            StatusCode::GATEWAY_TIMEOUT,
        );
        let response = call(router, get("/slow")).await;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    fn preflight(origin: &str) -> Request<Body> {
        Request::options("/")
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn test_cors_wikimedia() {
        let config = WikimediaCors {
            credentials: true,
            allow_localhost: false,
        };
        let router = cors_wikimedia(app(), config);
        let response = call(router.clone(), preflight("https://www.wikidata.org")).await;
        let headers = response.headers();
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://www.wikidata.org"
        );
        assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
        let allowed = headers[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(allowed.contains("api-user-agent"), "{allowed}");

        let response = call(router, preflight("https://example.org")).await;
        assert!(!response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[tokio::test]
    async fn test_cors_any() {
        let response = call(cors_any(app()), preflight("https://example.org")).await;
        assert!(response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[cfg(feature = "telemetry")]
    #[tokio::test]
    async fn test_request_metrics_pass_responses_through() {
        // No recorder is installed here, so this checks the plumbing only;
        // what gets recorded is `record_http_request`'s business.
        let router = request_metrics(standard_layers(app()), &["/healthz"]);
        assert_eq!(
            body_text(call(router.clone(), get("/")).await).await,
            "hello"
        );
        let response = call(router, get("/panic")).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_serve_answers_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, app()));
        let response = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        })
        .await
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("hello"), "{response}");
        server.abort();
    }
}
