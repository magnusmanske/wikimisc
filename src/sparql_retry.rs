//! When [`SparqlClient`](crate::sparql_client::SparqlClient) tries a failed
//! query again, and how long it waits first.
//!
//! Only failures that can go away on their own are retried: rate limiting
//! (after the wait the endpoint asks for), a 5xx other than a timeout, and a
//! request that failed on the way. A timeout, a malformed query (4xx) or an
//! oversized result fails the same way every time, so it is returned at once.

use crate::sparql_client::SparqlError;
use std::time::Duration;

const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(60);

/// One retry about to happen, as passed to [`RetryPolicy::on_retry`].
#[derive(Debug)]
#[non_exhaustive]
pub struct Retry<'a> {
    /// The endpoint URL.
    pub endpoint: &'a str,
    /// Why the attempt failed.
    pub error: &'a SparqlError,
    /// Which retry this is, from 1.
    pub retry: u32,
    /// How long the client waits before it.
    pub wait: Duration,
}

/// How often, and after what wait, a failed query is retried.
///
/// The wait doubles with each retry, starting from the initial backoff. A
/// rate-limited request waits at least as long as the endpoint's
/// `Retry-After`; if that is longer than the maximum wait, the error is
/// returned instead, since retrying sooner would only be refused again.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    retries: u32,
    initial_backoff: Duration,
    max_wait: Duration,
    on_retry: Option<fn(&Retry<'_>)>,
}

impl Default for RetryPolicy {
    /// No retries.
    fn default() -> Self {
        Self::new(0)
    }
}

impl RetryPolicy {
    /// Up to `retries` retries after the first attempt, waiting 1 s, 2 s,
    /// 4 s, … but never more than 60 s.
    pub fn new(retries: u32) -> Self {
        Self {
            retries,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_wait: DEFAULT_MAX_WAIT,
            on_retry: None,
        }
    }

    /// The wait before the first retry.
    #[must_use]
    pub fn with_initial_backoff(mut self, backoff: Duration) -> Self {
        self.initial_backoff = backoff;
        self
    }

    /// The longest single wait, including one the endpoint asks for.
    #[must_use]
    pub fn with_max_wait(mut self, max_wait: Duration) -> Self {
        self.max_wait = max_wait;
        self
    }

    /// Calls `hook` before each retry, e.g. to log it.
    #[must_use]
    pub fn on_retry(mut self, hook: fn(&Retry<'_>)) -> Self {
        self.on_retry = Some(hook);
        self
    }

    /// The wait before retrying after `error`, when `retries_done` retries
    /// have already been made; `None` if it is not to be retried.
    pub fn delay(&self, error: &SparqlError, retries_done: u32) -> Option<Duration> {
        if retries_done >= self.retries || !error.is_retryable() {
            return None;
        }
        let backoff = self
            .initial_backoff
            .saturating_mul(2u32.saturating_pow(retries_done))
            .min(self.max_wait);
        match error {
            SparqlError::RateLimited {
                retry_after: Some(asked),
                ..
            } if *asked > self.max_wait => None,
            SparqlError::RateLimited {
                retry_after: Some(asked),
                ..
            } => Some(backoff.max(*asked)),
            _ => Some(backoff),
        }
    }

    pub(crate) fn notify(&self, retry: &Retry<'_>) {
        if let Some(hook) = self.on_retry {
            hook(retry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate_limited(retry_after: Option<u64>) -> SparqlError {
        SparqlError::RateLimited {
            retry_after: retry_after.map(Duration::from_secs),
            message: String::new(),
        }
    }

    fn endpoint(status: u16) -> SparqlError {
        SparqlError::Endpoint {
            status,
            message: String::new(),
        }
    }

    #[test]
    fn test_default_never_retries() {
        assert_eq!(RetryPolicy::default().delay(&endpoint(503), 0), None);
    }

    #[test]
    fn test_backoff_doubles_up_to_the_maximum_and_retries_run_out() {
        let policy = RetryPolicy::new(4).with_max_wait(Duration::from_secs(5));
        let waits: Vec<_> = (0..5).map(|n| policy.delay(&endpoint(503), n)).collect();
        assert_eq!(
            waits,
            [1, 2, 4, 5]
                .map(|s| Some(Duration::from_secs(s)))
                .into_iter()
                .chain([None])
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_rate_limit_waits_as_asked_unless_too_long() {
        let policy = RetryPolicy::new(3);
        assert_eq!(
            policy.delay(&rate_limited(Some(30)), 0),
            Some(Duration::from_secs(30))
        );
        // Never sooner than the backoff, even if the endpoint says 0.
        assert_eq!(
            policy.delay(&rate_limited(Some(0)), 1),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            policy.delay(&rate_limited(None), 0),
            Some(Duration::from_secs(1))
        );
        assert_eq!(policy.delay(&rate_limited(Some(1000)), 0), None);
    }

    #[test]
    fn test_only_transient_errors_are_retried() {
        let policy = RetryPolicy::new(3);
        for status in [500, 502, 503] {
            assert!(policy.delay(&endpoint(status), 0).is_some(), "{status}");
        }
        for status in [400, 403, 404] {
            assert_eq!(policy.delay(&endpoint(status), 0), None, "{status}");
        }
        let timeout = SparqlError::Timeout {
            status: 504,
            message: String::new(),
        };
        assert_eq!(policy.delay(&timeout, 0), None);
        let too_large = SparqlError::ResponseTooLarge { limit: 1 };
        assert_eq!(policy.delay(&too_large, 0), None);
    }

    #[test]
    fn test_huge_retry_count_does_not_overflow() {
        let policy = RetryPolicy::new(u32::MAX);
        assert_eq!(policy.delay(&endpoint(503), 100), Some(DEFAULT_MAX_WAIT));
    }
}
