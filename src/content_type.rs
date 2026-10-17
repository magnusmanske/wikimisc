//! Content-type checks shared by the HTTP clients of several features. Kept
//! free of any HTTP crate's types, so it couples none of those features.

/// Whether a response whose `Content-Type` is `content_type` may hold a CSV
/// result. WDQS answers `text/csv;charset=utf-8` and QLever `text/csv`;
/// `text/plain` and the catch-all `application/octet-stream` are let through
/// too, because a server that does not know better sends those for CSV. What
/// this rules out is a page in some other format served with status 200 —
/// typically an HTML error page from a proxy.
pub(crate) fn may_be_csv(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "" | "text/csv"
            | "application/csv"
            | "text/comma-separated-values"
            | "text/plain"
            | "application/octet-stream"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_may_be_csv_accepts_csv_types_with_parameters() {
        assert!(may_be_csv("text/csv;charset=utf-8"));
        assert!(may_be_csv("Text/CSV ; charset=UTF-8"));
        assert!(may_be_csv("text/plain"));
        assert!(may_be_csv("application/octet-stream"));
        assert!(may_be_csv(""));
    }

    #[test]
    fn test_may_be_csv_rejects_other_formats() {
        assert!(!may_be_csv("text/html; charset=utf-8"));
        assert!(!may_be_csv("application/json"));
        assert!(!may_be_csv("application/sparql-results+json"));
        assert!(!may_be_csv("application/xml"));
    }
}
