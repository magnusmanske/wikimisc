//! Detect whether the current process is running inside Toolforge.
//!
//! Two signals are checked, and either one is enough:
//!
//! * `TOOL_TOOLFORGE_API_URL`, one of the `TOOL_*` variables Toolforge always
//!   sets for a job or web service. This is the
//!   [documented way](https://wikitech.wikimedia.org/wiki/Help:Toolforge/Envvars#How_to_know_if_my_code_is_running_inside_toolforge?)
//!   to detect Toolforge from code, and the only one that works in a
//!   [build-service](https://wikitech.wikimedia.org/wiki/Help:Toolforge/Building_container_images)
//!   container, which need not have the host's files.
//! * `/etc/wmcs-project`, present on Wikimedia Cloud Services hosts. Kept as a
//!   fallback for contexts without the `TOOL_*` variables, such as an
//!   interactive shell on a bastion. Note this is really a "somewhere on WMCS"
//!   signal: it is also there on a plain Cloud VPS instance.
//!
//! The result is cached in a `LazyLock`, so subsequent calls are free — and a
//! change to the environment after the first call is not picked up.

use std::path::Path;
use std::sync::LazyLock;

/// Environment variable Toolforge guarantees to be set for every job and web
/// service it runs.
const TOOLFORGE_ENV_MARKER: &str = "TOOL_TOOLFORGE_API_URL";

/// Marker file found on Wikimedia Cloud Services hosts.
const WMCS_MARKER_FILE: &str = "/etc/wmcs-project";

static IS_ON_TOOLFORGE: LazyLock<bool> = LazyLock::new(|| {
    ToolforgeApp::detect(
        std::env::var_os(TOOLFORGE_ENV_MARKER).is_some(),
        Path::new(WMCS_MARKER_FILE),
    )
});

pub struct ToolforgeApp {}

impl ToolforgeApp {
    /// Whether this process appears to be running inside Toolforge. See the
    /// [module documentation](self) for what is checked.
    pub fn is_on_toolforge() -> bool {
        *IS_ON_TOOLFORGE
    }

    /// The detection itself, taking its two inputs as arguments so it can be
    /// tested without touching the real environment.
    fn detect(env_marker_set: bool, marker_file: &Path) -> bool {
        env_marker_set || marker_file.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_on_toolforge_consistent() {
        // The result should be consistent across calls
        let result1 = ToolforgeApp::is_on_toolforge();
        let result2 = ToolforgeApp::is_on_toolforge();
        assert_eq!(result1, result2);
    }

    #[test]
    fn test_detect_uses_either_signal() {
        let missing = Path::new("/nonexistent/wmcs-project");
        // The env var alone is enough: a build-service container has the
        // `TOOL_*` variables but not the host's marker file.
        assert!(ToolforgeApp::detect(true, missing));
        // The marker file alone is enough too, e.g. an interactive shell.
        let present = Path::new(WMCS_MARKER_FILE);
        assert_eq!(ToolforgeApp::detect(false, present), present.exists());
        // Neither signal means we are somewhere else entirely.
        assert!(!ToolforgeApp::detect(false, missing));
    }
}
