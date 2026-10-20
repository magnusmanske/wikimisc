//! Inactivity watchdog that kills the current process when no activity has
//! been observed for `max_seconds`.
//!
//! Intended for Toolforge web services that should self-terminate when idle:
//! call [`Seppuku::arm`] once at startup, then call [`Seppuku::alive`] on each
//! request. If no `alive()` arrives within the configured window, the timer
//! task calls `std::process::exit(0)` — after telling the dashboard why, when
//! [`crate::telemetry`] is running.
//!
//! Where "alive" is better asked than announced — a job runner that is stuck
//! when jobs are queued but none has made progress lately, a bot whose
//! database connection has gone — [`Seppuku::watch`] runs a probe at an
//! interval instead and ends the process the same way once it reports
//! [`Verdict::Dead`] often enough in a row.

use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// Lock one of the watchdog's mutexes, recovering from a poisoned lock.
///
/// A panic anywhere else in the process must not turn every subsequent
/// `alive()` call into a panic of its own — that would take down a web service
/// on its next request. The guarded values are an `Instant` and two `bool`s;
/// none can be left logically inconsistent by a panic part-way through an
/// assignment, so reusing the poisoned value is both safe and correct.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a [`Seppuku::watch`] probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// Working; resets the count of consecutive [`Verdict::Dead`]s.
    Alive,
    /// Could not tell (e.g. the check itself failed); changes nothing.
    Unknown,
    /// Not working. The reason is shown publicly on the telemetry dashboard,
    /// so it must not identify anyone (see [`crate::telemetry`]).
    Dead(String),
}

#[derive(Debug, Clone)]
pub struct Seppuku {
    max_seconds: u64,
    last_activity: Arc<Mutex<Instant>>,
    armed: Arc<Mutex<bool>>,
    timer_running: Arc<Mutex<bool>>,
}

/// Kills the process (=app) if no activity for a while.
impl Seppuku {
    /// Creates a new instance, unarmed (not active).
    /// Use `arm` to start it.
    pub fn new(max_seconds: u64) -> Self {
        Self {
            max_seconds,
            last_activity: Arc::new(Mutex::new(Instant::now())),
            armed: Arc::new(Mutex::new(false)),
            timer_running: Arc::new(Mutex::new(false)),
        }
    }

    /// Arms the seppuku timer.
    ///
    /// Must be called from within a tokio runtime, which runs the timer.
    /// Outside one, it prints a warning to stderr and stays unarmed, so the
    /// process is never killed; a later call from within a runtime arms it.
    pub fn arm(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            eprintln!("Seppuku not armed: arm() was called outside a tokio runtime");
            return;
        };
        {
            let mut armed = lock(&self.armed);
            if *armed {
                return;
            }
            self.alive();
            *armed = true;
        }
        self.start_timer(&runtime);
    }

    /// Disarms the seppuku timer.
    pub fn disarm(&self) {
        *lock(&self.armed) = false;
    }

    /// Updates the last activity timestamp.
    /// Call this from your client code to indicate activity.
    pub fn alive(&self) {
        *lock(&self.last_activity) = Instant::now();
    }

    /// Starts the seppuku timer.
    ///
    /// On each iteration the timer sleeps until the *deadline* (the moment that
    /// is `max_seconds` after the last recorded activity), rather than always
    /// sleeping for the full `max_seconds`. Without this, an `alive()` call
    /// late in the window could push the actual fire time out to nearly
    /// `2 * max_seconds` past the last activity.
    fn start_timer(&self, runtime: &tokio::runtime::Handle) {
        let mut timer_running = lock(&self.timer_running);
        if *timer_running {
            // Already running
            return;
        }
        let max = Duration::from_secs(self.max_seconds);
        let max_seconds = self.max_seconds;
        let last_activity = self.last_activity.clone();
        let armed = self.armed.clone();
        *timer_running = true;
        runtime.spawn(async move {
            loop {
                let elapsed = lock(&last_activity).elapsed();
                if *lock(&armed) && elapsed >= max {
                    commit(format!("No activity for {max_seconds} seconds")).await;
                }
                // Sleep just until the deadline. A fresh `alive()` call will
                // simply mean we wake up early and loop without firing.
                let remaining = max.checked_sub(elapsed).unwrap_or(max);
                tokio::time::sleep(remaining).await;
            }
        });
    }
}

impl Seppuku {
    /// Runs `probe` every `interval` and ends the process once it has
    /// returned [`Verdict::Dead`] `failures_needed` times in a row (at least
    /// once): the exit, and the telemetry push before it, are those of the
    /// inactivity watchdog, with the probe's reason. The first probe runs
    /// after one `interval`.
    ///
    /// Like [`Self::arm`], this needs a tokio runtime; outside one it prints
    /// a warning to stderr and watches nothing. A probe that panics counts as
    /// [`Verdict::Unknown`].
    pub fn watch<F, Fut>(interval: Duration, failures_needed: u32, probe: F)
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = Verdict> + Send + 'static,
    {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            eprintln!("Seppuku watch not started: called outside a tokio runtime");
            return;
        };
        runtime.spawn(async move {
            let mut deaths = Deaths::new(failures_needed);
            loop {
                tokio::time::sleep(interval).await;
                // A separate task, so a panicking probe does not end the watch.
                let verdict = tokio::spawn(probe()).await.unwrap_or(Verdict::Unknown);
                if let Some(reason) = deaths.record(verdict) {
                    commit(reason).await;
                }
            }
        });
    }
}

/// Counts consecutive [`Verdict::Dead`]s.
#[derive(Debug)]
struct Deaths {
    needed: u32,
    seen: u32,
}

impl Deaths {
    fn new(needed: u32) -> Self {
        Self {
            needed: needed.max(1),
            seen: 0,
        }
    }

    /// The reason to die, once enough deaths in a row have been seen.
    fn record(&mut self, verdict: Verdict) -> Option<String> {
        match verdict {
            Verdict::Alive => self.seen = 0,
            Verdict::Unknown => {}
            Verdict::Dead(reason) => {
                self.seen = self.seen.saturating_add(1);
                if self.seen >= self.needed {
                    return Some(reason);
                }
            }
        }
        None
    }
}

/// Ends the process, telling the dashboard why first.
async fn commit(reason: String) {
    println!("Committing seppuku: {reason}");
    #[cfg(feature = "telemetry")]
    crate::telemetry::shutdown(crate::telemetry::ExitReason::Watchdog(reason)).await;
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seppuku_new() {
        let seppuku = Seppuku::new(60);
        assert_eq!(seppuku.max_seconds, 60);
        assert!(!*seppuku.armed.lock().unwrap());
        assert!(!*seppuku.timer_running.lock().unwrap());
    }

    #[test]
    fn test_seppuku_disarm() {
        let seppuku = Seppuku::new(60);
        assert!(!*seppuku.armed.lock().unwrap());

        // Manually set armed to true without calling arm() which requires tokio runtime
        *seppuku.armed.lock().unwrap() = true;
        assert!(*seppuku.armed.lock().unwrap());

        seppuku.disarm();
        assert!(!*seppuku.armed.lock().unwrap());
    }

    #[test]
    fn test_seppuku_alive_updates_timestamp() {
        let seppuku = Seppuku::new(60);
        let before = *seppuku.last_activity.lock().unwrap();

        std::thread::sleep(Duration::from_millis(10));
        seppuku.alive();

        let after = *seppuku.last_activity.lock().unwrap();
        assert!(after > before);
    }

    #[test]
    fn test_arm_outside_a_runtime_does_nothing() {
        // Must not panic (tokio::spawn would), and must not leave `armed` set
        // without a timer, which would make a later arm() a no-op.
        let seppuku = Seppuku::new(60);
        seppuku.arm();
        assert!(!*seppuku.armed.lock().unwrap());
        assert!(!*seppuku.timer_running.lock().unwrap());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async { seppuku.arm() });
        assert!(*seppuku.armed.lock().unwrap());
        assert!(*seppuku.timer_running.lock().unwrap());
    }

    #[tokio::test]
    async fn test_seppuku_arm_sets_flags() {
        // arm() must set both `armed` and `timer_running` to true. Use a long
        // timeout so the spawned task does not actually fire during the test.
        let seppuku = Seppuku::new(3600);
        seppuku.arm();
        assert!(*seppuku.armed.lock().unwrap());
        assert!(*seppuku.timer_running.lock().unwrap());
    }

    #[tokio::test]
    async fn test_seppuku_arm_is_idempotent() {
        // Calling arm() twice must not panic, restart the timer, or change state.
        let seppuku = Seppuku::new(3600);
        seppuku.arm();
        // Snapshot state, then re-arm.
        let armed_before = *seppuku.armed.lock().unwrap();
        let timer_before = *seppuku.timer_running.lock().unwrap();
        seppuku.arm();
        assert_eq!(*seppuku.armed.lock().unwrap(), armed_before);
        assert_eq!(*seppuku.timer_running.lock().unwrap(), timer_before);
    }

    #[tokio::test]
    async fn test_seppuku_arm_then_disarm() {
        // After arm() + disarm() the armed flag must clear. The timer task
        // remains running but is gated by the armed flag, so disarm() prevents
        // the process from exiting on inactivity.
        let seppuku = Seppuku::new(3600);
        seppuku.arm();
        seppuku.disarm();
        assert!(!*seppuku.armed.lock().unwrap());
    }

    #[tokio::test]
    async fn test_rearming_after_disarm_reuses_the_running_timer() {
        // arm() short-circuits on the `armed` flag, so the only way to reach
        // start_timer() a second time is to disarm first. When that happens the
        // still-running timer task must be reused rather than a second one
        // spawned, which is what start_timer's `timer_running` guard is for.
        let seppuku = Seppuku::new(3600);
        seppuku.arm();
        seppuku.disarm();

        seppuku.arm();

        assert!(*seppuku.armed.lock().unwrap());
        assert!(
            *seppuku.timer_running.lock().unwrap(),
            "the original timer task must still be marked as running"
        );
    }

    #[tokio::test]
    async fn test_armed_timer_does_not_fire_before_the_deadline() {
        // Let the spawned watchdog run a full iteration: with a long window the
        // elapsed time is below the deadline, so it must compute a remaining
        // sleep and loop instead of exiting the process. (The exit branch itself
        // cannot be covered in-process -- it calls std::process::exit.)
        let seppuku = Seppuku::new(3600);
        seppuku.arm();

        // Yield repeatedly so the spawned task is actually polled.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        // Reaching here at all means the watchdog did not terminate the process.
        assert!(*seppuku.armed.lock().unwrap());
        assert!(*seppuku.timer_running.lock().unwrap());
    }
    #[test]
    fn test_deaths_must_be_consecutive() {
        let dead = || Verdict::Dead("stuck".to_string());
        let mut deaths = Deaths::new(2);
        assert_eq!(deaths.record(dead()), None);
        assert_eq!(deaths.record(Verdict::Alive), None);
        assert_eq!(deaths.record(dead()), None);
        // Unknown neither counts nor resets.
        assert_eq!(deaths.record(Verdict::Unknown), None);
        assert_eq!(deaths.record(dead()), Some("stuck".to_string()));
    }

    #[test]
    fn test_at_least_one_death_is_needed() {
        let mut deaths = Deaths::new(0);
        assert_eq!(deaths.record(Verdict::Alive), None);
        assert_eq!(
            deaths.record(Verdict::Dead("gone".to_string())),
            Some("gone".to_string())
        );
    }

    #[test]
    fn test_watch_outside_a_runtime_does_nothing() {
        Seppuku::watch(Duration::from_millis(1), 1, || async {
            Verdict::Dead("must never fire".to_string())
        });
    }

    #[tokio::test]
    async fn test_watch_keeps_probing_while_alive() {
        let probes = Arc::new(Mutex::new(0u32));
        let counter = probes.clone();
        Seppuku::watch(Duration::from_millis(5), 1, move || {
            let counter = counter.clone();
            async move {
                *lock(&counter) += 1;
                Verdict::Alive
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(*lock(&probes) >= 3);
    }

    #[tokio::test]
    async fn test_panicking_probe_does_not_end_the_watch() {
        let probes = Arc::new(Mutex::new(0u32));
        let counter = probes.clone();
        Seppuku::watch(Duration::from_millis(5), 1, move || {
            let counter = counter.clone();
            async move {
                let count = {
                    let mut count = lock(&counter);
                    *count += 1;
                    *count
                };
                if count == 1 {
                    panic!("intentional probe panic (expected in test output)");
                }
                Verdict::Alive
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            *lock(&probes) >= 2,
            "the watch must outlive a panicking probe"
        );
    }

    #[test]
    fn test_poisoned_mutex_is_recovered_instead_of_panicking() {
        // A panic elsewhere while holding one of the locks must not make every
        // later call panic — `alive()` runs on every request of a web service.
        // NOTE: this test intentionally panics a helper thread, so a
        // "thread panicked" message in the test output here is expected.
        let seppuku = Seppuku::new(60);
        let poisoner = seppuku.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.armed.lock();
            panic!("intentional panic to poison the mutex (expected in test output)");
        })
        .join();

        assert!(
            seppuku.armed.is_poisoned(),
            "the lock must actually be poisoned for this test to mean anything"
        );

        // None of these may panic.
        seppuku.alive();
        seppuku.disarm();
        assert!(!*lock(&seppuku.armed));
        assert!(*lock(&seppuku.last_activity) <= Instant::now());
    }
}
