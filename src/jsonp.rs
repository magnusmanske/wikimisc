//! JSONP: JSON wrapped in a call of a function the client names, for
//! clients (old user scripts and gadgets) that load data with a `<script>`
//! tag.
//!
//! The callback name ends up verbatim in a script the browser runs, so it
//! must be checked: [`is_valid_callback`] admits JavaScript identifiers only
//! (optionally dotted paths). This module has no dependencies; with the
//! `server` feature, `server::jsonp::Jsonp` is an axum response that
//! does the check itself.

/// The content type of a JSONP response.
pub const CONTENT_TYPE: &str = "application/javascript; charset=utf-8";

/// Which callback names [`is_valid_callback`] admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallbackRules {
    /// Also admit dotted paths and indexes: `jQuery.cb`, `cbs[3]`.
    pub allow_dotted: bool,
    /// The longest name admitted, in bytes.
    pub max_len: usize,
}

impl Default for CallbackRules {
    /// A plain identifier of at most 64 bytes.
    fn default() -> Self {
        Self {
            allow_dotted: false,
            max_len: 64,
        }
    }
}

/// Whether `callback` is a safe callback name: a JavaScript identifier
/// (`[A-Za-z_$][A-Za-z0-9_$]*`), with `rules.allow_dotted` followed by any
/// number of `.identifier` and `[digits]`, and at most `rules.max_len` long.
pub fn is_valid_callback(callback: &str, rules: CallbackRules) -> bool {
    if callback.is_empty() || callback.len() > rules.max_len {
        return false;
    }
    let mut rest = match identifier(callback) {
        Some(rest) => rest,
        None => return false,
    };
    while !rest.is_empty() {
        if !rules.allow_dotted {
            return false;
        }
        rest = if let Some(after_dot) = rest.strip_prefix('.') {
            match identifier(after_dot) {
                Some(after) => after,
                None => return false,
            }
        } else if let Some(after_bracket) = rest.strip_prefix('[') {
            let digits = after_bracket.bytes().take_while(u8::is_ascii_digit).count();
            match after_bracket
                .get(digits..)
                .and_then(|r| r.strip_prefix(']'))
            {
                Some(after) if digits > 0 => after,
                _ => return false,
            }
        } else {
            return false;
        };
    }
    true
}

/// Consumes one identifier from the front of `s`, returning the rest.
fn identifier(s: &str) -> Option<&str> {
    let is_start = |b: u8| b.is_ascii_alphabetic() || b == b'_' || b == b'$';
    let first = *s.as_bytes().first()?;
    if !is_start(first) {
        return None;
    }
    let len = s
        .bytes()
        .take_while(|&b| is_start(b) || b.is_ascii_digit())
        .count();
    s.get(len..)
}

/// `callback(json);`. Does not check the name; see [`is_valid_callback`].
pub fn wrap(callback: &str, json: &str) -> String {
    format!("{callback}({json});")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strict_callbacks() {
        let rules = CallbackRules::default();
        for ok in ["cb", "_cb", "$", "jQuery123_456", "a1"] {
            assert!(is_valid_callback(ok, rules), "{ok}");
        }
        for bad in [
            "",
            "1cb",
            "cb.name",
            "cb[0]",
            "alert(1)",
            "cb;evil",
            "cb name",
            "cb\u{e9}",
            &"a".repeat(65),
        ] {
            assert!(!is_valid_callback(bad, rules), "{bad}");
        }
    }

    #[test]
    fn test_dotted_callbacks() {
        let rules = CallbackRules {
            allow_dotted: true,
            max_len: 256,
        };
        for ok in ["cb", "jQuery.cb", "a.b.c", "cbs[3]", "a.b[12].c"] {
            assert!(is_valid_callback(ok, rules), "{ok}");
        }
        for bad in ["a.", ".a", "a..b", "a[]", "a[x]", "a[1", "a.1b", "a(1)"] {
            assert!(!is_valid_callback(bad, rules), "{bad}");
        }
    }

    #[test]
    fn test_wrap() {
        assert_eq!(wrap("cb", r#"{"a":1}"#), r#"cb({"a":1});"#);
    }
}
