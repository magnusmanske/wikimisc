pub struct ToolforgeApp {}

impl ToolforgeApp {
    pub fn is_on_toolforge() -> bool {
        lazy_static! {
            static ref IS_ON_TOOLFORGE: bool = std::path::Path::new("/etc/wmcs-project").exists();
        }
        IS_ON_TOOLFORGE.to_owned()
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
