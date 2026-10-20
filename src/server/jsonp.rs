//! JSONP: JSON wrapped in a call of a function the client names, for
//! clients (old user scripts and gadgets) that load data with a `<script>`
//! tag.
//!
//! The callback name ends up verbatim in a script the browser runs, so it
//! must be checked: [`is_valid_callback`] admits JavaScript identifiers only
//! (optionally dotted paths). [`Jsonp`] does the check itself and answers
//! 400 to an invalid name. The name checks live in [`crate::jsonp`] (feature
//! `jsonp`, no axum) and are re-exported here.
//!
//! ```no_run
//! use wikimisc::server::jsonp::Jsonp;
//!
//! # #[derive(serde::Serialize)] struct Data;
//! async fn handler(callback: Option<String>) -> Jsonp<Data> {
//!     Jsonp::new(Data, callback).cache_for(300)
//! }
//! ```

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

pub use crate::jsonp::{is_valid_callback, wrap, CallbackRules, CONTENT_TYPE};

/// A response that is JSON, or JSONP when a callback is given.
///
/// An invalid callback name (see [`Self::rules`]) gets a 400 instead.
#[derive(Debug, Clone)]
pub struct Jsonp<T> {
    value: T,
    callback: Option<String>,
    rules: CallbackRules,
    cache_seconds: Option<u32>,
}

impl<T> Jsonp<T> {
    /// `value` as JSON, wrapped in `callback` if there is one. An empty
    /// callback counts as none.
    pub fn new(value: T, callback: Option<String>) -> Self {
        Self {
            value,
            callback: callback.filter(|c| !c.is_empty()),
            rules: CallbackRules::default(),
            cache_seconds: None,
        }
    }

    /// The callback names admitted; [`CallbackRules::default`] unless set.
    pub fn rules(mut self, rules: CallbackRules) -> Self {
        self.rules = rules;
        self
    }

    /// Adds `Cache-Control: public, max-age=<seconds>`.
    pub fn cache_for(mut self, seconds: u32) -> Self {
        self.cache_seconds = Some(seconds);
        self
    }
}

impl<T: Serialize> IntoResponse for Jsonp<T> {
    fn into_response(self) -> Response {
        if let Some(callback) = &self.callback {
            if !is_valid_callback(callback, self.rules) {
                return (StatusCode::BAD_REQUEST, "invalid callback name").into_response();
            }
        }
        let json = match serde_json::to_string(&self.value) {
            Ok(json) => json,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        let (content_type, body) = match &self.callback {
            Some(callback) => (CONTENT_TYPE, wrap(callback, &json)),
            None => ("application/json", json),
        };
        let mut response = ([(header::CONTENT_TYPE, content_type)], body).into_response();
        if let Some(seconds) = self.cache_seconds {
            if let Ok(value) = HeaderValue::from_str(&format!("public, max-age={seconds}")) {
                response.headers_mut().insert(header::CACHE_CONTROL, value);
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn parts(response: Response) -> (StatusCode, Option<String>, Option<String>, String) {
        let header = |name| {
            response
                .headers()
                .get(name)
                .map(|v: &HeaderValue| v.to_str().unwrap().to_string())
        };
        let (status, content_type, cache) = (
            response.status(),
            header(header::CONTENT_TYPE),
            header(header::CACHE_CONTROL),
        );
        let body = crate::server::test_util::body_text(response).await;
        (status, content_type, cache, body)
    }

    #[tokio::test]
    async fn test_json_without_callback() {
        let (status, content_type, cache, body) =
            parts(Jsonp::new(json!({"a": 1}), Some(String::new())).into_response()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("application/json"));
        assert_eq!(cache, None);
        assert_eq!(body, r#"{"a":1}"#);
    }

    #[tokio::test]
    async fn test_jsonp_with_callback() {
        let response = Jsonp::new(json!([1]), Some("cb".to_string()))
            .cache_for(60)
            .into_response();
        let (status, content_type, cache, body) = parts(response).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some(CONTENT_TYPE));
        assert_eq!(cache.as_deref(), Some("public, max-age=60"));
        assert_eq!(body, "cb([1]);");
    }

    #[tokio::test]
    async fn test_invalid_callback_is_refused() {
        let response = Jsonp::new(json!(1), Some("alert(document.cookie)".to_string()));
        let (status, _, _, body) = parts(response.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!body.contains("alert"));

        let dotted = Jsonp::new(json!(1), Some("a.b".to_string())).rules(CallbackRules {
            allow_dotted: true,
            max_len: 64,
        });
        assert_eq!(dotted.into_response().status(), StatusCode::OK);
    }
}
