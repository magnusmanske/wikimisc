//! Running CPU-heavy work off the async executor.
//!
//! Parsing a large SPARQL result or a `wbgetentities` answer takes long enough
//! that doing it inline stalls every other task on the same executor thread.
//! These paths hand the parse to a blocking thread instead, when there is a
//! tokio runtime to hand it to.

use tokio::runtime::Handle;

/// Runs `work` on a blocking thread when called inside a tokio runtime, and
/// inline otherwise.
///
/// Outside a runtime there is nowhere to send it, and `spawn_blocking` would
/// panic, so the work runs inline: slow, but never a process abort. The same
/// applies to callers on another executor, such as `futures::executor`.
///
/// Returns `None` if the blocking task did not finish — it panicked, or the
/// runtime is shutting down. Callers turn that into their own error: it is a
/// bug, never something the response data can cause.
pub(crate) async fn run<T, F>(work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    match Handle::try_current() {
        Ok(handle) => handle.spawn_blocking(work).await.ok(),
        Err(_) => Some(work()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    #[test]
    fn runs_inline_when_there_is_no_runtime() {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = pin!(run(|| 42));
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => assert_eq!(value, Some(42)),
            Poll::Pending => panic!("without a runtime the work must run inline"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runs_on_a_blocking_thread_inside_a_runtime() {
        let caller = std::thread::current().id();
        let worker = run(move || std::thread::current().id()).await;
        assert!(worker.is_some());
        assert_ne!(worker, Some(caller));
    }
}
