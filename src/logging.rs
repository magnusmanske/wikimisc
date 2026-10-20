//! One-call logging setup for tools: a [`tracing`](https://docs.rs/tracing)
//! subscriber writing to stderr (or stdout), filtered by an environment
//! variable (or an explicit filter, e.g. from a command-line flag), with
//! the `log` crate bridged into it (so `log::info!` from dependencies, and
//! from wikimisc itself, shows up too).
//!
//! ```no_run
//! wikimisc::logging::init(); // RUST_LOG, else "info"
//!
//! // or, configured:
//! wikimisc::logging::Logging::new()
//!     .env_var("GREEKDATA_LOG")
//!     .json(true)
//!     .init();
//! ```
//!
//! Only the first subscriber of a process takes effect; a later `init` is a
//! no-op that returns `false` (handy in tests).

use std::io::IsTerminal;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::EnvFilter;

/// Configures the logging set up by [`Logging::init`].
#[derive(Debug, Clone)]
pub struct Logging {
    env_var: String,
    filter: Option<String>,
    default_filter: String,
    json: bool,
    with_target: bool,
    ansi: Option<bool>,
    stdout: bool,
}

impl Default for Logging {
    fn default() -> Self {
        Self::new()
    }
}

impl Logging {
    /// Filter from `RUST_LOG`, else `info`; plain text to stderr with the
    /// target (the module that logged) shown.
    pub fn new() -> Self {
        Self {
            env_var: "RUST_LOG".to_string(),
            filter: None,
            default_filter: "info".to_string(),
            json: false,
            with_target: true,
            ansi: None,
            stdout: false,
        }
    }

    /// The environment variable holding the filter, in
    /// [`EnvFilter`] syntax (`info`, `warn,my_tool=debug`, …).
    pub fn env_var(mut self, name: impl Into<String>) -> Self {
        self.env_var = name.into();
        self
    }

    /// A filter that takes precedence over the environment variable, e.g.
    /// from a command-line flag. An invalid one is ignored.
    pub fn filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// The filter used when neither [`Self::filter`] nor the environment
    /// variable gives a valid one.
    pub fn default_filter(mut self, filter: impl Into<String>) -> Self {
        self.default_filter = filter.into();
        self
    }

    /// One JSON object per line instead of plain text.
    pub fn json(mut self, json: bool) -> Self {
        self.json = json;
        self
    }

    /// Whether each line shows the module that logged it. On by default.
    pub fn with_target(mut self, with_target: bool) -> Self {
        self.with_target = with_target;
        self
    }

    /// Writes to stdout instead of stderr, for tools whose process manager
    /// collects logs from there.
    pub fn stdout(mut self) -> Self {
        self.stdout = true;
        self
    }

    /// Whether to colour the output with ANSI escape codes. By default only
    /// when the output is a terminal, so log files stay free of escape codes.
    pub fn ansi(mut self, ansi: bool) -> Self {
        self.ansi = Some(ansi);
        self
    }

    /// Installs the subscriber, and the `log` bridge with it. Returns `false`
    /// if a subscriber was already installed, in which case nothing changes.
    pub fn init(self) -> bool {
        let (writer, is_terminal) = if self.stdout {
            (
                BoxMakeWriter::new(std::io::stdout),
                std::io::stdout().is_terminal(),
            )
        } else {
            (
                BoxMakeWriter::new(std::io::stderr),
                std::io::stderr().is_terminal(),
            )
        };
        let builder = tracing_subscriber::fmt()
            .with_env_filter(self.env_filter())
            .with_target(self.with_target)
            .with_ansi(self.ansi.unwrap_or(is_terminal))
            .with_writer(writer);
        let installed = if self.json {
            builder.json().try_init().is_ok()
        } else {
            builder.try_init().is_ok()
        };
        // `try_init` already sets up the bridge through tracing-subscriber's
        // `tracing-log` feature when that is enabled; installing it again
        // then fails harmlessly. Only after our subscriber went in, so a
        // tool's own `log` logger is never displaced.
        if installed {
            let _ = tracing_log::LogTracer::init();
        }
        installed
    }

    fn env_filter(&self) -> EnvFilter {
        self.filter
            .as_ref()
            .and_then(|filter| EnvFilter::try_new(filter).ok())
            .or_else(|| {
                std::env::var(&self.env_var)
                    .ok()
                    .and_then(|value| EnvFilter::try_new(value).ok())
            })
            .or_else(|| EnvFilter::try_new(&self.default_filter).ok())
            .unwrap_or_else(|| EnvFilter::new("info"))
    }
}

/// [`Logging::new`]`.init()`: `RUST_LOG`, else `info`, to stderr.
pub fn init() -> bool {
    Logging::new().init()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_falls_back_to_the_default() {
        let logging = Logging::new()
            .env_var("WIKIMISC_TEST_UNSET_LOG_VARIABLE")
            .default_filter("warn");
        assert_eq!(logging.env_filter().to_string(), "warn");

        let invalid_default = Logging::new()
            .env_var("WIKIMISC_TEST_UNSET_LOG_VARIABLE")
            .default_filter("=[not a filter");
        assert_eq!(invalid_default.env_filter().to_string(), "info");
    }

    #[test]
    fn test_explicit_filter_beats_the_environment() {
        std::env::set_var("WIKIMISC_TEST_LOG_PRECEDENCE", "debug");
        let from_env = Logging::new().env_var("WIKIMISC_TEST_LOG_PRECEDENCE");
        assert_eq!(from_env.env_filter().to_string(), "debug");
        let explicit = from_env.clone().filter("error");
        assert_eq!(explicit.env_filter().to_string(), "error");
        // An invalid explicit filter falls through to the variable.
        let invalid = from_env.filter("=[not a filter");
        assert_eq!(invalid.env_filter().to_string(), "debug");
    }

    #[test]
    fn test_only_the_first_init_installs() {
        // Whichever call came first in this test binary installed it.
        Logging::new().with_target(false).init();
        assert!(!init(), "a second subscriber must not be installed");
    }
}
