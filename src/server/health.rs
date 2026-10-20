//! Health check endpoints.
//!
//! ```no_run
//! use axum::{routing::get, Router};
//! use wikimisc::server::health;
//!
//! # fn example(index_loaded: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Router {
//! Router::new()
//!     .route("/healthz", get(health::ok))
//!     .route(
//!         "/readyz",
//!         health::readiness(
//!             move || index_loaded.load(std::sync::atomic::Ordering::Relaxed),
//!             "no index loaded",
//!         ),
//!     )
//! # }
//! ```

use axum::http::StatusCode;
use axum::routing::{get, MethodRouter};

/// Liveness: answers 200 `ok` as long as the service answers at all.
pub async fn ok() -> &'static str {
    "ok"
}

/// Readiness: a GET route answering 200 `ok` while `is_ready` returns true,
/// and 503 with `not_ready` otherwise (e.g. while data is still loading).
pub fn readiness<S, F>(is_ready: F, not_ready: &'static str) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
    F: Fn() -> bool + Clone + Send + Sync + 'static,
{
    get(move || {
        let ready = is_ready();
        async move {
            if ready {
                (StatusCode::OK, "ok")
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, not_ready)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{body_text, call, get as request};
    use super::*;
    use axum::Router;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_ok() {
        let router = Router::new().route("/healthz", get(ok));
        let response = call(router, request("/healthz")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, "ok");
    }

    #[tokio::test]
    async fn test_readiness() {
        let ready = Arc::new(AtomicBool::new(false));
        let flag = ready.clone();
        let router = Router::new().route(
            "/readyz",
            readiness(move || flag.load(Ordering::Relaxed), "still loading"),
        );

        let response = call(router.clone(), request("/readyz")).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_text(response).await, "still loading");

        ready.store(true, Ordering::Relaxed);
        let response = call(router, request("/readyz")).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
