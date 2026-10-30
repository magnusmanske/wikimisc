//! Which paths under a `public_html` directory may be served.
//!
//! `public_html` holds much that must never go out: PHP and Perl source,
//! `.git` and `.svn` directories, keys. Only files with an allowlisted
//! extension are public, and nothing under a hidden name.

use percent_encoding::percent_decode_str;

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Public,
    Private,
    /// Public only if this path, relative to the root, is a directory.
    IfDirectory(String),
}

/// Classifies `uri_path` against `extensions` (lowercase, without a dot).
///
/// A hidden segment (which also covers `..`), a backslash, or a NUL byte is
/// always private, however it was percent-encoded. A directory is public so
/// that `/dir` can redirect to `/dir/` (and its `index.html`).
pub fn classify(uri_path: &str, extensions: &[&str]) -> Verdict {
    let Ok(path) = percent_decode_str(uri_path).decode_utf8() else {
        return Verdict::Private;
    };
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // A leading dot also covers `..`.
    if segments
        .iter()
        .any(|s| s.starts_with('.') || s.contains(['\\', '\0']))
    {
        return Verdict::Private;
    }
    match segments.last() {
        None => Verdict::Public,
        Some(_) if path.ends_with('/') => Verdict::Public,
        Some(name) if has_public_extension(name, extensions) => Verdict::Public,
        Some(_) => Verdict::IfDirectory(segments.join("/")),
    }
}

/// Whether `name` ends in one of `extensions` (case-insensitive).
pub fn has_public_extension(name: &str, extensions: &[&str]) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        extensions
            .iter()
            .any(|allowed| ext.eq_ignore_ascii_case(allowed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use Verdict::*;

    const EXTENSIONS: &[&str] = &["css", "html", "jpg", "js", "png"];

    #[test]
    fn classification() {
        assert_eq!(classify("/", EXTENSIONS), Public);
        assert_eq!(classify("/index.html", EXTENSIONS), Public);
        assert_eq!(classify("/resources/js/common.JS", EXTENSIONS), Public);
        assert_eq!(classify("/mashwiki/", EXTENSIONS), Public);
        assert_eq!(
            classify("/mashwiki", EXTENSIONS),
            IfDirectory("mashwiki".into())
        );
        assert_eq!(
            classify("/logger.php", EXTENSIONS),
            IfDirectory("logger.php".into())
        );
    }

    #[test]
    fn nothing_hidden_or_climbing_out() {
        for private in [
            "/resources/js/dracula/.git/config",
            "/resources/%2Egit/config",
            "/file_reuse/.svn/",
            "/relator2/.claude/",
            "/../replica.my.cnf",
            "/%2e%2e/replica.my.cnf",
            "/a%5C..%5Cb.html",
            "/%00.html",
            "/%FF.html",
        ] {
            assert_eq!(classify(private, EXTENSIONS), Private, "{private}");
        }
    }

    #[test]
    fn extensions_are_case_insensitive_and_complete() {
        assert!(has_public_extension("A.PNG", EXTENSIONS));
        assert!(!has_public_extension("A.php", EXTENSIONS));
        assert!(!has_public_extension("php", EXTENSIONS));
    }
}
