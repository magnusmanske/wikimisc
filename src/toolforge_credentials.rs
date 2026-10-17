//! Credentials for the Toolforge databases.
//!
//! Toolforge hands a tool its database user and password two ways, and
//! [`Credentials::for_service`] tries both in the order the
//! [docs](https://wikitech.wikimedia.org/wiki/Help:Toolforge/Database#Credentials)
//! present them:
//!
//! 1. The `TOOL_REPLICA_USER` / `TOOL_REPLICA_PASSWORD` and `TOOL_TOOLSDB_USER`
//!    / `TOOL_TOOLSDB_PASSWORD` environment variables. These are the only
//!    source available to a tool built into a custom container image, which has
//!    no shared storage.
//! 2. The `replica.my.cnf` file, looked for in `$TOOL_DATA_DIR` first — that is
//!    the tool's NFS home directory, which the docs warn need not be `$HOME` —
//!    and then in `$HOME`.
//!
//! The replicas and ToolsDB currently share one user and password, but
//! [`DbService`] keeps them apart because the docs say that may change.

use crate::toolforge_db::DatabaseError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Name of the credentials file in the tool's home directory.
const MY_CNF_FILE: &str = "replica.my.cnf";

/// One of the two Toolforge database services, which have separate credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DbService {
    /// The Wiki Replicas, holding MediaWiki data.
    Replicas,
    /// ToolsDB, holding the tool's own databases.
    ToolsDb,
}

impl DbService {
    /// The environment variables Toolforge exports for this service, as
    /// `(user, password)`.
    pub fn env_vars(self) -> (&'static str, &'static str) {
        match self {
            Self::Replicas => ("TOOL_REPLICA_USER", "TOOL_REPLICA_PASSWORD"),
            Self::ToolsDb => ("TOOL_TOOLSDB_USER", "TOOL_TOOLSDB_PASSWORD"),
        }
    }
}

impl std::fmt::Display for DbService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Replicas => "the Wiki Replicas",
            Self::ToolsDb => "ToolsDB",
        })
    }
}

/// A user name and password for a Toolforge database service.
///
/// `Debug` deliberately redacts the password, so a credentials value can be
/// logged with the rest of a configuration.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    user: String,
    password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl Credentials {
    pub fn new(user: &str, password: &str) -> Self {
        Self {
            user: user.to_string(),
            password: password.to_string(),
        }
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    /// The credentials for `service`: from the environment if Toolforge
    /// exported them, otherwise from `replica.my.cnf`. See the [module
    /// documentation](self) for the details.
    pub fn for_service(service: DbService) -> Result<Self, DatabaseError> {
        if let Some(credentials) = Self::from_env(service) {
            return Ok(credentials);
        }
        let path = Self::my_cnf_path().ok_or(DatabaseError::MissingCredentials(service))?;
        Self::from_my_cnf(&path)
    }

    /// The credentials `service` exports through the environment, if both of
    /// its variables are set and non-empty.
    pub fn from_env(service: DbService) -> Option<Self> {
        let (user_var, password_var) = service.env_vars();
        Self::from_non_empty(
            std::env::var(user_var).ok(),
            std::env::var(password_var).ok(),
        )
    }

    /// Credentials from a user and password that are both present and
    /// non-empty.
    fn from_non_empty(user: Option<String>, password: Option<String>) -> Option<Self> {
        let user = user.filter(|user| !user.is_empty())?;
        let password = password.filter(|password| !password.is_empty())?;
        Some(Self { user, password })
    }

    /// Where `replica.my.cnf` is, preferring `$TOOL_DATA_DIR` over `$HOME`.
    /// `None` if neither directory holds one.
    pub fn my_cnf_path() -> Option<PathBuf> {
        Self::my_cnf_candidates(
            ["TOOL_DATA_DIR", "HOME"]
                .iter()
                .filter_map(std::env::var_os),
        )
        .find(|path| path.is_file())
    }

    /// Where `replica.my.cnf` would be in each of `dirs`. An empty entry is
    /// skipped rather than read as the current directory, which is what
    /// joining onto an empty path would mean.
    fn my_cnf_candidates(
        dirs: impl IntoIterator<Item = OsString>,
    ) -> impl Iterator<Item = PathBuf> {
        dirs.into_iter()
            .filter(|dir| !dir.is_empty())
            .map(|dir| Path::new(&dir).join(MY_CNF_FILE))
    }

    /// Reads credentials from a `my.cnf`-style file.
    ///
    /// Only the `user` and `password` of the `[client]` section are read, and
    /// only as much of the format as Toolforge's generated file uses: one
    /// `key = value` per line, values optionally wrapped in single or double
    /// quotes, `#` and `;` starting a comment, also after a section header. A
    /// leading UTF-8 byte order mark is ignored.
    pub fn from_my_cnf(path: &Path) -> Result<Self, DatabaseError> {
        let contents =
            std::fs::read_to_string(path).map_err(|source| DatabaseError::CredentialsFile {
                path: path.display().to_string(),
                source,
            })?;
        Self::parse_my_cnf(&contents).ok_or_else(|| DatabaseError::MalformedCredentials {
            path: path.display().to_string(),
        })
    }

    fn parse_my_cnf(contents: &str) -> Option<Self> {
        let mut in_client_section = false;
        let mut user = None;
        let mut password = None;
        let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(header) = line.strip_prefix('[') {
                // Anything after the `]`, such as a comment, is not the name.
                let section = header
                    .split_once(']')
                    .map_or(header, |(name, _)| name)
                    .trim();
                in_client_section = section.eq_ignore_ascii_case("client");
                continue;
            }
            if !in_client_section {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = unquote(value.trim()).to_string();
            match key.trim().to_ascii_lowercase().as_str() {
                "user" => user = Some(value),
                "password" => password = Some(value),
                _ => {}
            }
        }
        Some(Self::new(&user?, &password?))
    }
}

/// Strips one layer of matching single or double quotes, which `my.cnf` values
/// sometimes carry.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|value| value.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_vars_are_service_specific() {
        assert_eq!(
            DbService::Replicas.env_vars(),
            ("TOOL_REPLICA_USER", "TOOL_REPLICA_PASSWORD")
        );
        assert_eq!(
            DbService::ToolsDb.env_vars(),
            ("TOOL_TOOLSDB_USER", "TOOL_TOOLSDB_PASSWORD")
        );
    }

    #[test]
    fn test_parse_my_cnf() {
        let credentials = Credentials::parse_my_cnf(
            "# generated file\n[client]\nuser='s51234'\npassword='sekrit'\n",
        )
        .unwrap();
        assert_eq!(credentials.user(), "s51234");
        assert_eq!(credentials.password(), "sekrit");
    }

    #[test]
    fn test_parse_my_cnf_tolerates_formatting() {
        // Double quotes, padding, an unrelated key, and a trailing section.
        let credentials = Credentials::parse_my_cnf(
            "[client]\n  USER = \"u12345\"  \npassword=\"pw\"\nport = 3306\n; comment\n[mysql]\nuser=other\n",
        )
        .unwrap();
        assert_eq!(credentials.user(), "u12345");
        assert_eq!(credentials.password(), "pw");
    }

    #[test]
    fn test_parse_my_cnf_ignores_other_sections() {
        // A `user` outside `[client]` must not be picked up.
        assert!(Credentials::parse_my_cnf("[mysqldump]\nuser=u1\npassword=p1\n").is_none());
        // Neither must a file that is missing the password.
        assert!(Credentials::parse_my_cnf("[client]\nuser=u1\n").is_none());
        assert!(Credentials::parse_my_cnf("").is_none());
    }

    #[test]
    fn test_parse_my_cnf_keeps_password_characters() {
        // Passwords are not shell-quoted, and may contain anything but a
        // newline -- including the `=` that splits the line.
        let credentials =
            Credentials::parse_my_cnf("[client]\nuser=u1\npassword=a=b#c'd\"e\n").unwrap();
        assert_eq!(credentials.password(), "a=b#c'd\"e");
    }

    #[test]
    fn test_parse_my_cnf_tolerates_bom_and_header_comment() {
        // A byte order mark from an editor, and a comment after the header,
        // must not hide the `[client]` section.
        let credentials =
            Credentials::parse_my_cnf("\u{feff}[client] # generated\nuser=u1\npassword=p1\n")
                .unwrap();
        assert_eq!(credentials, Credentials::new("u1", "p1"));
        let credentials = Credentials::parse_my_cnf("[client];x\nuser=u1\npassword=p1\n").unwrap();
        assert_eq!(credentials, Credentials::new("u1", "p1"));
        // ... while another section with a comment is still another section.
        assert!(Credentials::parse_my_cnf("[mysql] # client\nuser=u1\npassword=p1\n").is_none());
    }

    #[test]
    fn test_env_credentials_need_user_and_password() {
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            Credentials::from_non_empty(some("u1"), some("p1")),
            Some(Credentials::new("u1", "p1"))
        );
        // Anything missing or empty falls through to `replica.my.cnf`.
        assert_eq!(Credentials::from_non_empty(some("u1"), some("")), None);
        assert_eq!(Credentials::from_non_empty(some(""), some("p1")), None);
        assert_eq!(Credentials::from_non_empty(some("u1"), None), None);
        assert_eq!(Credentials::from_non_empty(None, some("p1")), None);
    }

    #[test]
    fn test_my_cnf_candidates_skip_empty_dirs() {
        // An empty `TOOL_DATA_DIR` or `HOME` must not turn into a relative
        // `replica.my.cnf`, i.e. one in the current directory.
        let candidates: Vec<PathBuf> = Credentials::my_cnf_candidates([
            OsString::new(),
            OsString::from("/data/project/x"),
            OsString::new(),
            OsString::from("/home/x"),
        ])
        .collect();
        assert_eq!(
            candidates,
            [
                Path::new("/data/project/x/replica.my.cnf"),
                Path::new("/home/x/replica.my.cnf")
            ]
        );
    }

    #[test]
    fn test_unquote() {
        assert_eq!(unquote("'x'"), "x");
        assert_eq!(unquote("\"x\""), "x");
        assert_eq!(unquote("x"), "x");
        // Unbalanced or degenerate input is left alone rather than panicking.
        assert_eq!(unquote("'x"), "'x");
        assert_eq!(unquote("\""), "\"");
        assert_eq!(unquote(""), "");
    }

    #[test]
    fn test_debug_redacts_password() {
        let debug = format!("{:?}", Credentials::new("u12345", "sekrit"));
        assert!(debug.contains("u12345"), "{debug}");
        assert!(!debug.contains("sekrit"), "{debug}");
    }

    #[test]
    fn test_from_my_cnf_missing_file_is_err() {
        let err = Credentials::from_my_cnf(Path::new("/nonexistent/replica.my.cnf")).unwrap_err();
        assert!(
            matches!(&err, DatabaseError::CredentialsFile { path, .. } if path.contains("replica.my.cnf")),
            "expected CredentialsFile, got: {err:?}"
        );
    }

    #[test]
    fn test_from_my_cnf_reads_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MY_CNF_FILE);
        std::fs::write(&path, "[client]\nuser=u42\npassword=pw42\n").unwrap();
        let credentials = Credentials::from_my_cnf(&path).unwrap();
        assert_eq!(credentials, Credentials::new("u42", "pw42"));
    }

    #[test]
    fn test_from_my_cnf_malformed_file_is_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MY_CNF_FILE);
        std::fs::write(&path, "nothing useful here\n").unwrap();
        let err = Credentials::from_my_cnf(&path).unwrap_err();
        assert!(
            matches!(&err, DatabaseError::MalformedCredentials { .. }),
            "expected MalformedCredentials, got: {err:?}"
        );
    }
}
