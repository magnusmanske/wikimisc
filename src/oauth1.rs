//! MediaWiki OAuth 1.0a, as the PHP tools' `MW_OAuth` did it: the login
//! handshake on meta's Special:OAuth, and signed API requests on behalf of a
//! user. Tokens live in encrypted cookies (see [`Session`]).

use anyhow::{anyhow, bail, Context, Result};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::PrivateCookieJar;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::Sha1;
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const OAUTH_URL: &str = "https://www.mediawiki.org/wiki/Special:OAuth";
/// An access token cookie lasts as long as the old tools' did.
const LOGIN_DAYS: i64 = 90;
/// An authorization not finished within this time is gone.
const HANDSHAKE_MINUTES: i64 = 60;

/// A registered OAuth consumer, from an ini file with `consumerKey`,
/// `consumerSecret` and `agent`.
#[derive(Clone)]
pub struct Consumer {
    key: String,
    secret: String,
    agent: String,
}

impl std::fmt::Debug for Consumer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Consumer")
            .field("key", &self.key)
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

impl Consumer {
    pub fn new(
        key: impl Into<String>,
        secret: impl Into<String>,
        agent: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            secret: secret.into(),
            agent: agent.into(),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    pub fn agent(&self) -> &str {
        &self.agent
    }

    pub fn from_ini(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let values: HashMap<&str, &str> = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim(), v.trim().trim_matches(['"', '\''])))
            .collect();
        let get = |key: &str| {
            values
                .get(key)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string())
                .ok_or_else(|| anyhow!("{} lacks '{key}'", path.display()))
        };
        Ok(Self {
            key: get("consumerKey")?,
            secret: get("consumerSecret")?,
            agent: get("agent")?,
        })
    }
}

/// A request or access token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub key: String,
    pub secret: String,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct OAuth {
    consumer: Consumer,
    http: reqwest::Client,
    base_url: String,
    public_url: String,
}

impl OAuth {
    pub fn new(consumer: Consumer, http: reqwest::Client) -> Self {
        Self {
            consumer,
            http,
            base_url: OAUTH_URL.to_string(),
            public_url: OAUTH_URL.to_string(),
        }
    }

    /// Use a different `Special:OAuth` base URL, and a different public URL
    /// for the authorization link (some tools reach MediaWiki differently
    /// from the user's browser).
    pub fn with_urls(mut self, base_url: impl Into<String>, public_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self.public_url = public_url.into();
        self
    }

    pub fn consumer(&self) -> &Consumer {
        &self.consumer
    }

    /// Where to send the user to approve `token_key`.
    pub fn authorize_url(&self, token_key: &str) -> String {
        authorize_url(&self.public_url, &self.consumer.key, token_key)
    }

    /// Step 1: a request token, and where to send the user to approve it.
    pub async fn initiate(&self, callback: &str) -> Result<(Token, String)> {
        let url = format!("{}/initiate", self.base_url);
        let mut params = self.oauth_params(None);
        params.extend([
            ("format".to_string(), "json".to_string()),
            ("oauth_callback".to_string(), callback.to_string()),
        ]);
        let token = self.signed_get(&url, params, "").await?;
        Ok((token.clone(), self.authorize_url(&token.key)))
    }

    /// Step 2: the access token, for the request token the user approved.
    pub async fn access_token(&self, request: &Token, verifier: &str) -> Result<Token> {
        let url = format!("{}/token", self.base_url);
        let mut params = self.oauth_params(Some(&request.key));
        params.extend([
            ("format".to_string(), "json".to_string()),
            ("oauth_verifier".to_string(), verifier.to_string()),
        ]);
        self.signed_get(&url, params, &request.secret).await
    }

    /// A POST to a wiki's action API, signed for the user (or with an empty
    /// token, which the API answers with an error, like the old tools did).
    pub async fn api(
        &self,
        api_url: &str,
        token: Option<&Token>,
        params: &[(&str, &str)],
    ) -> Result<Value> {
        let mut post: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        post.push(("format".to_string(), "json".to_string()));
        let header = self.authorization("POST", api_url, &post, token)?;
        self.http
            .post(api_url)
            .header(reqwest::header::USER_AGENT, &self.consumer.agent)
            .header(reqwest::header::AUTHORIZATION, header)
            .form(&post)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .context("OAuth API request failed")?
            .json()
            .await
            .context("OAuth API answer is not JSON")
    }

    /// A file upload (multipart POST) to a wiki's action API. As in the old
    /// tools, only the OAuth parameters are signed, not the form fields.
    pub async fn api_upload(
        &self,
        api_url: &str,
        token: &Token,
        fields: &[(&str, String)],
        file: reqwest::multipart::Part,
    ) -> Result<Value> {
        let header = self.authorization("POST", api_url, &[], Some(token))?;
        let form = fields
            .iter()
            .fold(reqwest::multipart::Form::new(), |form, (k, v)| {
                form.text(k.to_string(), v.clone())
            })
            .text("format", "json")
            .part("file", file);
        self.http
            .post(api_url)
            .header(reqwest::header::USER_AGENT, &self.consumer.agent)
            .header(reqwest::header::AUTHORIZATION, header)
            .multipart(form)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .context("upload request failed")?
            .json()
            .await
            .context("upload answer is not JSON")
    }

    /// The `Authorization: OAuth …` header for a request whose body (or
    /// query) is `params`.
    pub fn authorization(
        &self,
        method: &str,
        url: &str,
        params: &[(String, String)],
        token: Option<&Token>,
    ) -> Result<String> {
        let mut oauth = self.oauth_params(Some(token.map_or("", |t| t.key.as_str())));
        let mut signed = params.to_vec();
        signed.extend(oauth.iter().cloned());
        let secret = token.map_or("", |t| t.secret.as_str());
        oauth.push((
            "oauth_signature".to_string(),
            sign(method, url, &signed, &self.consumer.secret, secret)?,
        ));
        let fields: Vec<String> = oauth
            .iter()
            .map(|(k, v)| format!("{}=\"{}\"", encode(k), encode(v)))
            .collect();
        Ok(format!("OAuth {}", fields.join(", ")))
    }

    fn oauth_params(&self, token: Option<&str>) -> Vec<(String, String)> {
        let nonce: String = rand::random::<[u8; 16]>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let mut params = vec![
            ("oauth_consumer_key".to_string(), self.consumer.key.clone()),
            ("oauth_version".to_string(), "1.0".to_string()),
            ("oauth_nonce".to_string(), nonce),
            ("oauth_timestamp".to_string(), timestamp.to_string()),
            (
                "oauth_signature_method".to_string(),
                "HMAC-SHA1".to_string(),
            ),
        ];
        if let Some(token) = token {
            params.push(("oauth_token".to_string(), token.to_string()));
        }
        params
    }

    async fn signed_get(
        &self,
        url: &str,
        mut params: Vec<(String, String)>,
        token_secret: &str,
    ) -> Result<Token> {
        params.push((
            "oauth_signature".to_string(),
            sign("GET", url, &params, &self.consumer.secret, token_secret)?,
        ));
        let json: Value = self
            .http
            .get(url)
            .header(reqwest::header::USER_AGENT, &self.consumer.agent)
            .query(&params)
            .send()
            .await
            .context("OAuth request failed")?
            .json()
            .await
            .context("OAuth answer is not JSON")?;
        if let Some(error) = json.get("error") {
            bail!("OAuth error: {error}");
        }
        // Current MediaWiki: oauth_token/oauth_token_secret; older: key/secret.
        let field = |a: &str, b: &str| json[a].as_str().or(json[b].as_str()).map(str::to_string);
        match (
            field("oauth_token", "key"),
            field("oauth_token_secret", "secret"),
        ) {
            (Some(key), Some(secret)) => Ok(Token { key, secret }),
            _ => bail!("unexpected OAuth answer: {json}"),
        }
    }
}

/// HMAC-SHA1 signature of a request, exactly as PHP `MW_OAuth::sign_request`.
fn sign(
    method: &str,
    url: &str,
    params: &[(String, String)],
    consumer_secret: &str,
    token_secret: &str,
) -> Result<String> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("invalid URL {url}"))?;
    let host = parsed.host_str().unwrap_or_default();
    let base = match parsed.port() {
        Some(port) => format!("{}://{host}:{port}{}", parsed.scheme(), parsed.path()),
        None => format!("{}://{host}{}", parsed.scheme(), parsed.path()),
    };
    let mut pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .chain(params.iter().cloned())
        .filter(|(k, _)| k != "oauth_signature")
        .map(|(k, v)| (encode(&k), encode(&v)))
        .collect();
    pairs.sort();
    let joined: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let text = format!(
        "{}&{}&{}",
        encode(&method.to_uppercase()),
        encode(&base),
        encode(&joined.join("&"))
    );
    let key = format!("{}&{}", encode(consumer_secret), encode(token_secret));
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes()).map_err(|e| anyhow!("HMAC: {e}"))?;
    mac.update(text.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// RFC 3986 encoding, as PHP's `rawurlencode`.
fn encode(s: &str) -> String {
    crate::wiki::encode_component(s)
}

/// Where a tool keeps its OAuth tokens: encrypted cookies under its own
/// names, so that tools sharing this host cannot pick up each other's login.
#[derive(Debug, Clone, Copy)]
pub struct Session {
    pub name: &'static str,
    pub path: &'static str,
}

impl Session {
    pub fn access_token(&self, jar: &PrivateCookieJar) -> Option<Token> {
        Self::read(jar, self.name)
    }

    pub fn request_token(&self, jar: &PrivateCookieJar) -> Option<Token> {
        Self::read(jar, &self.handshake_name())
    }

    pub fn with_request_token(&self, jar: PrivateCookieJar, token: &Token) -> PrivateCookieJar {
        jar.add(self.cookie(
            self.handshake_name(),
            token,
            time::Duration::minutes(HANDSHAKE_MINUTES),
        ))
    }

    /// Stores the access token, and forgets the finished handshake.
    pub fn with_access_token(&self, jar: PrivateCookieJar, token: &Token) -> PrivateCookieJar {
        jar.remove(Cookie::build(self.handshake_name()).path(self.path))
            .add(self.cookie(
                self.name.to_string(),
                token,
                time::Duration::days(LOGIN_DAYS),
            ))
    }

    /// Forgets the login (and any handshake).
    pub fn without_tokens(&self, jar: PrivateCookieJar) -> PrivateCookieJar {
        jar.remove(Cookie::build(self.handshake_name()).path(self.path))
            .remove(Cookie::build(self.name).path(self.path))
    }

    fn handshake_name(&self) -> String {
        format!("{}_handshake", self.name)
    }

    fn read(jar: &PrivateCookieJar, name: &str) -> Option<Token> {
        serde_json::from_str(jar.get(name)?.value()).ok()
    }

    fn cookie(&self, name: String, token: &Token, max_age: time::Duration) -> Cookie<'static> {
        Cookie::build((name, serde_json::to_string(token).unwrap_or_default()))
            .path(self.path)
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Lax)
            .max_age(max_age)
            .build()
    }
}

/// The authorization URL for `token_key`, on `public_url` (which may already
/// contain a query string, as `.../index.php?title=Special:OAuth` does).
pub fn authorize_url(public_url: &str, consumer_key: &str, token_key: &str) -> String {
    let separator = if public_url.contains('?') { '&' } else { '?' };
    format!(
        "{public_url}/authorize{separator}oauth_token={}&oauth_consumer_key={}",
        encode(token_key),
        encode(consumer_key)
    )
}

/// Request tokens of logins in progress, for tools that keep the handshake
/// server-side instead of in a cookie.
#[derive(Debug)]
pub struct PendingTokens {
    ttl: std::time::Duration,
    max: usize,
    tokens: std::sync::Mutex<HashMap<String, (String, std::time::Instant)>>,
}

impl PendingTokens {
    pub fn new(ttl: std::time::Duration, max: usize) -> Self {
        Self {
            ttl,
            max,
            tokens: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Remembers `token` as a login in progress.
    pub fn remember(&self, token: &Token) -> Result<()> {
        let mut pending = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = std::time::Instant::now();
        pending.retain(|_, (_, issued)| now.duration_since(*issued) < self.ttl);
        if pending.len() >= self.max {
            return Err(anyhow!(
                "Too many logins in progress, please try again later."
            ));
        }
        pending.insert(token.key.clone(), (token.secret.clone(), now));
        Ok(())
    }

    /// Takes a login in progress; it can be used once, and only within `ttl`.
    pub fn take(&self, key: &str) -> Option<Token> {
        let mut pending = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (secret, issued) = pending.remove(key)?;
        (issued.elapsed() < self.ttl).then(|| Token {
            key: key.to_string(),
            secret,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // Expected signatures computed with the PHP original.
    #[test]
    fn signatures_match_php() {
        let params = pairs(&[
            ("action", "query"),
            ("meta", "userinfo"),
            ("uiprop", "blockinfo|groups|rights"),
            ("oauth_consumer_key", "ck"),
            ("oauth_token", "tk"),
            ("oauth_nonce", "n0nce"),
            ("oauth_timestamp", "1700000000"),
            ("oauth_version", "1.0"),
            ("oauth_signature_method", "HMAC-SHA1"),
            ("text", "Ä ~*!"),
        ]);
        let signature = sign(
            "POST",
            "https://www.mediawiki.org/w/api.php",
            &params,
            "consumer&secret",
            "token secret",
        )
        .unwrap();
        assert_eq!(signature, "LGExTIHUCc6kOdGfRPmri7bcuMQ=");

        let url = "https://www.mediawiki.org/wiki/Special:OAuth/initiate?format=json\
            &oauth_callback=https%3A%2F%2Fx.org%2Fa.php&oauth_consumer_key=ck&oauth_version=1.0&oauth_nonce=abc\
            &oauth_timestamp=1700000000&oauth_signature_method=HMAC-SHA1";
        assert_eq!(
            sign("GET", url, &[], "consumer&secret", "token secret").unwrap(),
            "5RmCvDDm38ICZHopQnMZofOAuIA="
        );
    }

    #[test]
    fn consumer_from_ini() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oauth.ini");
        std::fs::write(
            &path,
            "[settings]\nagent = 'Tool'\nconsumerKey = \"abc\"\nconsumerSecret = def\n",
        )
        .unwrap();
        let consumer = Consumer::from_ini(&path).unwrap();
        assert_eq!(
            (
                consumer.key.as_str(),
                consumer.secret.as_str(),
                consumer.agent.as_str()
            ),
            ("abc", "def", "Tool")
        );
        std::fs::write(&path, "consumerKey = abc\n").unwrap();
        assert!(Consumer::from_ini(&path).is_err());
    }
}
