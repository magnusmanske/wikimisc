//! Test-only utilities for spinning up an in-process [`wiremock`] server that
//! impersonates the Wikidata Action API and SPARQL endpoint, plus a [`Wikidata`]
//! client preconfigured to talk to it.
//!
//! Lets the suite cover HTTP-driven paths (`SiteMatrix::new`, `Wikidata::api`,
//! `Wikidata::load_sparql_csv`, `ExternalId::*`) deterministically and offline.

use crate::wikidata::Wikidata;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const API_PATH: &str = "/w/api.php";
pub const SPARQL_PATH: &str = "/sparql";

/// Build a [`Wikidata`] whose API and SPARQL endpoints point at `server`.
pub fn wikidata_for(server: &MockServer) -> Wikidata {
    let mut wd = Wikidata::new();
    wd.set_api_url(&format!("{}{}", server.uri(), API_PATH));
    wd.set_sparql_url(&format!("{}{}", server.uri(), SPARQL_PATH));
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

/// Mount a CSV response for the SPARQL endpoint.
pub async fn mount_sparql_csv(server: &MockServer, csv: &str) {
    Mock::given(method("GET"))
        .and(path(SPARQL_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(csv.to_string())
                .insert_header("content-type", "text/csv"),
        )
        .mount(server)
        .await;
}
