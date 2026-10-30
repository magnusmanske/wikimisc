//! Small pieces shared by the tools' web layers: request parameters read the
//! way the old PHP tools read them (`$_REQUEST`), and the allowlist that
//! keeps static-file serving away from source files and secrets.

pub mod params;
pub mod static_files;
