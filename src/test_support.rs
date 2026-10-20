//! Test-only utilities for spinning up an in-process [`wiremock`] server that
//! impersonates the Wikidata Action API, plus a [`Wikidata`] client
//! preconfigured to talk to it.
//!
//! Lets the suite cover HTTP-driven paths (`SiteMatrix::new`, `Wikidata::api`,
//! `ExternalId::*`) deterministically and offline.
//!
//! Which helpers are actually reachable depends on the enabled features, so the
//! module tolerates unused ones rather than gating each on a feature list that
//! would need updating whenever a test moves.
#![allow(dead_code)]

use serde_json::{json, Value};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const API_PATH: &str = "/w/api.php";

/// The user agent the test clients send.
pub const TEST_USER_AGENT: &str = "wikimisc-tests/0 (https://github.com/magnusmanske/wikimisc)";

/// Build a [`crate::wikidata::Wikidata`] whose API endpoint points at `server`.
#[cfg(feature = "wikidata")]
pub fn wikidata_for(server: &MockServer) -> crate::wikidata::Wikidata {
    use crate::wikidata::Wikidata;

    let mut wd = Wikidata::new(TEST_USER_AGENT);
    wd.set_api_url(&format!("{}{}", server.uri(), API_PATH));
    wd
}

/// Returns the api.php URL hosted by `server`.
pub fn api_url(server: &MockServer) -> String {
    format!("{}{}", server.uri(), API_PATH)
}

/// Mount the minimal `action=query&meta=siteinfo` response that
/// [`mediawiki::Api::new`] requires during construction.
pub async fn mount_siteinfo(server: &MockServer) {
    let body = json!({
        "batchcomplete": "",
        "query": {
            "general": { "sitename": "Wikidata", "lang": "en" },
            "namespaces": {
                "0": { "id": 0, "case": "first-letter", "*": "", "canonical": "" }
            },
            "namespacealiases": [],
            "libraries": [],
            "extensions": [],
            "statistics": {}
        }
    });
    Mock::given(method("GET"))
        .and(path(API_PATH))
        .and(query_param("meta", "siteinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Mount a JSON response for a specific `action=…` Action API request.
pub async fn mount_action(server: &MockServer, action: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(API_PATH))
        .and(query_param("action", action))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Mount a JSON response for a `action=query&list=…` Action API request.
pub async fn mount_list(server: &MockServer, list: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(API_PATH))
        .and(query_param("list", list))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Mount a JSON response for Action API POST requests whose form body
/// contains `needle` (e.g. `action=wbgetentities`, or a form-encoded
/// `srsearch=…`).
pub async fn mount_post(server: &MockServer, needle: &str, body: Value) {
    Mock::given(method("POST"))
        .and(path(API_PATH))
        .and(body_string_contains(needle))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}
