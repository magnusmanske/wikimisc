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
    use proptest::prelude::*;
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

    // --- properties -----------------------------------------------------

    fn any_extensions() -> impl Strategy<Value = Vec<String>> {
        proptest::collection::vec("[a-z]{1,5}", 0..4)
    }

    fn as_refs(extensions: &[String]) -> Vec<&str> {
        extensions.iter().map(String::as_str).collect()
    }

    /// A clean absolute path of ordinary segments, possibly `/` itself.
    fn safe_prefix() -> impl Strategy<Value = String> {
        proptest::collection::vec("[A-Za-z0-9_-]{1,8}", 0..3)
            .prop_map(|segments| format!("/{}", segments.join("/")))
    }

    /// A path whose last segment has the returned extension.
    fn public_path() -> impl Strategy<Value = (String, String)> {
        (
            proptest::collection::vec("[A-Za-z0-9_-]{1,8}", 0..3),
            "[A-Za-z0-9_-]{1,8}",
            "[a-z]{1,5}",
        )
            .prop_map(|(mut directories, stem, extension)| {
                directories.push(format!("{stem}.{extension}"));
                (format!("/{}", directories.join("/")), extension)
            })
    }

    fn decode(path: &str) -> String {
        percent_encoding::percent_decode_str(path)
            .decode_utf8()
            .expect("a Public verdict implies the path decoded as UTF-8")
            .into_owned()
    }

    proptest! {
        /// Nothing served as `Public` is hidden or able to climb out, however
        /// it was percent-encoded.
        #[test]
        fn public_is_never_hidden_or_escaping(
            path in any::<String>(),
            extensions in any_extensions(),
        ) {
            let extensions = as_refs(&extensions);
            if classify(&path, &extensions) == Public {
                let path = decode(&path);
                for segment in path.split('/') {
                    prop_assert!(
                        !segment.starts_with('.'),
                        "hidden segment {segment:?} in {path:?}"
                    );
                    prop_assert!(
                        !segment.contains(['\\', '\0']),
                        "backslash/NUL segment {segment:?} in {path:?}"
                    );
                }
            }
        }

        /// `IfDirectory` is always a clean relative path whose last segment is
        /// not a file that would have been served as `Public`.
        #[test]
        fn if_directory_is_a_safe_relative_path(
            path in any::<String>(),
            extensions in any_extensions(),
        ) {
            let extensions = as_refs(&extensions);
            if let IfDirectory(relative) = classify(&path, &extensions) {
                prop_assert!(!relative.is_empty());
                prop_assert!(!relative.starts_with('/') && !relative.ends_with('/'));
                for segment in relative.split('/') {
                    prop_assert!(!segment.is_empty());
                    prop_assert!(
                        !segment.starts_with('.') && !segment.contains(['\\', '\0']),
                        "unsafe segment {segment:?} in {relative:?}"
                    );
                }
                if let Some(last) = relative.rsplit('/').next() {
                    prop_assert!(
                        !has_public_extension(last, &extensions),
                        "{relative:?} would have been served as a file"
                    );
                }
            }
        }

        /// `Public` means a directory (or root), or a file with an allowlisted
        /// extension.
        #[test]
        fn public_is_a_directory_or_an_allowed_file(
            path in any::<String>(),
            extensions in any_extensions(),
        ) {
            let extensions = as_refs(&extensions);
            if classify(&path, &extensions) == Public {
                let path = decode(&path);
                let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                prop_assert!(
                    path.ends_with('/')
                        || segments.is_empty()
                        || segments
                            .last()
                            .is_some_and(|name| has_public_extension(name, &extensions)),
                    "unexpected Public path {path:?}"
                );
            }
        }

        /// Root and empty paths are public.
        #[test]
        fn empty_path_is_public(extensions in any_extensions()) {
            let extensions = as_refs(&extensions);
            for path in ["", "/", "//", "///"] {
                prop_assert_eq!(classify(path, &extensions), Public);
            }
        }

        /// A clean path ending in an allowlisted extension is public.
        #[test]
        fn allowed_extension_is_public((path, extension) in public_path()) {
            prop_assert_eq!(classify(&path, &[extension.as_str()]), Public);
        }

        /// A dot segment, backslash or NUL stays private however encoded.
        #[test]
        fn encoded_dot_segment_is_private(
            prefix in safe_prefix(),
            name in "[A-Za-z0-9_-]{0,8}",
        ) {
            let path = format!("{prefix}/%2E{name}");
            prop_assert_eq!(classify(&path, EXTENSIONS), Private);
        }

        #[test]
        fn encoded_backslash_and_nul_are_private(
            prefix in safe_prefix(),
            name in "[A-Za-z0-9_-]{0,8}",
        ) {
            for encoded in ["%5C", "%5c", "%00"] {
                let path = format!("{prefix}/a{encoded}{name}");
                prop_assert_eq!(classify(&path, EXTENSIONS), Private, "path {:?}", path);
            }
        }

        /// Percent-encoding a path does not change what it classifies as.
        #[test]
        fn percent_encoding_preserves_the_verdict(
            path in safe_prefix(),
            extensions in any_extensions(),
        ) {
            let extensions = as_refs(&extensions);
            let encoded: String =
                percent_encoding::utf8_percent_encode(&path, percent_encoding::NON_ALPHANUMERIC)
                    .to_string();
            prop_assert_eq!(
                classify(&encoded, &extensions),
                classify(&path, &extensions)
            );
        }

        /// Extension matching needs a dot and ignores ASCII case.
        #[test]
        fn extension_matches_case_insensitively(
            name in any::<String>(),
            extension in "[a-z]{1,5}",
        ) {
            let upper = extension.to_ascii_uppercase();
            let file = format!("{name}.{upper}");
            prop_assert!(has_public_extension(&file, &[extension.as_str()]));
            prop_assert!(has_public_extension(&file, &[upper.as_str()]));
        }

        #[test]
        fn extension_match_requires_a_dot(
            name in any::<String>(),
            extensions in any_extensions(),
        ) {
            let extensions = as_refs(&extensions);
            if has_public_extension(&name, &extensions) {
                prop_assert!(name.contains('.'));
            }
            let none: [&str; 0] = [];
            prop_assert!(!has_public_extension(&name, &none));
        }
    }
}
