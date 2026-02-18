use std::sync::LazyLock;

static IS_ON_TOOLFORGE: LazyLock<bool> =
    LazyLock::new(|| std::path::Path::new("/etc/wmcs-project").exists());

pub struct ToolforgeApp {}

impl ToolforgeApp {
    pub fn is_on_toolforge() -> bool {
        *IS_ON_TOOLFORGE
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
}
