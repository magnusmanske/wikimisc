//! Inactivity watchdog that kills the current process when no activity has
//! been observed for `max_seconds`.
//!
//! Intended for Toolforge web services that should self-terminate when idle:
//! call [`Seppuku::arm`] once at startup, then call [`Seppuku::alive`] on each
//! request. If no `alive()` arrives within the configured window, the timer
//! task calls `std::process::exit(0)`.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

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
    pub fn arm(&self) {
        {
            let mut armed = self.armed.lock().unwrap();
            if *armed {
                return;
            }
            self.alive();
            *armed = true;
        }
        self.start_timer();
    }

    /// Disarms the seppuku timer.
    pub fn disarm(&self) {
        *self.armed.lock().unwrap() = false;
    }

    /// Updates the last activity timestamp.
    /// Call this from your client code to indicate activity.
    pub fn alive(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    /// Starts the seppuku timer.
    ///
    /// On each iteration the timer sleeps until the *deadline* (the moment that
    /// is `max_seconds` after the last recorded activity), rather than always
    /// sleeping for the full `max_seconds`. Without this, an `alive()` call
    /// late in the window could push the actual fire time out to nearly
    /// `2 * max_seconds` past the last activity.
    fn start_timer(&self) {
        let mut timer_running = self.timer_running.lock().unwrap();
        if *timer_running {
            // Already running
            return;
        }
        let max = Duration::from_secs(self.max_seconds);
        let max_seconds = self.max_seconds;
        let last_activity = self.last_activity.clone();
        let armed = self.armed.clone();
        *timer_running = true;
        tokio::spawn(async move {
            loop {
                let elapsed = last_activity.lock().unwrap().elapsed();
                if *armed.lock().unwrap() && elapsed >= max {
                    println!("Committing seppuku after {max_seconds} seconds of inactivity");
                    std::process::exit(0);
                }
                // Sleep just until the deadline. A fresh `alive()` call will
                // simply mean we wake up early and loop without firing.
                let remaining = max.checked_sub(elapsed).unwrap_or(max);
                tokio::time::sleep(remaining).await;
            }
        });
    }
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
}
