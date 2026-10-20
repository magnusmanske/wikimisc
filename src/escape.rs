//! Escaping text for HTML and XML output, and decoding HTML character
//! references.
//!
//! Every function returns a [`Cow`] that borrows the input when there is
//! nothing to change, so escaping text that needs none costs no allocation.

use std::borrow::Cow;

/// Escapes `& < > " '` (the apostrophe as `&#39;`), so the result is safe both
/// as element text and inside a quoted attribute value.
pub fn html(s: &str) -> Cow<'_, str> {
    escape_with(s, |c| match c {
        '&' => Some("&amp;"),
        '<' => Some("&lt;"),
        '>' => Some("&gt;"),
        '"' => Some("&quot;"),
        '\'' => Some("&#39;"),
        _ => None,
    })
}

/// Escapes `& < >` only: enough for element text, but **not** for attribute
/// values — use [`html`] there.
pub fn html_text(s: &str) -> Cow<'_, str> {
    escape_with(s, |c| match c {
        '&' => Some("&amp;"),
        '<' => Some("&lt;"),
        '>' => Some("&gt;"),
        _ => None,
    })
}

/// Escapes `& < > " '` for XML (the apostrophe as `&apos;`, which HTML 4 did
/// not know), e.g. for KML or RSS output.
pub fn xml(s: &str) -> Cow<'_, str> {
    escape_with(s, |c| match c {
        '&' => Some("&amp;"),
        '<' => Some("&lt;"),
        '>' => Some("&gt;"),
        '"' => Some("&quot;"),
        '\'' => Some("&apos;"),
        _ => None,
    })
}

/// Decodes the character references MediaWiki and its APIs commonly produce:
/// `&amp; &lt; &gt; &quot; &apos; &nbsp;` and numeric references (`&#39;`,
/// `&#x27;`). Anything else — an unknown entity name, a reference to an
/// invalid code point, a lone `&` — is left as it is.
pub fn unescape_html(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        let (before, from_amp) = rest.split_at(pos);
        out.push_str(before);
        match decode_reference(from_amp) {
            Some((c, consumed)) => {
                out.push(c);
                rest = from_amp.get(consumed..).unwrap_or("");
            }
            None => {
                out.push('&');
                rest = from_amp.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// The longest reference [`decode_reference`] looks at, `;` included
/// (`&#x10FFFF;` is 10 bytes).
const MAX_REFERENCE_LEN: usize = 12;

/// Decodes the reference at the start of `s` (which starts with `&`), returning
/// the character and the number of bytes it took up.
fn decode_reference(s: &str) -> Option<(char, usize)> {
    let end = s
        .char_indices()
        .take(MAX_REFERENCE_LEN)
        .find(|&(_, c)| c == ';')
        .map(|(i, _)| i)?;
    let body = s.get(1..end)?;
    let c = match body {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => {
            let number = body.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((c, end + 1))
}

fn escape_with(s: &str, replacement: impl Fn(char) -> Option<&'static str>) -> Cow<'_, str> {
    let Some(first) = s.find(|c| replacement(c).is_some()) else {
        return Cow::Borrowed(s);
    };
    let (clean, rest) = s.split_at(first);
    let mut out = String::with_capacity(s.len() + 16);
    out.push_str(clean);
    for c in rest.chars() {
        match replacement(c) {
            Some(escaped) => out.push_str(escaped),
            None => out.push(c),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escapes_text_and_attribute_characters() {
        assert_eq!(
            html(r#"<a href="x">Tom & Jerry's</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; Jerry&#39;s&lt;/a&gt;"
        );
    }

    #[test]
    fn html_text_leaves_quotes_alone() {
        assert_eq!(html_text(r#"a<b & "c" 'd'"#), r#"a&lt;b &amp; "c" 'd'"#);
    }

    #[test]
    fn xml_uses_apos() {
        assert_eq!(xml(r#"<>&"'"#), "&lt;&gt;&amp;&quot;&apos;");
    }

    #[test]
    fn nothing_to_escape_borrows() {
        for f in [html, html_text, xml, unescape_html] {
            assert!(matches!(f("Köln, plain text"), Cow::Borrowed(_)));
        }
        assert_eq!(html(""), "");
    }

    #[test]
    fn unescape_named_and_numeric_references() {
        assert_eq!(
            unescape_html("Tom &amp; Jerry&#39;s &lt;b&gt; &quot;x&quot; &apos;y&apos;"),
            r#"Tom & Jerry's <b> "x" 'y'"#
        );
        assert_eq!(unescape_html("&#x27;&#X41;&#228;&nbsp;"), "'Aä\u{a0}");
    }

    #[test]
    fn unescape_leaves_unknown_and_broken_references() {
        assert_eq!(unescape_html("AT&T"), "AT&T");
        assert_eq!(unescape_html("&copy; &bogus; & &;"), "&copy; &bogus; & &;");
        assert_eq!(
            unescape_html("&#xD800; &#99999999;"),
            "&#xD800; &#99999999;"
        );
        assert_eq!(unescape_html("&amp"), "&amp");
        assert_eq!(unescape_html("&&amp;"), "&&");
    }

    #[test]
    fn escape_round_trips() {
        let original = r#"<p class="a">Ä & 'b'</p>"#;
        assert_eq!(unescape_html(&html(original)), original);
        assert_eq!(unescape_html(&xml(original)), original);
    }
}
