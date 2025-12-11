pub struct ToolforgeApp {}

impl ToolforgeApp {
    pub fn is_on_toolforge() -> bool {
        lazy_static! {
            static ref IS_ON_TOOLFORGE: bool = std::path::Path::new("/etc/wmcs-project").exists();
        }
        IS_ON_TOOLFORGE.to_owned()
    }
}
