//! Convenience wrapper for talking to Wikidata.
//!
//! [`Wikidata`] holds the caller's user-agent string and a request timeout,
//! and constructs `reqwest::Client` / `mediawiki::Api` instances with both
//! applied consistently. New HTTP calls against Wikidata should go through
//! this struct rather than building a `reqwest::Client` directly, so that the
//! tool's UA and timeout policy stay uniform. SPARQL queries go through
//! `sparql_client::SparqlClient` (feature `sparql-client`).
//!
//! Besides [`Wikidata::api`], it offers the read-only calls tools keep
//! reimplementing, over plain HTTP (no site-info round trip per call):
//! batched entity loading ([`Wikidata::get_entities`]), item lookup by
//! sitelink ([`Wikidata::items_for_titles`]) and CirrusSearch
//! ([`Wikidata::search`], with queries built by [`CirrusQuery`]).
//!
//! Also provides [`Wikidata::item2qs`] for converting an `ItemEntity` into a
//! QuickStatements-compatible line list.

use crate::entity_id::{EntityId, ItemId, PropertyId};
use futures::stream::{self, StreamExt, TryStreamExt};
use reqwest::{Client, ClientBuilder};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use thiserror::Error;
use wikibase::mediawiki::media_wiki_error::MediaWikiError;
use wikibase::{
    mediawiki::Api, Coordinate, EntityTrait, ItemEntity, QuantityValue, SnakType, TimeValue, Value,
};

/// Failure modes of [`Wikidata`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WikidataError {
    /// [`Wikidata::item2qs`] was handed an item that already exists on Wikidata.
    /// QuickStatements can only create new items, so the ID must be empty.
    #[error("item2qs: item ID is not empty")]
    ItemIdNotEmpty,

    /// The HTTP client could not be built, or a request failed.
    #[error(transparent)]
    Http(#[from] reqwest::Error),

    /// Constructing the MediaWiki API client failed.
    #[error(transparent)]
    MediaWiki(#[from] MediaWikiError),

    /// The API answered with an error object.
    #[error("Wikidata API error '{code}': {info}")]
    Api { code: String, info: String },
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_API_URL: &str = "https://www.wikidata.org/w/api.php";
const DEFAULT_CONCURRENCY: usize = 4;

/// `wbgetentities` takes at most 50 IDs or titles per request (for clients
/// without the `apihighlimits` right).
const ENTITIES_PER_REQUEST: usize = 50;

#[derive(Debug, Clone)]
pub struct Wikidata {
    user_agent: String,
    timeout: Duration,
    api_url: String,
    concurrency: usize,
    /// Built on first use by the plain-HTTP calls (or given to
    /// [`Self::set_client`]) and shared between clones. The user agent and
    /// timeout are set per request, so the client never goes stale.
    client: Arc<OnceLock<Client>>,
}

impl Wikidata {
    /// A client that identifies itself as `user_agent` — Wikimedia's
    /// [user-agent policy](https://meta.wikimedia.org/wiki/User-Agent_policy)
    /// asks for the tool's name and a way to contact its maintainer, e.g.
    /// `wikimisc::user_agent!()` (feature `http`).
    pub fn new(user_agent: impl Into<String>) -> Self {
        Wikidata {
            user_agent: user_agent.into(),
            timeout: DEFAULT_TIMEOUT,
            api_url: DEFAULT_API_URL.to_string(),
            concurrency: DEFAULT_CONCURRENCY,
            client: Arc::default(),
        }
    }

    /// Like [`Self::new`], but the plain-HTTP calls use `client` — e.g. one
    /// the tool already shares, so that it keeps a single connection pool.
    /// Requests still send `user_agent` and the configured timeout.
    pub fn with_client(user_agent: impl Into<String>, client: Client) -> Self {
        let mut wikidata = Self::new(user_agent);
        wikidata.set_client(client);
        wikidata
    }

    /// Makes the plain-HTTP calls use `client`; see [`Self::with_client`].
    /// Clones made earlier keep the client they had.
    pub fn set_client(&mut self, client: Client) {
        self.client = Arc::new(OnceLock::from(client));
    }

    /// A fresh `mediawiki::Api` for the configured endpoint, sending the
    /// configured user agent. Each call fetches the site info again; keep the
    /// `Api` around when making several calls.
    ///
    /// (The site-info request made while constructing the `Api` still carries
    /// the `mediawiki` crate's default user agent: `Api` only lets the agent be
    /// set once it exists.)
    pub async fn api(&self) -> Result<Api, WikidataError> {
        let mut api = Api::new_from_builder(&self.api_url, self.client_builder()).await?;
        // `Api` sends its own `User-Agent` header with every request, which
        // overrides the one the client builder sets.
        api.set_user_agent(&self.user_agent);
        Ok(api)
    }

    /// Returns a reqwest client with the current user agent and timeout.
    pub fn reqwest_client(&self) -> Result<Client, WikidataError> {
        Ok(self.client_builder().build()?)
    }

    /// A client builder with the configured user agent and timeout, a 10 s
    /// connect timeout and gzip compression.
    pub fn client_builder(&self) -> ClientBuilder {
        Client::builder()
            .user_agent(&self.user_agent)
            .timeout(self.timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .gzip(true)
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub fn set_user_agent(&mut self, user_agent: &str) {
        self.user_agent = user_agent.to_string();
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Overrides the MediaWiki Action API endpoint used by [`Self::api`] and
    /// the plain-HTTP calls. Default: `https://www.wikidata.org/w/api.php`.
    /// Primarily a test seam for pointing at a local mock server.
    pub fn set_api_url(&mut self, api_url: &str) {
        self.api_url = api_url.to_string();
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// How many batch requests [`Self::get_entities`] and
    /// [`Self::items_for_titles`] keep in flight at once (default 4, at
    /// least 1).
    pub fn set_concurrency(&mut self, concurrency: usize) {
        self.concurrency = concurrency.max(1);
    }

    /// Converts a new (ID-less) item into QuickStatements V1 commands: a
    /// `CREATE` line followed by one `LAST` line per term, sitelink and
    /// statement (qualifiers and references on the statement's line).
    ///
    /// The values follow the syntax the QuickStatements V1 parser accepts.
    /// Anything that parser would reject or silently change is left out rather
    /// than emitted: novalue/somevalue snaks, entity schemas, non-Earth or
    /// altitude-carrying coordinates, quantities with a non-Wikidata unit or
    /// bounds that are not a symmetric tolerance, times in a calendar other
    /// than Gregorian or Julian or with a timezone or before/after, and text
    /// containing a tab, line break, `||` or `/*` (column, line, command and
    /// comment separators to QuickStatements). A skipped main value drops the
    /// whole statement; a skipped qualifier or reference snak drops only that
    /// snak. Coordinate precision cannot be expressed: QuickStatements applies
    /// its own.
    pub fn item2qs(item: &ItemEntity) -> Result<Vec<String>, WikidataError> {
        if !item.id().is_empty() {
            return Err(WikidataError::ItemIdNotEmpty);
        }

        let mut ret = vec!["CREATE".to_string()];

        let terms = [
            ('L', item.labels()),
            ('D', item.descriptions()),
            ('A', item.aliases()),
        ];
        for (command, terms) in terms {
            ret.extend(
                terms
                    .iter()
                    .filter_map(|ls| Self::term2qs(command, ls.language(), ls.value())),
            );
        }

        ret.extend(
            item.sitelinks()
                .as_ref()
                .map(|sl| sl.as_slice())
                .unwrap_or(&[])
                .iter()
                .filter_map(|sl| Self::term2qs('S', sl.site(), sl.title())),
        );

        // Add statements
        for statement in item.claims() {
            let main_value = match Self::snak2qs(statement.main_snak()) {
                Some(value) => value,
                None => continue,
            };

            let qualifiers: Vec<String> = statement
                .qualifiers()
                .iter()
                .filter_map(|snak| {
                    let v = Self::snak2qs(snak)?;
                    Some(vec![snak.property().to_string(), v])
                })
                .flatten()
                .collect();

            let references: Vec<String> = statement
                .references()
                .iter()
                .flat_map(|r| {
                    let mut parts: Vec<String> = r
                        .snaks()
                        .iter()
                        .filter_map(|snak| {
                            let v = Self::snak2qs(snak)?;
                            let p = snak.property().replace('P', "S");
                            Some(vec![p, v])
                        })
                        .flatten()
                        .collect();
                    if !parts.is_empty() {
                        parts[0] = format!("!{}", parts[0]); // Start new reference section
                    }
                    parts
                })
                .collect();

            let mut parts = vec![main_value];
            parts.extend(qualifiers);
            parts.extend(references);
            ret.push(format!(
                "LAST\t{}\t{}",
                statement.property(),
                parts.join("\t")
            ));
        }
        Ok(ret)
    }

    /// A label (`L`), description (`D`), alias (`A`) or sitelink (`S`) line.
    fn term2qs(command: char, code: &str, text: &str) -> Option<String> {
        if !is_qs_code(code) || !is_qs_text(text) {
            return None;
        }
        Some(format!("LAST\t{command}{code}\t\"{text}\""))
    }

    fn snak2qs(snak: &wikibase::Snak) -> Option<String> {
        if *snak.snak_type() != SnakType::Value {
            return None;
        }
        match snak.data_value().as_ref()?.value() {
            Value::Coordinate(c) => Self::coordinate2qs(c),
            Value::MonoLingual(m) => (is_qs_code(m.language()) && is_qs_text(m.text()))
                .then(|| format!("{}:\"{}\"", m.language(), m.text())),
            // Bare: a quoted ID is a string to QuickStatements.
            Value::Entity(entity) => is_qs_entity_id(entity.id()).then(|| entity.id().to_string()),
            Value::Quantity(quantity) => Self::quantity2qs(quantity),
            Value::StringValue(s) => is_qs_text(s).then(|| format!("\"{s}\"")),
            Value::Time(time) => Self::time2qs(time),
            _ => None,
        }
    }

    /// `@<lat>/<lon>`. QuickStatements places every coordinate on Earth, with
    /// no altitude and a precision of its own choosing.
    fn coordinate2qs(c: &Coordinate) -> Option<String> {
        let (latitude, longitude) = (*c.latitude(), *c.longitude());
        (c.globe() == GLOBE_EARTH
            && c.altitude().is_none()
            && latitude.is_finite()
            && longitude.is_finite())
        .then(|| format!("@{latitude}/{longitude}"))
    }

    /// `<amount>[~<tolerance>][U<unit item number>]`. QuickStatements knows
    /// only a symmetric tolerance, from which it recomputes both bounds, so
    /// bounds are emitted only when that reproduces them exactly.
    fn quantity2qs(quantity: &QuantityValue) -> Option<String> {
        let amount = *quantity.amount();
        if !amount.is_finite() {
            return None;
        }
        let tolerance = match (*quantity.lower_bound(), *quantity.upper_bound()) {
            (None, None) => String::new(),
            (Some(lower), Some(upper)) => {
                let tolerance = upper - amount;
                let exact = tolerance.is_finite()
                    && tolerance >= 0.0
                    && amount - tolerance == lower
                    && amount + tolerance == upper;
                if !exact {
                    return None;
                }
                format!("~{tolerance}")
            }
            _ => return None,
        };
        let unit = match quantity.unit() {
            UNITLESS => String::new(),
            unit => {
                let number = unit.strip_prefix(ENTITY_URI_ITEM_PREFIX)?;
                if !is_number(number) {
                    return None;
                }
                format!("U{number}")
            }
        };
        Some(format!("{amount}{tolerance}{unit}"))
    }

    /// `<time>/<precision>[/J]`. QuickStatements sets timezone, before and
    /// after to 0 and knows only the Gregorian and (`/J`) Julian calendars.
    fn time2qs(time: &TimeValue) -> Option<String> {
        if *time.timezone() != 0 || *time.before() != 0 || *time.after() != 0 {
            return None;
        }
        if !is_qs_time(time.time()) {
            return None;
        }
        let calendar = match time.calendarmodel() {
            CALENDAR_GREGORIAN => "",
            CALENDAR_JULIAN => "/J",
            _ => return None,
        };
        Some(format!("{}/{}{calendar}", time.time(), time.precision()))
    }
}

/// Read-only Action API calls over plain HTTP.
impl Wikidata {
    /// The shared HTTP client, built on first use.
    fn http_client(&self) -> Result<Client, WikidataError> {
        if let Some(client) = self.client.get() {
            return Ok(client.clone());
        }
        let client = self.reqwest_client()?;
        // A concurrent first call may have won the race; either client works.
        Ok(self.client.get_or_init(|| client).clone())
    }

    /// POSTs an Action API request (read-only modules accept POST, and it
    /// keeps long title lists out of the URL) and returns the JSON answer.
    async fn api_post(&self, params: &[(&str, &str)]) -> Result<JsonValue, WikidataError> {
        let mut form: Vec<(&str, &str)> = vec![("format", "json")];
        form.extend_from_slice(params);
        let json: JsonValue = self
            .http_client()?
            .post(&self.api_url)
            // Per request, so that a client from `set_client` and changes made
            // by the setters after the client was built both take effect.
            .header(reqwest::header::USER_AGENT, &self.user_agent)
            .timeout(self.timeout)
            .form(&form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        api_error(&json).map_or(Ok(json), Err)
    }

    /// Loads entities with `wbgetentities`, 50 per request, up to
    /// [`Self::set_concurrency`] requests at a time. Duplicate IDs are fetched
    /// once.
    ///
    /// `props` and `languages` restrict what is loaded (`["labels",
    /// "claims"]`, `["en", "mul"]`); empty slices load everything. Entities
    /// that do not exist are left out of the result. A redirected ID is keyed
    /// by the ID asked for, with the target's JSON as the value. Any failed
    /// request fails the whole call.
    pub async fn get_entities(
        &self,
        ids: &[EntityId],
        props: &[&str],
        languages: &[&str],
    ) -> Result<HashMap<EntityId, JsonValue>, WikidataError> {
        let props = props.join("|");
        let languages = languages.join("|");
        let params: Vec<(&str, &str)> =
            [("props", props.as_str()), ("languages", languages.as_str())]
                .into_iter()
                .filter(|(_, value)| !value.is_empty())
                .collect();
        self.get_entities_with(ids, &params).await
    }

    /// Like [`Self::get_entities`], with any `wbgetentities` parameters given
    /// as name/value pairs: `props`, `languages`, `sitefilter`,
    /// `languagefallback`, …. `action`, `format` and `ids` are set by this
    /// method; pairs with those names are ignored.
    ///
    /// ```rust,no_run
    /// # async fn f(wd: &wikimisc::wikidata::Wikidata, ids: &[wikimisc::entity_id::EntityId])
    /// # -> Result<(), wikimisc::wikidata::WikidataError> {
    /// let entities = wd
    ///     .get_entities_with(ids, &[("props", "sitelinks"), ("sitefilter", "enwiki|dewiki")])
    ///     .await?;
    /// # Ok(()) }
    /// ```
    pub async fn get_entities_with(
        &self,
        ids: &[EntityId],
        params: &[(&str, &str)],
    ) -> Result<HashMap<EntityId, JsonValue>, WikidataError> {
        let mut seen = HashSet::new();
        let unique: Vec<String> = ids
            .iter()
            .filter(|id| seen.insert(**id))
            .map(ToString::to_string)
            .collect();
        let params: Vec<(&str, &str)> = params
            .iter()
            .copied()
            .filter(|(name, _)| !matches!(*name, "action" | "format" | "ids"))
            .collect();
        let params = &params;

        // Owned batches: a closure over borrowed chunks would make the
        // returned future not `Send`.
        let batches: Vec<String> = unique
            .chunks(ENTITIES_PER_REQUEST)
            .map(|chunk| chunk.join("|"))
            .collect();
        let results: Vec<HashMap<EntityId, JsonValue>> = stream::iter(batches)
            .map(|ids| async move {
                let mut request = vec![("action", "wbgetentities"), ("ids", ids.as_str())];
                request.extend_from_slice(params);
                let json = self.api_post(&request).await?;
                Ok::<_, WikidataError>(entities_by_requested_id(json))
            })
            .buffer_unordered(self.concurrency)
            .try_collect()
            .await?;
        Ok(results.into_iter().flatten().collect())
    }

    /// The items with a sitelink to each of `titles` on `site` (`enwiki`,
    /// `commonswiki`, …), keyed by the title as given. Titles without an item
    /// (or without a page) are left out rather than failing the call.
    /// Underscores and a lowercase first letter in a title are tolerated.
    pub async fn items_for_titles(
        &self,
        site: &str,
        titles: &[&str],
    ) -> Result<HashMap<String, ItemId>, WikidataError> {
        // Owned batches: a closure over borrowed chunks would make the
        // returned future not `Send`.
        let batches: Vec<Vec<String>> = titles
            .chunks(ENTITIES_PER_REQUEST)
            .map(|chunk| chunk.iter().map(ToString::to_string).collect())
            .collect();
        let results: Vec<HashMap<String, ItemId>> = stream::iter(batches)
            .map(|batch| async move {
                let joined = batch.join("|");
                let params = [
                    ("action", "wbgetentities"),
                    ("sites", site),
                    ("titles", joined.as_str()),
                    ("props", "sitelinks"),
                    ("sitefilter", site),
                ];
                let json = self.api_post(&params).await?;
                Ok::<_, WikidataError>(items_by_title(&json, site, &batch))
            })
            .buffer_unordered(self.concurrency)
            .try_collect()
            .await?;
        Ok(results.into_iter().flatten().collect())
    }

    /// Full-text search (`list=search`, i.e. CirrusSearch) in one namespace:
    /// 0 for items, 120 for properties, 146 for lexemes. `query` may use
    /// CirrusSearch keywords; [`CirrusQuery`] builds such queries.
    pub async fn search(
        &self,
        query: &str,
        namespace: i64,
        limit: usize,
    ) -> Result<SearchResults, WikidataError> {
        let namespace = namespace.to_string();
        let limit = limit.max(1).to_string();
        let params = [
            ("action", "query"),
            ("list", "search"),
            ("srsearch", query),
            ("srnamespace", namespace.as_str()),
            ("srlimit", limit.as_str()),
            ("srprop", ""),
        ];
        let json = self.api_post(&params).await?;
        Ok(SearchResults::from_json(&json))
    }

    /// The items matching `query`, best match first.
    pub async fn search_items(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ItemId>, WikidataError> {
        let results = self.search(query, 0, limit).await?;
        Ok(results.ids.iter().filter_map(|id| id.as_item()).collect())
    }

    /// The one item matching `query`; `None` if there is none or more than
    /// one (an ambiguous match is no match).
    pub async fn search_single_item(&self, query: &str) -> Result<Option<ItemId>, WikidataError> {
        let results = self.search(query, 0, 1).await?;
        if results.total_hits != 1 {
            return Ok(None);
        }
        Ok(results.ids.first().and_then(|id| id.as_item()))
    }
}

/// The error object of an Action API answer, if it has one.
fn api_error(json: &JsonValue) -> Option<WikidataError> {
    let error = json.get("error")?;
    let text = |key: &str| error[key].as_str().unwrap_or_default().to_string();
    Some(WikidataError::Api {
        code: text("code"),
        info: text("info"),
    })
}

/// The existing entities of a `wbgetentities` answer, keyed by the ID asked
/// for (which differs from the entity's own ID for a redirect).
fn entities_by_requested_id(json: JsonValue) -> HashMap<EntityId, JsonValue> {
    let JsonValue::Object(mut root) = json else {
        return HashMap::new();
    };
    let Some(JsonValue::Object(entities)) = root.remove("entities") else {
        return HashMap::new();
    };
    entities
        .into_iter()
        .filter(|(_, entity)| entity.get("missing").is_none())
        .filter_map(|(key, entity)| {
            let requested = entity["redirects"]["from"].as_str().unwrap_or(&key);
            Some((requested.parse().ok()?, entity))
        })
        .collect()
}

/// Maps each of `titles` to the item whose `site` sitelink matches it.
fn items_by_title(json: &JsonValue, site: &str, titles: &[String]) -> HashMap<String, ItemId> {
    let Some(entities) = json["entities"].as_object() else {
        return HashMap::new();
    };
    let linked: Vec<(&str, ItemId)> = entities
        .values()
        .filter_map(|entity| {
            let title = entity["sitelinks"][site]["title"].as_str()?;
            let item = entity["id"].as_str()?.parse().ok()?;
            Some((title, item))
        })
        .collect();
    titles
        .iter()
        .filter_map(|title| {
            let (_, item) = linked
                .iter()
                .find(|(linked, _)| same_page_title(linked, title))?;
            Some((title.clone(), *item))
        })
        .collect()
}

/// Whether two titles name the same page on a wiki with first-letter case
/// insensitivity: underscores are spaces, and the first letter's case does
/// not matter.
fn same_page_title(a: &str, b: &str) -> bool {
    fn normalized(title: &str) -> String {
        let title = title.replace('_', " ");
        let title = title.trim();
        let mut chars = title.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect()
        })
    }
    normalized(a) == normalized(b)
}

/// The answer to [`Wikidata::search`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SearchResults {
    /// How many pages match in total, beyond the ones returned.
    pub total_hits: u64,
    /// The matching entities, best match first. Pages whose title is not an
    /// entity ID are left out.
    pub ids: Vec<EntityId>,
}

impl SearchResults {
    fn from_json(json: &JsonValue) -> Self {
        let ids = json["query"]["search"]
            .as_array()
            .map(|hits| {
                hits.iter()
                    .filter_map(|hit| entity_id_from_title(hit["title"].as_str()?))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            total_hits: json["query"]["searchinfo"]["totalhits"]
                .as_u64()
                .unwrap_or_default(),
            ids,
        }
    }
}

/// The entity of a Wikidata page title: `Q42`, `Property:P31`, `Lexeme:L7`.
fn entity_id_from_title(title: &str) -> Option<EntityId> {
    let id = title.rsplit_once(':').map_or(title, |(_namespace, id)| id);
    id.parse().ok()
}

/// A CirrusSearch query: free text plus `haswbstatement`/`inlabel` keywords.
///
/// ```
/// # use wikimisc::wikidata::CirrusQuery;
/// # use wikimisc::entity_id::PropertyId;
/// let query = CirrusQuery::new("Douglas Adams")
///     .has_statement(PropertyId::new(31), "Q5")
///     .not_has_statement(PropertyId::new(31), "Q13442814");
/// assert_eq!(
///     query.to_string(),
///     r#"Douglas Adams haswbstatement:"P31=Q5" -haswbstatement:"P31=Q13442814""#
/// );
/// ```
///
/// Keyword values are always quoted, so values with spaces (an external ID
/// like `n 79021164`) stay one value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CirrusQuery {
    parts: Vec<String>,
}

impl CirrusQuery {
    /// A query for `text`, which may be empty.
    pub fn new(text: &str) -> Self {
        Self::default().text(text)
    }

    /// Adds free text.
    #[must_use]
    pub fn text(mut self, text: &str) -> Self {
        let text = text.trim();
        if !text.is_empty() {
            self.parts.push(text.to_string());
        }
        self
    }

    /// Only entities with a `property` statement whose value is `value`
    /// (an item ID, or an external ID / string value).
    #[must_use]
    pub fn has_statement(self, property: PropertyId, value: impl fmt::Display) -> Self {
        self.keyword("haswbstatement:", &format!("{property}={value}"))
    }

    /// Only entities without such a statement.
    #[must_use]
    pub fn not_has_statement(self, property: PropertyId, value: impl fmt::Display) -> Self {
        self.keyword("-haswbstatement:", &format!("{property}={value}"))
    }

    /// Only entities with any `property` statement.
    #[must_use]
    pub fn has_property(mut self, property: PropertyId) -> Self {
        self.parts.push(format!("haswbstatement:{property}"));
        self
    }

    /// Only entities without a `property` statement.
    #[must_use]
    pub fn not_has_property(mut self, property: PropertyId) -> Self {
        self.parts.push(format!("-haswbstatement:{property}"));
        self
    }

    /// Only entities with `text` in a label or alias, in any language or (if
    /// `languages` is not empty) in one of `languages`.
    #[must_use]
    pub fn in_label(self, text: &str, languages: &[&str]) -> Self {
        let value = if languages.is_empty() {
            text.to_string()
        } else {
            format!("{text}@{}", languages.join(","))
        };
        self.keyword("inlabel:", &value)
    }

    fn keyword(mut self, keyword: &str, value: &str) -> Self {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        self.parts.push(format!("{keyword}\"{escaped}\""));
        self
    }
}

impl fmt::Display for CirrusQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.parts.join(" "))
    }
}

const ENTITY_URI_ITEM_PREFIX: &str = "http://www.wikidata.org/entity/Q";
const GLOBE_EARTH: &str = "http://www.wikidata.org/entity/Q2";
const CALENDAR_GREGORIAN: &str = "http://www.wikidata.org/entity/Q1985727";
const CALENDAR_JULIAN: &str = "http://www.wikidata.org/entity/Q1985786";
const UNITLESS: &str = "1";

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Text QuickStatements reads back unchanged inside `"…"`. It needs no
/// escaping (a value runs to the last quote), but a tab or line break splits
/// the command, `||` is a command separator, and `/* … */` a comment.
fn is_qs_text(text: &str) -> bool {
    !text.contains(['\t', '\n', '\r']) && !text.contains("||") && !text.contains("/*")
}

/// A language code or site ID, as the QuickStatements parsers match it.
fn is_qs_code(code: &str) -> bool {
    code.starts_with(|c: char| c.is_ascii_lowercase())
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
}

/// An ID QuickStatements reads as an entity: `P`/`Q`/`L`/`M` + number, or a
/// lexeme form/sense such as `L1-F2`.
fn is_qs_entity_id(id: &str) -> bool {
    match id.split_once('-') {
        Some((lexeme, sub)) => {
            lexeme.strip_prefix('L').is_some_and(is_number)
                && sub.strip_prefix(['F', 'S']).is_some_and(is_number)
        }
        None => id.strip_prefix(['P', 'Q', 'L', 'M']).is_some_and(is_number),
    }
}

/// A Wikibase timestamp, `[+-]YYYY…-MM-DDThh:mm:ssZ`.
fn is_qs_time(time: &str) -> bool {
    let two_digits = |s: &&str| s.len() == 2 && is_number(s);
    let unsigned = time.strip_prefix(['+', '-']).unwrap_or(time);
    let Some((date, clock)) = unsigned.split_once('T') else {
        return false;
    };
    let Some(clock) = clock.strip_suffix('Z') else {
        return false;
    };
    let date: Vec<&str> = date.split('-').collect();
    let clock: Vec<&str> = clock.split(':').collect();
    matches!(date.as_slice(), [year, month, day] if is_number(year) && two_digits(month) && two_digits(day))
        && clock.len() == 3
        && clock.iter().all(two_digits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wikibase::*;

    use crate::test_support::TEST_USER_AGENT;

    #[test]
    fn test_item2qs_rejects_item_with_id() {
        let mut item = ItemEntity::new_empty();
        item.set_id("Q123".to_string());
        assert!(Wikidata::item2qs(&item).is_err());
    }

    #[test]
    fn test_item2qs_empty_item() {
        let item = ItemEntity::new_empty();
        let qs = Wikidata::item2qs(&item).unwrap();
        assert_eq!(qs, vec!["CREATE"]);
    }

    #[test]
    fn test_item2qs_labels_descriptions_aliases() {
        let mut item = ItemEntity::new_empty();
        item.labels_mut().push(LocaleString::new("en", "Test item"));
        item.labels_mut()
            .push(LocaleString::new("de", "Testelement"));
        item.descriptions_mut()
            .push(LocaleString::new("en", "a test"));
        item.aliases_mut().push(LocaleString::new("en", "alias1"));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"CREATE".to_string()));
        assert!(qs.contains(&"LAST\tLen\t\"Test item\"".to_string()));
        assert!(qs.contains(&"LAST\tLde\t\"Testelement\"".to_string()));
        assert!(qs.contains(&"LAST\tDen\t\"a test\"".to_string()));
        assert!(qs.contains(&"LAST\tAen\t\"alias1\"".to_string()));
    }

    #[test]
    fn test_item2qs_sitelinks() {
        let mut item = ItemEntity::new_empty();
        item.sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Test item", vec![]));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tSenwiki\t\"Test item\"".to_string()));
    }

    #[test]
    fn test_item2qs_string_statement_no_qualifiers_no_refs() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello world"),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tP1476\t\"hello world\"".to_string()));
    }

    #[test]
    fn test_item2qs_item_statement() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_item("P31", "Q5"),
            vec![],
            vec![],
        ));

        // A quoted ID would be read as a string, failing every item statement.
        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tP31\tQ5".to_string()), "got: {qs:?}");
    }

    /// The single statement line `item2qs` emits for `snak` as a main value,
    /// or `None` when it skips the statement.
    fn qs_line(snak: Snak) -> Option<String> {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(snak, vec![], vec![]));
        let mut qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.len() <= 2, "got: {qs:?}");
        qs.pop().filter(|line| line != "CREATE")
    }

    fn value_snak(datatype: SnakDataType, value: Value) -> Snak {
        let value_type = match &value {
            Value::Quantity(_) => DataValueType::Quantity,
            Value::Time(_) => DataValueType::Time,
            Value::Coordinate(_) => DataValueType::GlobeCoordinate,
            Value::MonoLingual(_) => DataValueType::MonoLingualText,
            Value::Entity(_) | Value::EntitySchema(_) => DataValueType::EntityId,
            Value::StringValue(_) => DataValueType::StringType,
        };
        Snak::new(
            datatype,
            "P1",
            SnakType::Value,
            Some(DataValue::new(value_type, value)),
        )
    }

    fn quantity(amount: f64, bounds: Option<(f64, f64)>, unit: &str) -> Option<String> {
        let (lower, upper) = bounds.unzip();
        let value = Value::Quantity(QuantityValue::new(amount, lower, unit, upper));
        qs_line(value_snak(SnakDataType::Quantity, value))
    }

    fn time(time: &str, calendar: &str, timezone: u64) -> Option<String> {
        let value = Value::Time(TimeValue::new(0, 0, calendar, 11, time, timezone));
        qs_line(value_snak(SnakDataType::Time, value))
    }

    const GREGORIAN: &str = "http://www.wikidata.org/entity/Q1985727";

    #[test]
    fn test_item2qs_entity_values_are_bare() {
        for (entity_type, id) in [
            (EntityType::Property, "P31"),
            (EntityType::Lexeme, "L7"),
            (EntityType::Lexeme, "L7-F2"),
            (EntityType::Lexeme, "L7-S1"),
            (EntityType::MediaInfo, "M5"),
        ] {
            let value = Value::Entity(EntityValue::new(entity_type, id));
            assert_eq!(
                qs_line(value_snak(SnakDataType::WikibaseItem, value)),
                Some(format!("LAST\tP1\t{id}"))
            );
        }
        let malformed = Value::Entity(EntityValue::new(EntityType::Item, "Q5 "));
        assert_eq!(
            qs_line(value_snak(SnakDataType::WikibaseItem, malformed)),
            None
        );
    }

    #[test]
    fn test_item2qs_quantity_unit() {
        assert_eq!(
            quantity(-1.5, None, "http://www.wikidata.org/entity/Q11573"),
            Some("LAST\tP1\t-1.5U11573".to_string())
        );
        assert_eq!(quantity(42.0, None, "1"), Some("LAST\tP1\t42".to_string()));
        // A unit QuickStatements cannot name must not be dropped silently.
        assert_eq!(quantity(1.0, None, "http://example.org/Q1"), None);
        assert_eq!(
            quantity(1.0, None, "http://www.wikidata.org/entity/P1"),
            None
        );
    }

    #[test]
    fn test_item2qs_quantity_bounds() {
        assert_eq!(
            quantity(
                5.0,
                Some((4.5, 5.5)),
                "http://www.wikidata.org/entity/Q11570"
            ),
            Some("LAST\tP1\t5~0.5U11570".to_string())
        );
        assert_eq!(
            quantity(5.0, Some((5.0, 5.0)), "1"),
            Some("LAST\tP1\t5~0".to_string())
        );
        // Asymmetric bounds have no QuickStatements syntax.
        assert_eq!(quantity(5.0, Some((4.0, 5.5)), "1"), None);
        // Neither does a tolerance that does not reproduce the bounds exactly.
        assert_eq!(quantity(1.1, Some((1.0, 1.2)), "1"), None);
        assert_eq!(quantity(f64::NAN, None, "1"), None);
    }

    #[test]
    fn test_item2qs_time_calendars() {
        assert_eq!(
            time(
                "+1582-10-04T00:00:00Z",
                "http://www.wikidata.org/entity/Q1985786",
                0
            ),
            Some("LAST\tP1\t+1582-10-04T00:00:00Z/11/J".to_string())
        );
        assert_eq!(
            time("-0500-01-01T00:00:00Z", GREGORIAN, 0),
            Some("LAST\tP1\t-0500-01-01T00:00:00Z/11".to_string())
        );
        // Another calendar, a timezone, or a malformed timestamp would be
        // read differently.
        assert_eq!(
            time("+1582-10-04T00:00:00Z", "http://example.org/calendar", 0),
            None
        );
        assert_eq!(time("+1582-10-04T00:00:00Z", GREGORIAN, 60), None);
        assert_eq!(time("+1582-10-04", GREGORIAN, 0), None);
    }

    #[test]
    fn test_item2qs_coordinate_globe_and_altitude() {
        let coordinate = |globe: &str, altitude| {
            let value = Value::Coordinate(Coordinate::new(
                altitude,
                globe.to_string(),
                -12.5,
                0.000001,
                Some(0.1),
            ));
            qs_line(value_snak(SnakDataType::GlobeCoordinate, value))
        };
        assert_eq!(
            coordinate("http://www.wikidata.org/entity/Q2", None),
            Some("LAST\tP1\t@-12.5/0.000001".to_string())
        );
        // QuickStatements would put a Mars coordinate on Earth.
        assert_eq!(
            coordinate("http://www.wikidata.org/entity/Q111", None),
            None
        );
        assert_eq!(
            coordinate("http://www.wikidata.org/entity/Q2", Some(10.0)),
            None
        );
    }

    #[test]
    fn test_item2qs_text_needs_no_escaping_but_separators_are_skipped() {
        let string = |s: &str| qs_line(Snak::new_string("P1", s));
        assert_eq!(
            string("say \"hi\""),
            Some("LAST\tP1\t\"say \"hi\"\"".to_string())
        );
        for unsafe_text in ["a\tb", "a\nb", "a\rb", "a||b", "a /* b */"] {
            assert_eq!(string(unsafe_text), None, "{unsafe_text:?}");
        }

        let monolingual = |language: &str, text: &str| {
            let value = Value::MonoLingual(MonoLingualText::new(text, language));
            qs_line(value_snak(SnakDataType::MonolingualText, value))
        };
        assert_eq!(
            monolingual("be-tarask", "a \"b\""),
            Some("LAST\tP1\tbe-tarask:\"a \"b\"\"".to_string())
        );
        assert_eq!(monolingual("en", "a\tb"), None);
        assert_eq!(monolingual("EN", "a"), None);
    }

    #[test]
    fn test_item2qs_skips_terms_quickstatements_would_misread() {
        let mut item = ItemEntity::new_empty();
        item.labels_mut().push(LocaleString::new("en", "a\tb"));
        item.descriptions_mut()
            .push(LocaleString::new("de", "fine \"quoted\""));
        item.aliases_mut().push(LocaleString::new("en", "a||b"));
        item.sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Line\nbreak", vec![]));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert_eq!(
            qs,
            vec![
                "CREATE".to_string(),
                "LAST\tDde\t\"fine \"quoted\"\"".to_string()
            ]
        );
    }

    #[test]
    fn test_item2qs_statement_with_qualifier() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![Snak::new_item("P407", "Q1860")],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Property, value, qualifier property, (bare) qualifier value
        assert!(
            qs.contains(&"LAST\tP1476\t\"hello\"\tP407\tQ1860".to_string()),
            "got: {qs:?}"
        );
    }

    #[test]
    fn test_item2qs_statement_with_reference() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![Reference::new(vec![Snak::new_url(
                "P854",
                "http://example.com",
            )])],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Reference property becomes S854, prefixed with ! to start new ref section
        assert!(qs
            .iter()
            .any(|s| s.contains("!S854") && s.contains("http://example.com")));
    }

    #[test]
    fn test_item2qs_statement_with_multiple_references() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![
                Reference::new(vec![Snak::new_url("P854", "http://first.com")]),
                Reference::new(vec![Snak::new_url("P854", "http://second.com")]),
            ],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        let line = qs.iter().find(|s| s.starts_with("LAST\tP1476")).unwrap();
        // Both references start with !S854
        assert_eq!(line.matches("!S854").count(), 2);
    }

    #[test]
    fn test_item2qs_skips_novalue_snak() {
        let mut item = ItemEntity::new_empty();
        // A novalue snak — snak2qs should return None, so this claim is skipped
        item.add_claim(Statement::new_normal(
            Snak::new(SnakDataType::WikibaseItem, "P31", SnakType::NoValue, None),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Only "CREATE" — the novalue statement must be skipped
        assert!(!qs.iter().any(|s| s.contains("P31")));
    }

    #[test]
    fn test_item2qs_quantity() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::Quantity,
                "P1082",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Quantity,
                    Value::Quantity(QuantityValue::new(42.0, None, "1", None)),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.iter().any(|s| s.contains("P1082") && s.contains("42")));
    }

    #[test]
    fn test_item2qs_monolingual() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::MonolingualText,
                "P1705",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::MonoLingualText,
                    Value::MonoLingual(MonoLingualText::new("Original title", "en")),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P1705") && s.contains("en:\"Original title\"")));
    }

    #[test]
    fn test_item2qs_coordinate() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::GlobeCoordinate,
                "P625",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::GlobeCoordinate,
                    Value::Coordinate(Coordinate::new(
                        None,
                        "http://www.wikidata.org/entity/Q2".to_string(),
                        51.5,
                        -0.1,
                        None,
                    )),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P625") && s.contains("@51.5/-0.1")));
    }

    #[test]
    fn test_item2qs_time_value() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::Time,
                "P569",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Time,
                    Value::Time(TimeValue::new(
                        0,
                        0,
                        "http://www.wikidata.org/entity/Q1985727",
                        11,
                        "+1990-05-17T00:00:00Z",
                        0,
                    )),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P569") && s.contains("+1990-05-17T00:00:00Z/11")));
    }

    #[test]
    fn test_set_user_agent() {
        let mut wd = Wikidata::new(TEST_USER_AGENT);
        wd.set_user_agent("my-agent/1.0");
        // Verify it builds without error (can't easily inspect the built client's UA)
        assert!(wd.reqwest_client().is_ok());
    }

    #[test]
    fn test_set_timeout() {
        let mut wd = Wikidata::new(TEST_USER_AGENT);
        wd.set_timeout(std::time::Duration::from_secs(30));
        assert!(wd.reqwest_client().is_ok());
    }

    #[test]
    fn test_item2qs_novalue_qualifier_is_skipped() {
        // A qualifier whose snak_type is NoValue has no concrete value and must be
        // silently skipped by snak2qs / the qualifier filter_map.
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "title"),
            vec![Snak::new(
                SnakDataType::WikibaseItem,
                "P31",
                SnakType::NoValue,
                None,
            )],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        let line = qs.iter().find(|s| s.contains("P1476")).unwrap();
        // The NoValue qualifier must not appear in the output.
        assert!(
            !line.contains("P31"),
            "NoValue qualifier must be omitted from QS output, got: {line}"
        );
    }

    #[test]
    fn test_item2qs_novalue_reference_snak_is_skipped() {
        // A reference snak with NoValue produces no value string and must be
        // omitted from the QS reference section.
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "title"),
            vec![],
            vec![Reference::new(vec![
                Snak::new_url("P854", "http://example.com"), // valid — must appear
                Snak::new(SnakDataType::WikibaseItem, "P248", SnakType::NoValue, None), // must be skipped
            ])],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        let line = qs.iter().find(|s| s.contains("P1476")).unwrap();
        // The valid URL reference must be present.
        assert!(
            line.contains("S854") && line.contains("http://example.com"),
            "URL reference snak must be present, got: {line}"
        );
        // The NoValue reference snak must not appear.
        assert!(
            !line.contains("S248"),
            "NoValue reference snak must be omitted, got: {line}"
        );
    }

    #[test]
    fn test_item2qs_multiple_aliases() {
        // All aliases must each get their own LAST\tA… line.
        let mut item = ItemEntity::new_empty();
        item.aliases_mut().push(LocaleString::new("en", "first"));
        item.aliases_mut().push(LocaleString::new("en", "second"));
        item.aliases_mut().push(LocaleString::new("de", "erstes"));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tAen\t\"first\"".to_string()));
        assert!(qs.contains(&"LAST\tAen\t\"second\"".to_string()));
        assert!(qs.contains(&"LAST\tAde\t\"erstes\"".to_string()));
    }

    #[test]
    fn test_wikidata_new_builds_client() {
        // Wikidata::new must produce a working instance whose client builder
        // compiles and builds without error.
        let wd = Wikidata::new(TEST_USER_AGENT);
        assert!(wd.reqwest_client().is_ok());
    }

    #[test]
    fn test_url_getters_reflect_the_defaults_and_overrides() {
        let mut wd = Wikidata::new(TEST_USER_AGENT);
        assert_eq!(wd.api_url(), DEFAULT_API_URL);

        wd.set_api_url("http://localhost:1234/w/api.php");
        assert_eq!(wd.api_url(), "http://localhost:1234/w/api.php");
    }

    #[test]
    fn test_item2qs_emits_coordinate_values() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::WikibaseItem,
                "P31",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::GlobeCoordinate,
                    wikibase::Value::Coordinate(wikibase::Coordinate::new(
                        None,
                        "http://www.wikidata.org/entity/Q2".to_string(),
                        1.0,
                        2.0,
                        Some(0.1),
                    )),
                )),
            ),
            vec![],
            vec![],
        ));
        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.iter().any(|l| l.contains("@1/2")), "got: {qs:?}");
    }

    #[test]
    fn test_item2qs_skips_unsupported_value_type() {
        // `EntitySchema` is the one Value variant snak2qs has no QuickStatements
        // representation for, so such a claim must be skipped rather than
        // emitted malformed.
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::WikibaseItem,
                "P31",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::EntityId,
                    wikibase::Value::EntitySchema(EntityValue::new(
                        EntityType::EntitySchema,
                        "E123",
                    )),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert_eq!(qs, vec!["CREATE".to_string()], "got: {qs:?}");
    }

    // ── plain-HTTP calls ─────────────────────────────────────────────────────

    use crate::test_support::mount_post as mock_post;
    use wiremock::MockServer;

    fn id(s: &str) -> EntityId {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn test_get_entities_skips_missing_and_keys_redirects_by_request() {
        let server = MockServer::start().await;
        mock_post(
            &server,
            "action=wbgetentities",
            serde_json::json!({"entities": {
                "Q42": {"id": "Q42", "type": "item"},
                "Q1": {"id": "Q2", "type": "item", "redirects": {"from": "Q1", "to": "Q2"}},
                "Q404": {"id": "Q404", "missing": ""}
            }}),
        )
        .await;
        let wd = crate::test_support::wikidata_for(&server);
        let entities = wd
            .get_entities(
                &[id("Q42"), id("Q1"), id("Q404"), id("Q42")],
                &["labels"],
                &[],
            )
            .await
            .unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[&id("Q42")]["id"], "Q42");
        assert_eq!(entities[&id("Q1")]["id"], "Q2");
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "duplicates must be fetched once");
        let body = String::from_utf8_lossy(&requests[0].body).to_string();
        assert!(body.contains("props=labels"), "{body}");
        assert!(!body.contains("languages="), "{body}");
        let user_agent = requests[0].headers.get("user-agent").unwrap();
        assert_eq!(user_agent.to_str().unwrap(), TEST_USER_AGENT);
    }

    #[tokio::test]
    async fn test_get_entities_batches_by_fifty() {
        let server = MockServer::start().await;
        mock_post(
            &server,
            "action=wbgetentities",
            serde_json::json!({"entities": {}}),
        )
        .await;
        let wd = crate::test_support::wikidata_for(&server);
        let ids: Vec<EntityId> = (1..=120).map(|n| ItemId::new(n).into()).collect();
        assert!(wd.get_entities(&ids, &[], &[]).await.unwrap().is_empty());
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        assert!(wd.get_entities(&[], &[], &[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_api_error_is_reported() {
        let server = MockServer::start().await;
        mock_post(
            &server,
            "action=wbgetentities",
            serde_json::json!({"error": {"code": "no-such-entity", "info": "Could not find"}}),
        )
        .await;
        let wd = crate::test_support::wikidata_for(&server);
        let error = wd.get_entities(&[id("Q1")], &[], &[]).await.unwrap_err();
        assert!(
            matches!(&error, WikidataError::Api { code, .. } if code == "no-such-entity"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn test_items_for_titles_tolerates_missing_and_normalisation() {
        let server = MockServer::start().await;
        mock_post(
            &server,
            "sites=enwiki",
            serde_json::json!({"entities": {
                "Q42": {"id": "Q42", "sitelinks": {"enwiki": {"site": "enwiki", "title": "Douglas Adams"}}},
                "-1": {"site": "enwiki", "title": "No such page", "missing": ""}
            }}),
        )
        .await;
        let wd = crate::test_support::wikidata_for(&server);
        let items = wd
            .items_for_titles("enwiki", &["douglas_Adams", "No such page"])
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items["douglas_Adams"], ItemId::new(42));
    }

    #[tokio::test]
    async fn test_search() {
        let server = MockServer::start().await;
        mock_post(
            &server,
            "list=search",
            serde_json::json!({"query": {
                "searchinfo": {"totalhits": 3},
                "search": [{"title": "Q1"}, {"title": "Property:P31"}, {"title": "Talk:Q1"}, {"title": "Main Page"}]
            }}),
        )
        .await;
        let wd = crate::test_support::wikidata_for(&server);
        let results = wd.search("foo", 0, 10).await.unwrap();
        assert_eq!(results.total_hits, 3);
        assert_eq!(results.ids, vec![id("Q1"), id("P31"), id("Q1")]);
        assert_eq!(
            wd.search_items("foo", 10).await.unwrap(),
            vec![ItemId::new(1), ItemId::new(1)]
        );
        // Three hits are ambiguous.
        assert_eq!(wd.search_single_item("foo").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_search_single_item() {
        let server = MockServer::start().await;
        mock_post(
            &server,
            "list=search",
            serde_json::json!({"query": {"searchinfo": {"totalhits": 1}, "search": [{"title": "Q42"}]}}),
        )
        .await;
        let wd = crate::test_support::wikidata_for(&server);
        assert_eq!(
            wd.search_single_item("haswbstatement:P214=113230702")
                .await
                .unwrap(),
            Some(ItemId::new(42))
        );
    }

    #[tokio::test]
    async fn test_api_sends_the_configured_user_agent() {
        let server = MockServer::start().await;
        crate::test_support::mount_siteinfo(&server).await;
        let wd = crate::test_support::wikidata_for(&server);
        let api = wd.api().await.unwrap();
        assert_eq!(api.user_agent(), TEST_USER_AGENT);
    }

    #[test]
    fn test_cirrus_query() {
        let query = CirrusQuery::new(" Magnus ")
            .has_statement(PropertyId::new(214), "30701597")
            .not_has_statement(PropertyId::new(31), ItemId::new(13442814))
            .has_property(PropertyId::new(18))
            .not_has_property(PropertyId::new(570))
            .in_label("Manske", &["en", "de"]);
        assert_eq!(
            query.to_string(),
            "Magnus haswbstatement:\"P214=30701597\" -haswbstatement:\"P31=Q13442814\" \
             haswbstatement:P18 -haswbstatement:P570 inlabel:\"Manske@en,de\""
        );
        assert_eq!(CirrusQuery::new("").to_string(), "");
        assert_eq!(
            CirrusQuery::default()
                .has_statement(PropertyId::new(1), "a \"b\"")
                .to_string(),
            r#"haswbstatement:"P1=a \"b\"""#
        );
    }

    #[test]
    fn test_same_page_title() {
        assert!(same_page_title("Douglas Adams", "douglas_Adams"));
        assert!(!same_page_title("Douglas Adams", "Douglas adams"));
        assert!(same_page_title("", ""));
    }

    #[tokio::test]
    async fn test_setters_apply_to_the_shared_client() {
        let server = MockServer::start().await;
        mock_post(&server, "list=search", serde_json::json!({"query": {}})).await;
        let mut wd = crate::test_support::wikidata_for(&server);
        let _ = wd.http_client().unwrap();
        let clone = wd.clone();
        wd.set_user_agent("other/1.0");
        // The client is kept, and shared with the clone ...
        assert!(wd.client.get().is_some());
        assert!(Arc::ptr_eq(&wd.client, &clone.client));
        // ... but the new user agent is sent all the same.
        wd.search("x", 0, 1).await.unwrap();
        clone.search("x", 0, 1).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let agents: Vec<&str> = requests
            .iter()
            .map(|r| r.headers["user-agent"].to_str().unwrap())
            .collect();
        assert_eq!(agents, ["other/1.0", TEST_USER_AGENT]);
    }

    #[tokio::test]
    async fn test_with_client_uses_the_given_client_and_still_sends_the_user_agent() {
        let server = MockServer::start().await;
        mock_post(&server, "list=search", serde_json::json!({"query": {}})).await;
        let client = Client::builder().user_agent("ignored/0").build().unwrap();
        let mut wd = Wikidata::with_client(TEST_USER_AGENT, client);
        wd.set_api_url(&crate::test_support::api_url(&server));
        wd.search("x", 0, 1).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests[0].headers["user-agent"], TEST_USER_AGENT);
    }

    #[tokio::test]
    async fn test_get_entities_with_passes_extra_parameters() {
        let server = MockServer::start().await;
        mock_post(
            &server,
            "action=wbgetentities",
            serde_json::json!({"entities": {"Q42": {"id": "Q42"}}}),
        )
        .await;
        let wd = crate::test_support::wikidata_for(&server);
        let entities = wd
            .get_entities_with(
                &[id("Q42")],
                &[
                    ("sitefilter", "enwiki"),
                    ("ids", "Q1"),
                    ("props", "sitelinks"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(entities.len(), 1);
        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&requests[0].body).to_string();
        assert!(body.contains("sitefilter=enwiki"), "{body}");
        assert!(body.contains("props=sitelinks"), "{body}");
        assert!(
            !body.contains("Q1"),
            "reserved parameters are ignored: {body}"
        );
    }

    /// Compile-time check: the batched calls can be awaited in a spawned
    /// task or an axum handler. Never run.
    #[allow(dead_code)]
    fn batched_calls_are_send(wd: &Wikidata, ids: &[EntityId], titles: &[&str]) {
        fn assert_send<T: Send>(_: T) {}
        assert_send(wd.get_entities(ids, &[], &[]));
        assert_send(wd.get_entities_with(ids, &[]));
        assert_send(wd.items_for_titles("enwiki", titles));
        assert_send(wd.search("x", 0, 1));
        assert_send(wd.search_items("x", 1));
        assert_send(wd.search_single_item("x"));
        assert_send(wd.api());
    }
}
