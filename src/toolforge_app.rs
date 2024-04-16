use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct ToolforgeApp {}

impl ToolforgeApp {

    /// Kills the process (=app) if no activity for a while.
    /// Returns a reference to the last activity time that has to be updated by the main process.
    pub fn seppuku(max_seconds: u64) -> Arc<Mutex<Instant>> {
        let ret = Arc::new(Mutex::new(Instant::now()));
        let last_activity = ret.clone();
        tokio::spawn(async move {
            loop {
                if last_activity.lock().unwrap().elapsed().as_secs() > max_seconds {
                    println!("Commiting seppuku after {max_seconds} seconds of inactivity");
                    std::process::exit(0);
                }
                tokio::time::sleep(Duration::from_secs(max_seconds)).await;
            }
        });
        ret
    }

}
