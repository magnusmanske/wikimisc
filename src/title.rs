//! MediaWiki page titles: normalisation, namespace prefixes, and page URLs.
//!
//! A title has two spellings: the *display* form with spaces (`Main Page`),
//! used in wikitext and most API parameters, and the *DB key* with underscores
//! (`Main_Page`), used in URLs and database tables. [`normalize`] turns user
//! input into the display form the way MediaWiki does, so two spellings of the
//! same page compare equal.
//!
//! This works offline, without the wiki's configuration. Localised namespace
//! names have to be supplied by the caller ([`strip_namespace`]); for
//! resolving them through the API, use `mediawiki::title::Title`.

use std::borrow::Cow;

/// Whether a wiki capitalises the first letter of titles. Most do;
/// Wiktionaries are case-sensitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Case {
    /// The first letter is always uppercase (`foo` is the page `Foo`).
    #[default]
    FirstLetter,
    /// Titles are taken as they are (`foo` and `Foo` are different pages).
    Sensitive,
}

/// The DB-key form: spaces become underscores. Nothing else changes; use
/// [`normalize`] first for user input.
pub fn to_db_key(title: &str) -> String {
    title.replace(' ', "_")
}

/// The display form: underscores become spaces. Nothing else changes; use
/// [`normalize`] for user input.
pub fn to_display(title: &str) -> String {
    title.replace('_', " ")
}

/// Normalises a title to its display form, as MediaWiki does: underscores
/// become spaces, directional marks are removed, runs of whitespace collapse
/// to one space, surrounding whitespace and one leading `:` (as in
/// `[[:Category:Foo]]`) go, and the first letter is uppercased unless the wiki
/// is [`Case::Sensitive`].
///
/// The namespace prefix is not recognised, so in `user:foo` only the `u` is
/// uppercased (MediaWiki would also uppercase the `f`).
pub fn normalize(title: &str, case: Case) -> String {
    let mut collapsed = String::with_capacity(title.len());
    let mut pending_space = false;
    for c in title.chars() {
        if is_directional_mark(c) {
            continue;
        }
        if c == '_' || is_title_whitespace(c) {
            pending_space = true;
            continue;
        }
        if pending_space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        pending_space = false;
        collapsed.push(c);
    }
    let without_colon = collapsed
        .strip_prefix(':')
        .map_or(collapsed.as_str(), str::trim_start);
    match case {
        Case::FirstLetter => ucfirst(without_colon).into_owned(),
        Case::Sensitive => without_colon.to_string(),
    }
}

/// Uppercases the first letter the way MediaWiki does on Wikimedia wikis.
///
/// A letter whose uppercase form is more than one character stays as it is
/// (`ß` does not become `SS`), and so do Georgian letters, which Wikimedia
/// exempts from capitalisation. Elsewhere this follows Rust's Unicode tables,
/// which may differ from MediaWiki's for a handful of rare characters.
pub fn ucfirst(s: &str) -> Cow<'_, str> {
    map_first(s, |c| {
        if is_georgian_lowercase(c) {
            return None;
        }
        single_char(c.to_uppercase())
    })
}

/// Lowercases the first letter, if its lowercase form is a single character.
pub fn lcfirst(s: &str) -> Cow<'_, str> {
    map_first(s, |c| single_char(c.to_lowercase()))
}

/// Strips a namespace prefix: if `title` starts with one of `names` followed
/// by a colon, returns the rest, else `None`.
///
/// Names match case-insensitively, with spaces and underscores equivalent and
/// whitespace allowed around the colon, so `["File", "Image"]` strips `File:`,
/// `file :` and `IMAGE:`. A leading colon (`:Category:Foo`) is skipped. Pass
/// every localised name and alias the wiki has for the namespace (`&[&str]`,
/// `Vec<String>`, … all work).
pub fn strip_namespace<'a, S: AsRef<str>>(title: &'a str, names: &[S]) -> Option<&'a str> {
    let title = title.trim_start();
    let title = title.strip_prefix(':').unwrap_or(title);
    let (prefix, rest) = title.split_once(':')?;
    let prefix = namespace_key(prefix);
    names
        .iter()
        .any(|name| namespace_key(name.as_ref()) == prefix)
        .then(|| rest.trim_start())
}

/// Percent-encodes everything except the RFC 3986 unreserved characters
/// (letters, digits, `-_.~`), like PHP's `rawurlencode`.
pub fn percent_encode(s: &str) -> String {
    encode_except(s, b"-_.~")
}

/// Decodes `%XX` sequences. Malformed sequences are kept as they are, and
/// bytes that do not form valid UTF-8 become U+FFFD.
pub fn percent_decode(s: &str) -> Cow<'_, str> {
    if !s.contains('%') {
        return Cow::Borrowed(s);
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let decoded = (b == b'%')
            .then(|| {
                Some((
                    hex_value(*bytes.get(i + 1)?)?,
                    hex_value(*bytes.get(i + 2)?)?,
                ))
            })
            .flatten();
        match decoded {
            Some((high, low)) => {
                out.push((high << 4) | low);
                i += 3;
            }
            None => {
                out.push(b);
                i += 1;
            }
        }
    }
    match String::from_utf8(out) {
        Ok(decoded) => Cow::Owned(decoded),
        Err(e) => Cow::Owned(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

/// Encodes a title for a URL the way MediaWiki does (`wfUrlencode`): spaces
/// become underscores, and everything is percent-encoded except letters,
/// digits and `-_.;@$!*(),/~:`. So `AC/DC (band)` becomes `AC/DC_(band)`.
pub fn wiki_urlencode(title: &str) -> String {
    encode_except(&to_db_key(title), b"-_.;@$!*(),/~:")
}

/// The URL of a page: `page_url("https://en.wikipedia.org", "AC/DC")` is
/// `https://en.wikipedia.org/wiki/AC/DC`. A trailing slash on `server` is
/// fine, and a bare host name gets `https://`.
pub fn page_url(server: &str, title: &str) -> String {
    format!("{}/wiki/{}", server_base(server), wiki_urlencode(title))
}

/// A URL of `index.php` for a page with extra parameters:
/// `{server}/w/index.php?title=…&key=value`. Keys and values are
/// percent-encoded.
pub fn index_url(server: &str, title: &str, params: &[(&str, &str)]) -> String {
    let mut url = format!(
        "{}/w/index.php?title={}",
        server_base(server),
        wiki_urlencode(title)
    );
    for (key, value) in params {
        url.push('&');
        url.push_str(&percent_encode(key));
        url.push('=');
        url.push_str(&percent_encode(value));
    }
    url
}

/// The permanent link to a revision of a page:
/// `{server}/w/index.php?title=…&oldid=…`.
pub fn permalink(server: &str, title: &str, revision: u64) -> String {
    index_url(server, title, &[("oldid", &revision.to_string())])
}

/// The title of the page a wiki URL points to, in display form:
/// `https://de.wikipedia.org/wiki/K%C3%B6ln` gives `Köln`. Understands
/// `/wiki/<title>` and `…/index.php?title=<title>`; `None` for other URLs.
pub fn title_from_url(url: &str) -> Option<String> {
    let rest = match url.split_once("://") {
        Some((_scheme, rest)) => rest,
        None => url.strip_prefix("//")?,
    };
    let (_host, path) = rest.split_once('/')?;
    let path = path.split('#').next().unwrap_or(path);
    let encoded = match path.strip_prefix("wiki/") {
        Some(title) => Cow::Borrowed(title.split('?').next().unwrap_or(title)),
        None => {
            let (script, query) = path.split_once('?')?;
            if !script.ends_with("index.php") {
                return None;
            }
            let title = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("title="))?;
            Cow::Owned(title.replace('+', " "))
        }
    };
    let title = to_display(&percent_decode(&encoded));
    (!title.is_empty()).then_some(title)
}

/// `https://host`, from `https://host/` or a bare `host`.
fn server_base(server: &str) -> Cow<'_, str> {
    let server = server.trim().trim_end_matches('/');
    if server.contains("://") || server.starts_with("//") {
        Cow::Borrowed(server)
    } else {
        Cow::Owned(format!("https://{server}"))
    }
}

/// Percent-encodes every byte except ASCII letters, digits and `keep`.
fn encode_except(s: &str, keep: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || keep.contains(&b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
    out
}

fn hex_value(b: u8) -> Option<u8> {
    char::from(b)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

/// A namespace name for comparison: trimmed, underscores as spaces,
/// lowercase.
fn namespace_key(name: &str) -> String {
    name.trim().replace('_', " ").to_lowercase()
}

/// Replaces the first character of `s` with `map(c)`, if that is `Some` and
/// different.
fn map_first(s: &str, map: impl Fn(char) -> Option<char>) -> Cow<'_, str> {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return Cow::Borrowed(s);
    };
    match map(first) {
        Some(mapped) if mapped != first => {
            let mut out = String::with_capacity(s.len() + 2);
            out.push(mapped);
            out.push_str(chars.as_str());
            Cow::Owned(out)
        }
        _ => Cow::Borrowed(s),
    }
}

/// The only character of a case mapping, or `None` if it has several.
fn single_char(mut mapping: impl Iterator<Item = char>) -> Option<char> {
    let c = mapping.next()?;
    mapping.next().is_none().then_some(c)
}

/// Georgian Mkhedruli letters, which have had uppercase (Mtavruli) forms since
/// Unicode 11 but which Wikimedia wikis do not capitalise.
fn is_georgian_lowercase(c: char) -> bool {
    matches!(c, '\u{10D0}'..='\u{10FA}' | '\u{10FD}'..='\u{10FF}')
}

/// The left-to-right and right-to-left marks and embedding controls that
/// MediaWiki strips from titles.
fn is_directional_mark(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}')
}

/// The characters MediaWiki treats as a space in titles, plus ASCII
/// whitespace.
fn is_title_whitespace(c: char) -> bool {
    c.is_ascii_whitespace()
        || matches!(
            c,
            '\u{A0}' | '\u{1680}' | '\u{180E}' | '\u{2000}'
                ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_key_and_display_only_swap_spaces_and_underscores() {
        assert_eq!(to_db_key(" Main Page "), "_Main_Page_");
        assert_eq!(to_display("Main_Page"), "Main Page");
    }

    #[test]
    fn normalize_like_mediawiki() {
        assert_eq!(normalize("main_page", Case::FirstLetter), "Main page");
        assert_eq!(
            normalize("  foo \u{A0}__ bar\u{200E}  ", Case::FirstLetter),
            "Foo bar"
        );
        assert_eq!(
            normalize(":category:Foo", Case::FirstLetter),
            "Category:Foo"
        );
        assert_eq!(normalize(": foo", Case::FirstLetter), "Foo");
        assert_eq!(normalize("foo_bar", Case::Sensitive), "foo bar");
        assert_eq!(normalize("", Case::FirstLetter), "");
        assert_eq!(normalize(" _ ", Case::FirstLetter), "");
    }

    #[test]
    fn ucfirst_follows_mediawiki_exceptions() {
        assert_eq!(ucfirst("köln"), "Köln");
        assert_eq!(ucfirst("élan"), "Élan");
        assert_eq!(ucfirst("ßtraße"), "ßtraße");
        assert_eq!(ucfirst("ᲐᲑ"), "ᲐᲑ");
        assert_eq!(ucfirst("ანა"), "ანა");
        assert_eq!(ucfirst("ǆ"), "Ǆ");
        assert_eq!(ucfirst(""), "");
        assert!(matches!(ucfirst("Foo"), Cow::Borrowed(_)));
        assert!(matches!(ucfirst("1foo"), Cow::Borrowed(_)));
    }

    #[test]
    fn lcfirst_lowercases_the_first_letter() {
        assert_eq!(lcfirst("Foo Bar"), "foo Bar");
        assert_eq!(lcfirst("Ärger"), "ärger");
        assert_eq!(lcfirst(""), "");
    }

    #[test]
    fn strip_namespace_variants() {
        let file = ["File", "Image"];
        assert_eq!(strip_namespace("File:Foo.jpg", &file), Some("Foo.jpg"));
        assert_eq!(strip_namespace("file : Foo.jpg", &file), Some("Foo.jpg"));
        assert_eq!(strip_namespace("IMAGE:Foo.jpg", &file), Some("Foo.jpg"));
        assert_eq!(strip_namespace(":File:Foo.jpg", &file), Some("Foo.jpg"));
        assert_eq!(strip_namespace("Foo.jpg", &file), None);
        assert_eq!(strip_namespace("Filey:Foo.jpg", &file), None);
        assert_eq!(strip_namespace("Fil", &file), None);
        let from_siteinfo = vec!["Datei".to_string(), "Bild".to_string()];
        assert_eq!(
            strip_namespace("Bild:Foo.jpg", &from_siteinfo),
            Some("Foo.jpg")
        );
        assert_eq!(
            strip_namespace("ШАБЛОН:Карточка", &["Шаблон"]),
            Some("Карточка")
        );
        assert_eq!(
            strip_namespace("Wikipedia_talk:Foo", &["Wikipedia talk"]),
            Some("Foo")
        );
    }

    #[test]
    fn wiki_urlencode_matches_wfurlencode() {
        assert_eq!(wiki_urlencode("AC/DC (band)"), "AC/DC_(band)");
        assert_eq!(wiki_urlencode("Côte d'Ivoire"), "C%C3%B4te_d%27Ivoire");
        assert_eq!(wiki_urlencode("Talk:A&B=C?#+"), "Talk:A%26B%3DC%3F%23%2B");
        assert_eq!(wiki_urlencode("a;b@c$d!e*f,g~h"), "a;b@c$d!e*f,g~h");
    }

    #[test]
    fn percent_encoding_round_trips() {
        assert_eq!(percent_encode("a b/(c)~"), "a%20b%2F%28c%29~");
        assert_eq!(percent_decode("K%C3%B6ln_%28x%29"), "Köln_(x)");
        assert_eq!(percent_decode("100%_sure %zz %"), "100%_sure %zz %");
        assert_eq!(percent_decode("%FF"), "\u{FFFD}");
        assert!(matches!(percent_decode("plain"), Cow::Borrowed(_)));
        let original = "Ä & ü/ß ~x";
        assert_eq!(percent_decode(&percent_encode(original)), original);
    }

    #[test]
    fn page_urls() {
        assert_eq!(
            page_url("https://en.wikipedia.org", "AC/DC"),
            "https://en.wikipedia.org/wiki/AC/DC"
        );
        assert_eq!(
            page_url("https://de.wikipedia.org/", "Köln Hbf"),
            "https://de.wikipedia.org/wiki/K%C3%B6ln_Hbf"
        );
        assert_eq!(
            page_url("www.wikidata.org", "Q42"),
            "https://www.wikidata.org/wiki/Q42"
        );
    }

    #[test]
    fn index_urls_and_permalinks() {
        assert_eq!(
            index_url("https://en.wikipedia.org", "A&B", &[("action", "raw")]),
            "https://en.wikipedia.org/w/index.php?title=A%26B&action=raw"
        );
        assert_eq!(
            permalink("https://en.wikipedia.org", "Main Page", 12345),
            "https://en.wikipedia.org/w/index.php?title=Main_Page&oldid=12345"
        );
    }

    #[test]
    fn titles_from_urls() {
        let title = |url: &str| title_from_url(url);
        assert_eq!(
            title("https://de.wikipedia.org/wiki/K%C3%B6ln").as_deref(),
            Some("Köln")
        );
        assert_eq!(
            title("https://en.wikipedia.org/wiki/AC/DC#History").as_deref(),
            Some("AC/DC")
        );
        assert_eq!(
            title("//en.wikipedia.org/wiki/Main_Page?action=history").as_deref(),
            Some("Main Page")
        );
        assert_eq!(
            title("https://en.wikipedia.org/w/index.php?oldid=5&title=Main_Page").as_deref(),
            Some("Main Page")
        );
        assert_eq!(
            title("https://en.wikipedia.org/w/index.php?title=A+B").as_deref(),
            Some("A B")
        );
        assert_eq!(title("https://en.wikipedia.org/w/api.php?title=X"), None);
        assert_eq!(title("https://en.wikipedia.org/"), None);
        assert_eq!(title("https://en.wikipedia.org/wiki/"), None);
        assert_eq!(title("not a url"), None);
    }

    #[test]
    fn url_round_trip() {
        for t in ["Köln", "AC/DC (band)", "A&B=C?", "100% sure", "Talk:X+Y"] {
            let url = page_url("https://en.wikipedia.org", t);
            assert_eq!(title_from_url(&url).as_deref(), Some(t), "{url}");
        }
    }
}
