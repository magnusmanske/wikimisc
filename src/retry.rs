//! When to try a failed operation again, and how long to wait first.
//!
//! A [`RetryPolicy`] retries only failures that can go away on their own, as
//! the error says through [`Retryable`] (or a predicate, with
//! [`RetryPolicy::run_if`]). The wait doubles with each retry, starting from
//! the initial backoff and capped at the maximum wait; a server that asks for
//! a longer wait (`Retry-After`) gets at least that, and if it asks for more
//! than the maximum, the error is returned instead — retrying sooner would
//! only be refused again — unless [`RetryPolicy::clamp_retry_after`] is set.
//!
//! The policy is generic over the error type. [`crate::sparql_retry`] uses it
//! for SPARQL queries, [`crate::http`] (feature `http`) for plain HTTP
//! requests, and anything else can implement [`Retryable`]:
//!
//! ```rust,no_run
//! use std::time::Duration;
//! use wikimisc::retry::{Retry, RetryPolicy, Retryable};
//!
//! #[derive(Debug)]
//! enum FetchError {
//!     Busy,
//!     NotFound,
//! }
//!
//! impl Retryable for FetchError {
//!     fn is_retryable(&self) -> bool {
//!         matches!(self, Self::Busy)
//!     }
//! }
//!
//! async fn fetch() -> Result<String, FetchError> {
//!     Err(FetchError::Busy)
//! }
//!
//! async fn fetch_patiently() -> Result<String, FetchError> {
//!     RetryPolicy::new(3)
//!         .with_initial_backoff(Duration::from_millis(250))
//!         .on_retry(|retry: &Retry<'_, FetchError>| {
//!             eprintln!("{}: {:?}, retry {} in {:?}", retry.endpoint, retry.error, retry.retry, retry.wait)
//!         })
//!         .run("https://example.org/data", fetch)
//!         .await
//! }
//! ```

use std::collections::hash_map::RandomState;
use std::fmt;
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::Duration;

/// The wait before the first retry, unless set otherwise.
pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// The longest single wait, unless set otherwise.
pub const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(60);

/// An error that knows whether the operation that failed with it may succeed
/// if tried again.
pub trait Retryable {
    /// Whether the failure may go away on its own: rate limiting, a server
    /// that is briefly unavailable, a connection that broke.
    fn is_retryable(&self) -> bool;

    /// The wait the other side asked for (`Retry-After`), if any.
    fn retry_after(&self) -> Option<Duration> {
        None
    }
}

/// One retry about to happen, as passed to [`RetryPolicy::on_retry`].
#[derive(Debug)]
#[non_exhaustive]
pub struct Retry<'a, E> {
    /// What is being retried: the URL, or the label passed to
    /// [`RetryPolicy::run`].
    pub endpoint: &'a str,
    /// Why the attempt failed.
    pub error: &'a E,
    /// Which retry this is, from 1.
    pub retry: u32,
    /// How long the policy waits before it.
    pub wait: Duration,
}

/// Shared, so that a policy and whatever holds it stay cheap to clone.
type RetryHook<E> = Arc<dyn Fn(&Retry<'_, E>) + Send + Sync>;

/// How often, and after what wait, a failed operation is retried.
///
/// The default retries nothing. See the [module documentation](self) for the
/// waits.
pub struct RetryPolicy<E> {
    retries: u32,
    initial_backoff: Duration,
    max_wait: Duration,
    jitter: bool,
    clamp_retry_after: bool,
    on_retry: Option<RetryHook<E>>,
}

// Hand-written: deriving would require `E: Clone` / `E: Debug`, which the
// policy does not need, and a closure has no `Debug`.
impl<E> Clone for RetryPolicy<E> {
    fn clone(&self) -> Self {
        Self {
            retries: self.retries,
            initial_backoff: self.initial_backoff,
            max_wait: self.max_wait,
            jitter: self.jitter,
            clamp_retry_after: self.clamp_retry_after,
            on_retry: self.on_retry.clone(),
        }
    }
}

impl<E> fmt::Debug for RetryPolicy<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("retries", &self.retries)
            .field("initial_backoff", &self.initial_backoff)
            .field("max_wait", &self.max_wait)
            .field("jitter", &self.jitter)
            .field("clamp_retry_after", &self.clamp_retry_after)
            .field("on_retry", &self.on_retry.as_ref().map(|_| "Fn(&Retry)"))
            .finish()
    }
}

impl<E> Default for RetryPolicy<E> {
    /// No retries.
    fn default() -> Self {
        Self::new(0)
    }
}

impl<E> RetryPolicy<E> {
    /// Up to `retries` retries after the first attempt, waiting 1 s, 2 s,
    /// 4 s, … but never more than 60 s.
    pub fn new(retries: u32) -> Self {
        Self {
            retries,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_wait: DEFAULT_MAX_WAIT,
            jitter: false,
            clamp_retry_after: false,
            on_retry: None,
        }
    }

    /// The wait before the first retry.
    #[must_use]
    pub fn with_initial_backoff(mut self, backoff: Duration) -> Self {
        self.initial_backoff = backoff;
        self
    }

    /// The longest single wait, including one the other side asks for.
    #[must_use]
    pub fn with_max_wait(mut self, max_wait: Duration) -> Self {
        self.max_wait = max_wait;
        self
    }

    /// Adds a random extra wait of up to half the backoff, so that many
    /// clients failing at once do not all retry at the same moment.
    #[must_use]
    pub fn with_jitter(mut self) -> Self {
        self.jitter = true;
        self
    }

    /// Waits the maximum wait when asked to wait longer, instead of giving
    /// up.
    #[must_use]
    pub fn clamp_retry_after(mut self) -> Self {
        self.clamp_retry_after = true;
        self
    }

    /// Calls `hook` before each retry, e.g. to log it. A plain function
    /// works, as does a closure that captures state such as a counter.
    #[must_use]
    pub fn on_retry<F>(mut self, hook: F) -> Self
    where
        F: Fn(&Retry<'_, E>) + Send + Sync + 'static,
    {
        self.on_retry = Some(Arc::new(hook));
        self
    }

    /// The most retries after the first attempt.
    pub fn retries(&self) -> u32 {
        self.retries
    }

    /// Runs `op`, and again as long as `should_retry` says the error is worth
    /// retrying and retries are left. A `Retry-After` cannot be known here, so
    /// the waits are the backoff alone.
    pub async fn run_if<T, F, Fut, P>(&self, endpoint: &str, should_retry: P, op: F) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        P: Fn(&E) -> bool,
    {
        self.run_classified(endpoint, |error| should_retry(error).then_some(None), op)
            .await
    }

    /// The wait before the next retry when `retries_done` retries have been
    /// made and the other side asked for `asked`; `None` if there is to be no
    /// retry.
    fn wait(&self, retries_done: u32, asked: Option<Duration>) -> Option<Duration> {
        if retries_done >= self.retries {
            return None;
        }
        let backoff = self
            .initial_backoff
            .saturating_mul(2u32.saturating_pow(retries_done))
            .min(self.max_wait);
        let backoff = if self.jitter {
            backoff.saturating_add(random_up_to(backoff / 2))
        } else {
            backoff
        };
        match asked {
            Some(asked) if asked > self.max_wait => {
                self.clamp_retry_after.then(|| self.max_wait.max(backoff))
            }
            Some(asked) => Some(backoff.max(asked)),
            None => Some(backoff),
        }
    }

    /// The retry loop. `classify` returns `None` for an error not to be
    /// retried, else the wait the other side asked for, if any.
    async fn run_classified<T, F, Fut, C>(
        &self,
        endpoint: &str,
        classify: C,
        mut op: F,
    ) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        C: Fn(&E) -> Option<Option<Duration>>,
    {
        let mut retries_done = 0;
        loop {
            let error = match op().await {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            let Some(wait) = classify(&error).and_then(|asked| self.wait(retries_done, asked))
            else {
                return Err(error);
            };
            retries_done = retries_done.saturating_add(1);
            self.notify(&Retry {
                endpoint,
                error: &error,
                retry: retries_done,
                wait,
            });
            tokio::time::sleep(wait).await;
        }
    }

    pub(crate) fn notify(&self, retry: &Retry<'_, E>) {
        if let Some(hook) = &self.on_retry {
            hook(retry);
        }
    }
}

impl<E: Retryable> RetryPolicy<E> {
    /// The wait before retrying after `error`, when `retries_done` retries
    /// have already been made; `None` if it is not to be retried.
    pub fn delay(&self, error: &E, retries_done: u32) -> Option<Duration> {
        if !error.is_retryable() {
            return None;
        }
        self.wait(retries_done, error.retry_after())
    }

    /// Runs `op`, and again as long as the error is [`Retryable`] and retries
    /// are left, waiting as described in the [module documentation](self) in
    /// between. `endpoint` names what is being tried, for
    /// [`Self::on_retry`]. Needs a tokio runtime (with the time driver).
    pub async fn run<T, F, Fut>(&self, endpoint: &str, op: F) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        self.run_classified(
            endpoint,
            |error| error.is_retryable().then(|| error.retry_after()),
            op,
        )
        .await
    }
}

/// A random duration from zero to `max`. Jitter needs no more randomness than
/// the per-process seeded hasher the standard library already has.
fn random_up_to(max: Duration) -> Duration {
    let max_nanos = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos()),
    );
    Duration::from_nanos(hasher.finish() % max_nanos.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug, PartialEq)]
    enum TestError {
        Transient,
        Permanent,
        Busy(u64),
    }

    impl Retryable for TestError {
        fn is_retryable(&self) -> bool {
            !matches!(self, Self::Permanent)
        }

        fn retry_after(&self) -> Option<Duration> {
            match self {
                Self::Busy(seconds) => Some(Duration::from_secs(*seconds)),
                _ => None,
            }
        }
    }

    fn fast(retries: u32) -> RetryPolicy<TestError> {
        RetryPolicy::new(retries).with_initial_backoff(Duration::from_millis(1))
    }

    #[test]
    fn test_default_never_retries() {
        assert_eq!(
            RetryPolicy::<TestError>::default().delay(&TestError::Transient, 0),
            None
        );
    }

    #[test]
    fn test_backoff_doubles_up_to_the_maximum() {
        let policy = RetryPolicy::new(4).with_max_wait(Duration::from_secs(5));
        let waits: Vec<_> = (0..5)
            .map(|n| policy.delay(&TestError::Transient, n))
            .collect();
        let expected: Vec<_> = [1, 2, 4, 5]
            .map(|s| Some(Duration::from_secs(s)))
            .into_iter()
            .chain([None])
            .collect();
        assert_eq!(waits, expected);
    }

    #[test]
    fn test_retry_after_is_a_floor_and_too_long_gives_up() {
        let policy = RetryPolicy::<TestError>::new(3);
        assert_eq!(
            policy.delay(&TestError::Busy(30), 0),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            policy.delay(&TestError::Busy(0), 1),
            Some(Duration::from_secs(2))
        );
        assert_eq!(policy.delay(&TestError::Busy(1000), 0), None);
        assert_eq!(policy.delay(&TestError::Permanent, 0), None);
    }

    #[test]
    fn test_clamped_retry_after_waits_the_maximum() {
        let policy = RetryPolicy::<TestError>::new(3)
            .with_max_wait(Duration::from_secs(60))
            .clamp_retry_after();
        assert_eq!(
            policy.delay(&TestError::Busy(1000), 0),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn test_jitter_adds_at_most_half_the_backoff() {
        let policy = RetryPolicy::<TestError>::new(3).with_jitter();
        for _ in 0..100 {
            let wait = policy.delay(&TestError::Transient, 1).unwrap();
            assert!(wait >= Duration::from_secs(2), "{wait:?}");
            assert!(wait <= Duration::from_secs(3), "{wait:?}");
        }
    }

    #[test]
    fn test_huge_retry_count_does_not_overflow() {
        let policy = RetryPolicy::<TestError>::new(u32::MAX);
        assert_eq!(
            policy.delay(&TestError::Transient, 100),
            Some(DEFAULT_MAX_WAIT)
        );
    }

    #[test]
    fn test_debug_and_clone_need_nothing_from_the_error_type() {
        struct Opaque;
        let policy = RetryPolicy::<Opaque>::new(2).on_retry(|_| {});
        let clone = policy.clone();
        assert_eq!(clone.retries(), 2);
        assert!(format!("{policy:?}").contains("on_retry: Some"));
    }

    #[tokio::test]
    async fn test_run_retries_until_success_and_reports_each_retry() {
        let attempts = AtomicU32::new(0);
        let seen = Arc::new(AtomicU32::new(0));
        let hook_seen = Arc::clone(&seen);
        let policy = fast(3).on_retry(move |retry: &Retry<'_, TestError>| {
            assert_eq!(retry.endpoint, "test");
            assert_eq!(*retry.error, TestError::Transient);
            hook_seen.store(retry.retry, Ordering::SeqCst);
        });
        let result = policy
            .run("test", || async {
                match attempts.fetch_add(1, Ordering::SeqCst) {
                    0 | 1 => Err(TestError::Transient),
                    _ => Ok("done"),
                }
            })
            .await;
        assert_eq!(result, Ok("done"));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_run_returns_a_permanent_error_at_once() {
        let attempts = AtomicU32::new(0);
        let result: Result<(), _> = fast(3)
            .run("test", || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(TestError::Permanent)
            })
            .await;
        assert_eq!(result, Err(TestError::Permanent));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_run_gives_up_after_the_last_retry() {
        let attempts = AtomicU32::new(0);
        let result: Result<(), _> = fast(2)
            .run("test", || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(TestError::Transient)
            })
            .await;
        assert_eq!(result, Err(TestError::Transient));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_run_if_retries_by_predicate_for_any_error_type() {
        let attempts = AtomicU32::new(0);
        let policy = RetryPolicy::<String>::new(5).with_initial_backoff(Duration::from_millis(1));
        let result = policy
            .run_if(
                "test",
                |error: &String| error == "again",
                || async {
                    match attempts.fetch_add(1, Ordering::SeqCst) {
                        0 => Err("again".to_string()),
                        _ => Err("stop".to_string()),
                    }
                },
            )
            .await;
        assert_eq!(result, Err::<(), _>("stop".to_string()));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
