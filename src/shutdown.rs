//! Waiting for the signal to shut down: SIGTERM (what Kubernetes, and so
//! Toolforge, sends to stop a container) or SIGINT (Ctrl-C).
//!
//! On its own for tools that drain their own work; `server::serve`
//! uses it for axum services.

/// The signal that ended a [`shutdown_signal`] wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    /// SIGINT, or Ctrl-C.
    Interrupt,
    /// SIGTERM, which Kubernetes (and so Toolforge) sends to stop a container.
    Terminate,
}

impl ShutdownSignal {
    /// `"SIGINT"` or `"SIGTERM"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
        }
    }
}

impl std::fmt::Display for ShutdownSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Waits for SIGINT (Ctrl-C) or SIGTERM. A handler that cannot be installed
/// is logged and its signal never arrives; this never resolves without a
/// signal, and never panics.
pub async fn shutdown_signal() -> ShutdownSignal {
    let interrupt = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => ShutdownSignal::Interrupt,
            Err(e) => {
                log::warn!("cannot listen for SIGINT: {e}");
                std::future::pending().await
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
                ShutdownSignal::Terminate
            }
            Err(e) => {
                log::warn!("cannot listen for SIGTERM: {e}");
                std::future::pending().await
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<ShutdownSignal>();

    tokio::select! {
        signal = interrupt => signal,
        signal = terminate => signal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_signal_names() {
        assert_eq!(ShutdownSignal::Interrupt.to_string(), "SIGINT");
        assert_eq!(ShutdownSignal::Terminate.as_str(), "SIGTERM");
    }
}
