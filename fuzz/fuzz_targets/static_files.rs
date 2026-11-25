//! Feeds arbitrary URI paths through the static-file allowlist.
//!
//! This is the check that keeps PHP/Perl sources, `.git` and keys out of
//! `public_html`, so the properties are the security ones: a `Public` verdict
//! may never resolve to a hidden segment, a backslash or a NUL, and
//! `IfDirectory` may never point outside the root.

#![no_main]

use libfuzzer_sys::fuzz_target;
use percent_encoding::percent_decode_str;
use wikimisc::web::static_files::{classify, has_public_extension, Verdict};

const EXTENSIONS: &[&str] = &["css", "html", "jpg", "js", "png"];

fn decode(path: &str) -> Option<String> {
    percent_decode_str(path)
        .decode_utf8()
        .ok()
        .map(|decoded| decoded.into_owned())
}

fuzz_target!(|data: &[u8]| {
    let Ok(path) = std::str::from_utf8(data) else {
        return;
    };

    match classify(path, EXTENSIONS) {
        Verdict::Private => {}
        Verdict::Public => {
            let decoded = decode(path).expect("Public implies the path decoded as UTF-8");
            for segment in decoded.split('/') {
                assert!(
                    !segment.starts_with('.'),
                    "serving hidden segment {segment:?} from {path:?}"
                );
                assert!(
                    !segment.contains(['\\', '\0']),
                    "serving backslash/NUL segment {segment:?} from {path:?}"
                );
            }
            if !decoded.ends_with('/') {
                let segments: Vec<&str> = decoded
                    .split('/')
                    .filter(|segment| !segment.is_empty())
                    .collect();
                if let Some(last) = segments.last() {
                    assert!(
                        has_public_extension(last, EXTENSIONS),
                        "serving {last:?} without an allowlisted extension from {path:?}"
                    );
                }
            }
        }
        Verdict::IfDirectory(relative) => {
            assert!(!relative.is_empty(), "empty directory path from {path:?}");
            assert!(
                !relative.starts_with('/') && !relative.ends_with('/'),
                "directory path {relative:?} is not relative"
            );
            for segment in relative.split('/') {
                assert!(!segment.is_empty(), "empty segment in {relative:?}");
                assert!(
                    !segment.starts_with('.'),
                    "hidden segment {segment:?} in {relative:?}"
                );
                assert!(
                    !segment.contains(['\\', '\0']),
                    "backslash/NUL segment {segment:?} in {relative:?}"
                );
            }
            if let Some(last) = relative.rsplit('/').next() {
                assert!(
                    !has_public_extension(last, EXTENSIONS),
                    "{relative:?} would have been served as a file"
                );
            }
        }
    }
});
