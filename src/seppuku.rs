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
        let mut armed = self.armed.lock().unwrap();
        if *armed {
            return;
        }
        self.alive();
        *armed = true;
        self.start_timer();
    }

    /// Disarms the seppuku timer.
    pub fn disarm(&mut self) {
        *self.armed.lock().unwrap() = false;
    }

    /// Updates the last activity timestamp.
    /// Call this from your client code to indicate activity.
    pub fn alive(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    /// Starts the seppuku timer.
    fn start_timer(&self) {
        let mut timer_running = self.timer_running.lock().unwrap();
        if *timer_running {
            // Already running
            return;
        }
        let max_seconds = self.max_seconds;
        let last_activity = self.last_activity.clone();
        let armed = self.armed.clone();
        *timer_running = true;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(max_seconds)).await;
                if *armed.lock().unwrap()
                    && last_activity.lock().unwrap().elapsed().as_secs() > max_seconds
                {
                    println!("Committing seppuku after {max_seconds} seconds of inactivity");
                    std::process::exit(0);
                }
            }
        });
    }
}
