//! Feeds arbitrary text through the query rewriters.
//!
//! The input is untrusted (queries come from wikitext), so the interesting
//! failures are a panic, unbounded output, or a rewrite that leaves work for
//! the next pass. The label service rewrite refuses named subqueries, so the
//! two rewrites are exercised independently.

#![no_main]

use libfuzzer_sys::fuzz_target;
use wikimisc::sparql_rewrite::{
    rewrite_label_service, rewrite_named_subqueries, LabelServiceRewrite, NamedSubqueryRewrite,
};

/// The documented cap on how much inlining may grow a query: `MAX_GROWTH`
/// times the input, or `MIN_LENGTH_LIMIT` bytes, whichever is larger.
const MAX_GROWTH: usize = 16;
const MIN_LENGTH_LIMIT: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    let Ok(query) = std::str::from_utf8(data) else {
        return;
    };

    match rewrite_named_subqueries(query) {
        NamedSubqueryRewrite::Rewritten(output) => {
            let cap = query.len().saturating_mul(MAX_GROWTH).max(MIN_LENGTH_LIMIT);
            assert!(
                output.len() <= cap,
                "inlined query is {} bytes, cap is {cap}",
                output.len()
            );
            assert!(
                matches!(
                    rewrite_named_subqueries(&output),
                    NamedSubqueryRewrite::Unchanged
                ),
                "inlined query still has named subqueries to inline"
            );
        }
        NamedSubqueryRewrite::Unchanged | NamedSubqueryRewrite::Unsupported(_) => {}
    }

    match rewrite_label_service(query, |_| true) {
        LabelServiceRewrite::Rewritten(output) => {
            assert!(!output.is_empty(), "label rewrite produced an empty query");
            assert!(
                matches!(
                    rewrite_label_service(&output, |_| true),
                    LabelServiceRewrite::Unchanged
                ),
                "rewritten query still has a label service to rewrite"
            );
        }
        LabelServiceRewrite::Unchanged | LabelServiceRewrite::Unsupported(_) => {}
    }
});
