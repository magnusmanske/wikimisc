//! When [`SparqlClient`](crate::sparql_client::SparqlClient) tries a failed
//! query again, and how long it waits first.
//!
//! Only failures that can go away on their own are retried: rate limiting
//! (after the wait the endpoint asks for), a 5xx other than a timeout, and a
//! request that failed on the way. A timeout, a malformed query (4xx) or an
//! oversized result fails the same way every time, so it is returned at once.
//!
//! These are the generic [`crate::retry`] types, fixed to [`SparqlError`]; see
//! there for the waits and the builder methods.

use crate::sparql_client::SparqlError;

/// How often, and after what wait, a failed query is retried; see
/// [`crate::retry::RetryPolicy`].
pub type RetryPolicy = crate::retry::RetryPolicy<SparqlError>;

/// One retry about to happen, as passed to [`RetryPolicy::on_retry`].
pub type Retry<'a> = crate::retry::Retry<'a, SparqlError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry::DEFAULT_MAX_WAIT;
    use std::sync::Arc;
    use std::time::Duration;

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
    fn test_on_retry_accepts_a_capturing_closure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let policy = RetryPolicy::new(3).on_retry(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let error = endpoint(503);
        let retry = Retry {
            endpoint: "http://localhost/sparql",
            error: &error,
            retry: 1,
            wait: Duration::ZERO,
        };
        policy.notify(&retry);
        // A clone shares the hook.
        policy.clone().notify(&retry);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(format!("{policy:?}").contains("on_retry: Some"));
        assert!(format!("{:?}", RetryPolicy::new(1)).contains("on_retry: None"));
    }

    #[test]
    fn test_huge_retry_count_does_not_overflow() {
        let policy = RetryPolicy::new(u32::MAX);
        assert_eq!(policy.delay(&endpoint(503), 100), Some(DEFAULT_MAX_WAIT));
    }
}
