//! Session primitives shared by the tools' websites.
//!
//! The stores differ (OAuth tokens in encrypted cookies, `tower-sessions`,
//! password sessions in a database), so only the pieces that must not drift
//! live here: the signed stateless cookie, the cookie attributes, and the
//! safe-return-path check.

use axum::http::{header::COOKIE, HeaderMap};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Signs and checks a stateless cookie value.
#[derive(Clone)]
pub struct SessionKey([u8; 32]);

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(…)")
    }
}

#[allow(clippy::expect_used)] // HMAC-SHA256 accepts a key of any length; the error is unreachable.
impl SessionKey {
    /// A key derived from `secret`, with a context that separates this
    /// cookie from other uses of the same secret.
    pub fn derive_with_context(secret: &str, context: &str) -> Self {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .expect("HMAC takes keys of any length");
        mac.update(context.as_bytes());
        Self(mac.finalize().into_bytes().into())
    }

    /// A key derived from `secret` with this crate's default context.
    pub fn derive(secret: &str) -> Self {
        Self::derive_with_context(secret, "wikimisc session")
    }

    /// `{expires}.{payload}.{signature}`, the latter two in base64.
    pub fn sign(&self, payload: &str, expires: u64) -> String {
        let payload = format!("{expires}.{}", URL_SAFE_NO_PAD.encode(payload));
        let signature = URL_SAFE_NO_PAD.encode(self.mac(&payload).finalize().into_bytes());
        format!("{payload}.{signature}")
    }

    /// The payload in a signed value, if the signature is right and the
    /// value has not expired by `now` (Unix time).
    pub fn verify(&self, value: &str, now: u64) -> Option<String> {
        let (payload, signature) = value.rsplit_once('.')?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        self.mac(payload).verify_slice(&signature).ok()?;
        let (expires, payload) = payload.split_once('.')?;
        if expires.parse::<u64>().ok()? <= now {
            return None;
        }
        String::from_utf8(URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
    }

    fn mac(&self, payload: &str) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC takes keys of any length");
        mac.update(payload.as_bytes());
        mac
    }
}

/// The current Unix time in seconds (0 before the epoch, which cannot happen
/// in practice).
pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A `Set-Cookie` value for a session cookie: `Path=/`, `HttpOnly`,
/// `SameSite=Lax`, and `Secure` when `secure` is set.
pub fn set_cookie(name: &str, value: &str, max_age: Duration, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{name}={value}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax{secure}",
        max_age.as_secs()
    )
}

/// A `Set-Cookie` value that clears `name`.
pub fn clear_cookie(name: &str, secure: bool) -> String {
    set_cookie(name, "", Duration::ZERO, secure)
}

/// The value of cookie `name` in a request.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// Only same-site paths, so a login cannot be abused as an open redirect.
pub fn safe_return_path(path: Option<&str>) -> String {
    match path {
        Some(p) if is_safe_return_path(p) => p.to_string(),
        _ => "/".to_string(),
    }
}

/// Whether `path` is a same-site path: starts with `/`, but not `//` or `/\`.
pub fn is_safe_return_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn key() -> SessionKey {
        SessionKey::derive_with_context("consumer secret", "test")
    }

    #[test]
    fn signs_and_verifies() {
        let value = key().sign("Magnus Manske", 2000);
        assert_eq!(key().verify(&value, 1000).as_deref(), Some("Magnus Manske"));
        assert_eq!(key().verify(&value, 2000), None);
        let forged = value.replacen(
            &URL_SAFE_NO_PAD.encode("Magnus Manske"),
            &URL_SAFE_NO_PAD.encode("Someone Else"),
            1,
        );
        assert_eq!(key().verify(&forged, 1000), None);
        assert_eq!(
            SessionKey::derive_with_context("other", "test").verify(&value, 1000),
            None
        );
    }

    #[test]
    fn cookie_attributes() {
        assert_eq!(
            set_cookie("name", "value", Duration::from_secs(60), true),
            "name=value; Max-Age=60; Path=/; HttpOnly; SameSite=Lax; Secure"
        );
        assert!(clear_cookie("name", false).contains("Max-Age=0"));
    }

    #[test]
    fn reads_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("a=1; name=value; b=2"));
        assert_eq!(cookie(&headers, "name"), Some("value"));
        assert_eq!(cookie(&headers, "missing"), None);
    }

    #[test]
    fn return_paths() {
        assert_eq!(safe_return_path(Some("/?p=345&run=1")), "/?p=345&run=1");
        assert_eq!(safe_return_path(Some("//evil.example")), "/");
        assert_eq!(safe_return_path(Some("https://evil.example")), "/");
        assert_eq!(safe_return_path(Some("/\\evil.example")), "/");
        assert_eq!(safe_return_path(None), "/");
    }
}
