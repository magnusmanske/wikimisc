//! SSRF-guarded HTTP clients.
//!
//! A client is obtained for a [`Target`], and the guard is a property of how
//! the client was built rather than something each call site has to remember:
//!
//! - [`guard_url`] rejects a non-HTTP scheme or a blocked IP literal before a
//!   request is issued (hyper skips the DNS resolver for IP literals, so the
//!   resolver alone cannot see those);
//! - a redirect policy re-runs the same check on every hop;
//! - [`PublicOnlyResolver`] filters hostnames whose DNS records point at a
//!   private/loopback/link-local/reserved address.
//!
//! Use [`get_guarded`] instead of `client.get(url).send()` for any
//! [`Target::Untrusted`] fetch.

use crate::http::{client_builder, HttpError};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Failure modes of the SSRF-guarded HTTP helpers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SsrfError {
    /// The URL could not be parsed.
    #[error("invalid URL '{url}': {message}")]
    InvalidUrl { url: String, message: String },

    /// The URL does not use the HTTP or HTTPS scheme.
    #[error("refusing non-HTTP scheme '{scheme}'")]
    NonHttpScheme { scheme: String },

    /// The URL has no host component.
    #[error("URL has no host")]
    NoHost,

    /// The URL names an IP address in a range that must not be fetched.
    #[error("refusing to fetch blocked address {ip} (private/loopback/link-local/reserved)")]
    BlockedAddress { ip: IpAddr },

    /// The redirect chain exceeded the `MAX_REDIRECTS` limit.
    #[error("too many redirects (>{MAX_REDIRECTS})")]
    TooManyRedirects,

    /// The `reqwest` client could not be built.
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(#[source] reqwest::Error),

    /// Sending the request failed.
    #[error("fetch of '{url}' failed: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    /// Reading the response body failed.
    #[error("response from '{url}': {source}")]
    Response {
        url: String,
        #[source]
        source: HttpError,
    },
}

/// Redirect hops allowed before we give up. Each hop is re-checked by
/// [`guard_url`], so this only bounds redirect loops.
const MAX_REDIRECTS: usize = 5;

/// How long a guarded client waits for a response unless the caller sets its
/// own timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a guarded client waits for a TCP/TLS handshake.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Who a client is going to talk to. This is the SSRF posture, and it is a
/// required argument precisely so that the safe option cannot be forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The URL originates, however indirectly, from user or catalog data.
    /// Installs the full SSRF guard.
    ///
    /// **This is the default choice.** If you are unsure which variant
    /// applies, it is this one: a needless guard on a public host costs
    /// nothing, while a missing one is a pivot into the internal network.
    Untrusted,
    /// The URL is a hard-coded constant pointing at a known Wikimedia or
    /// Toolforge service, with at most an id or query string interpolated
    /// into the path.
    ///
    /// No SSRF guard: there is no attacker-controlled host to guard against,
    /// and some of these legitimately resolve inside WMCS.
    FixedEndpoint,
}

/// True for any IP we refuse to fetch from: loopback, private, link-local
/// (which catches the AWS / WMCS metadata IP `169.254.169.254`), CGNAT,
/// benchmark, documentation, reserved, multicast, and unspecified ranges.
/// IPv6 covers the equivalent ranges plus `::ffff:0:0/96` IPv4-mapped
/// addresses, so a private IPv4 cannot be smuggled inside an AAAA response.
#[must_use]
pub const fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ipv4(&mapped);
            }
            is_blocked_ipv6(v6)
        }
    }
}

const fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_unspecified()        // 0.0.0.0/8
        || ip.is_loopback()    // 127.0.0.0/8
        || ip.is_private()     // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()  // 169.254.0.0/16 — includes cloud metadata
        || ip.is_broadcast()
        || ip.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || ip.is_multicast()
        // RFC 6598 carrier-grade NAT: 100.64.0.0/10
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 0b0100_0000)
        // RFC 2544 benchmark: 198.18.0.0/15
        || (o[0] == 198 && (o[1] & 0b1111_1110) == 18)
        // Reserved: 240.0.0.0/4 (excluding broadcast handled above)
        || o[0] >= 240
}

const fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    let s = ip.segments();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local fc00::/7
        || (s[0] & 0xfe00) == 0xfc00
        // Link-local fe80::/10
        || (s[0] & 0xffc0) == 0xfe80
}

/// A `reqwest` DNS resolver that drops every address in a blocked range. If
/// every resolved address is blocked the call returns an error and reqwest
/// surfaces it as a connect failure.
#[derive(Debug, Default, Clone, Copy)]
pub struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            // Port 0 is fine: reqwest only uses the IP component and supplies
            // its own port from the request URL.
            let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host.as_str(), 0_u16)).await
            {
                Ok(iter) => iter.collect(),
                Err(e) => return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
            };
            let filtered: Vec<SocketAddr> = addrs
                .into_iter()
                .filter(|sa| !is_blocked_ip(&sa.ip()))
                .collect();
            if filtered.is_empty() {
                let msg = format!(
                    "SSRF guard: '{host}' resolves only to blocked (private/loopback/link-local/reserved) addresses"
                );
                return Err(msg.into());
            }
            let iter: Addrs = Box::new(filtered.into_iter());
            Ok(iter)
        })
    }
}

/// A `reqwest::ClientBuilder` for `target`, sending `user_agent`.
///
/// The SSRF guard for [`Target::Untrusted`] is applied here rather than in
/// [`client_for`], so customising a client cannot silently drop it.
pub fn client_builder_for(target: Target, user_agent: &str) -> reqwest::ClientBuilder {
    let builder = client_builder(user_agent)
        .timeout(DEFAULT_TIMEOUT)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT);
    match target {
        Target::FixedEndpoint => builder,
        Target::Untrusted => builder
            .dns_resolver(Arc::new(PublicOnlyResolver))
            .redirect(guarded_redirect_policy()),
    }
}

/// Build a guarded or fixed-endpoint client with the default timeouts.
pub fn client_for(target: Target, user_agent: &str) -> Result<reqwest::Client, SsrfError> {
    client_builder_for(target, user_agent)
        .build()
        .map_err(SsrfError::ClientBuild)
}

/// Redirect policy that re-applies [`guard_url`] to every hop. Without this a
/// public host could bounce us to `http://169.254.169.254/` and `reqwest`
/// would follow it inside `send()`, never giving the caller a chance to look.
fn guarded_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error(SsrfError::TooManyRedirects);
        }
        match check_url(attempt.url()) {
            Ok(()) => attempt.follow(),
            // `stop()` rather than `error()`: the caller gets the redirect
            // response itself instead of a synthesised transport error.
            Err(e) => {
                log::warn!("SSRF guard: refusing redirect to {}: {e}", attempt.url());
                attempt.stop()
            }
        }
    })
}

/// Reject a URL that must not be fetched on behalf of a caller: a non-HTTP
/// scheme, or a host given as an IP literal in a blocked range.
///
/// Hostnames are deliberately *not* resolved here — that is the resolver's
/// job, and doing it twice would both double the DNS cost and open a
/// rebinding window between our check and reqwest's own lookup.
pub fn guard_url(url: &str) -> Result<reqwest::Url, SsrfError> {
    let parsed = reqwest::Url::parse(url).map_err(|e| SsrfError::InvalidUrl {
        url: url.to_string(),
        message: e.to_string(),
    })?;
    check_url(&parsed)?;
    Ok(parsed)
}

fn check_url(url: &reqwest::Url) -> Result<(), SsrfError> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(SsrfError::NonHttpScheme {
                scheme: other.to_string(),
            })
        }
    }
    let Some(host) = url.host_str() else {
        return Err(SsrfError::NoHost);
    };
    // Strip the brackets `Url` keeps around an IPv6 literal, then try to parse
    // the host as an address. A parse failure means it is a domain name, which
    // is the resolver's business rather than ours. This mirrors exactly what
    // hyper-util's connector does to decide whether to skip the resolver.
    let ip = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok();
    if let Some(ip) = ip {
        if is_blocked_ip(&ip) {
            return Err(SsrfError::BlockedAddress { ip });
        }
    }
    Ok(())
}

/// `GET url` with the SSRF pre-flight check applied. Use this instead of
/// `client.get(url).send()` for any [`Target::Untrusted`] fetch — the client's
/// resolver and redirect policy cover DNS and later hops, but only this covers
/// an IP literal in the URL the caller was handed.
pub async fn get_guarded(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, SsrfError> {
    let parsed = guard_url(url)?;
    client
        .get(parsed)
        .send()
        .await
        .map_err(|source| SsrfError::Request {
            url: url.to_string(),
            source,
        })
}

/// Read a response body, giving up once `max_bytes` have accumulated.
///
/// `Response::bytes()` buffers the whole body with no ceiling, so a hostile or
/// merely enormous URL is a memory-exhaustion vector on every endpoint that
/// fetches one. Streaming with a running total bounds the damage to
/// `max_bytes` per in-flight request.
pub async fn read_body_capped(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, SsrfError> {
    let url = response.url().to_string();
    crate::http::read_body_capped(response, max_bytes)
        .await
        .map_err(|source| SsrfError::Response { url, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("test IP must parse")
    }

    #[test]
    fn blocks_loopback_private_and_metadata() {
        for text in [
            "127.0.0.1",
            "127.255.255.255",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "100.64.0.1",
            "100.127.255.255",
            "198.18.0.1",
            "198.19.255.255",
            "240.0.0.1",
            "250.0.0.1",
        ] {
            assert!(is_blocked_ip(&ip(text)), "{text} must be blocked");
        }
        assert!(!is_blocked_ip(&ip("100.63.255.255")));
        assert!(!is_blocked_ip(&ip("100.128.0.0")));
        assert!(!is_blocked_ip(&ip("8.8.8.8")));
        assert!(!is_blocked_ip(&ip("1.1.1.1")));
        assert!(!is_blocked_ip(&ip("208.80.154.224")));
    }

    #[test]
    fn blocks_local_and_mapped_v6() {
        for text in [
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd00::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(is_blocked_ip(&ip(text)), "{text} must be blocked");
        }
        assert!(!is_blocked_ip(&ip("2001:4860:4860::8888")));
        assert!(!is_blocked_ip(&ip("2620:0:861:ed1a::1")));
    }

    #[test]
    fn guard_url_rejects_literals_and_schemes() {
        for url in [
            "http://127.0.0.1/",
            "http://127.0.0.1:3306/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/admin",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "https://0.0.0.0/",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "file:///etc/passwd",
            "ftp://example.com/x",
            "gopher://x/",
            "not a url",
            "",
        ] {
            assert!(guard_url(url).is_err(), "{url} must be refused");
        }
        for url in [
            "https://example.com/data.csv",
            "http://example.com:8080/odd-port",
            "https://8.8.8.8/",
            "https://[2001:4860:4860::8888]/",
        ] {
            assert!(guard_url(url).is_ok(), "{url} must be allowed");
        }
        // Hostnames are the resolver's responsibility.
        assert!(guard_url("http://localhost/").is_ok());
        assert!(guard_url("http://some-internal-name/").is_ok());
    }

    #[tokio::test]
    async fn resolver_refuses_localhost() {
        let r = PublicOnlyResolver;
        let name: Name = "localhost".parse().expect("valid hostname");
        let res = r.resolve(name).await;
        assert!(res.is_err(), "localhost must be refused");
        let err = res.err().unwrap();
        assert!(err.to_string().contains("SSRF guard"), "got: {err}");
    }

    #[tokio::test]
    async fn clients_build_for_both_targets() {
        assert!(client_for(Target::Untrusted, "test/1.0").is_ok());
        assert!(client_for(Target::FixedEndpoint, "test/1.0").is_ok());
    }

    #[tokio::test]
    async fn get_guarded_refuses_loopback_literal() {
        use wiremock::matchers::method;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("secret"))
            .mount(&server)
            .await;
        let guarded = client_for(Target::Untrusted, "test/1.0").unwrap();
        let plain = client_for(Target::FixedEndpoint, "test/1.0").unwrap();
        assert!(plain.get(server.uri()).send().await.is_ok());

        let err = get_guarded(&guarded, &server.uri())
            .await
            .expect_err("loopback literal must be refused")
            .to_string();
        assert!(err.contains("blocked address"), "got: {err}");
    }

    #[tokio::test]
    async fn read_body_capped_enforces_the_limit() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 4096]))
            .mount(&server)
            .await;
        let client = client_for(Target::FixedEndpoint, "test/1.0").unwrap();
        let resp = client.get(server.uri()).send().await.unwrap();
        let err = read_body_capped(resp, 1024).await.unwrap_err().to_string();
        assert!(err.contains("limit"), "got: {err}");
    }
}
