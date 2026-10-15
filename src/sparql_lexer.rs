//! A forgiving SPARQL tokenizer for rewriting queries as text.
//!
//! It recognises just enough of the SPARQL grammar to tell syntax apart from
//! the contents of strings, IRIs and comments — so that a `{`, `#` or
//! `SERVICE` inside one of those is never mistaken for the real thing — and
//! records where each token starts, so a rewrite can splice the original text
//! instead of reserialising it. Whitespace and comments are skipped.
//!
//! It never fails: input that is not valid SPARQL still yields tokens (an
//! unterminated string runs to the end of the input), and it is up to the
//! caller to notice that, for example, braces do not balance.

/// What a token is, as far as rewriting is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// `<http://…>`
    Iri,
    /// A quoted literal, quotes included: `"…"`, `'…'`, `"""…"""`, `'''…'''`.
    String,
    /// `?name` or `$name`.
    Var,
    /// Everything else made of name characters: keywords, prefixed names,
    /// numbers, blank-node labels, language tags (`@en`).
    Word,
    /// A single other character, such as `{`, `.` or `(`.
    Punct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    /// Byte offset of the token in the tokenized text.
    pub start: usize,
}

impl<'a> Token<'a> {
    /// Byte offset just past the token.
    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }

    pub fn is_punct(&self, c: char) -> bool {
        self.kind == TokenKind::Punct && self.text.starts_with(c)
    }

    /// A keyword, compared case-insensitively as SPARQL does.
    pub fn is_keyword(&self, keyword: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(keyword)
    }

    /// The variable's name without `?`/`$`, if this is a variable.
    pub fn var_name(&self) -> Option<&'a str> {
        let text: &'a str = self.text;
        match self.kind {
            TokenKind::Var => text.get(1..),
            _ => None,
        }
    }
}

pub(crate) fn tokenize(sparql: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut pos = 0;
    while let Some(c) = sparql[pos..].chars().next() {
        let rest = &sparql[pos..];
        let (kind, len) = if c.is_whitespace() {
            pos += c.len_utf8();
            continue;
        } else if c == '#' {
            pos += rest.find('\n').unwrap_or(rest.len());
            continue;
        } else if c == '"' || c == '\'' {
            (TokenKind::String, string_len(rest, c))
        } else if c == '<' {
            match iri_len(rest) {
                Some(len) => (TokenKind::Iri, len),
                None => (TokenKind::Punct, 1),
            }
        } else if (c == '?' || c == '$') && rest[1..].chars().next().is_some_and(is_var_char) {
            (TokenKind::Var, 1 + name_len(&rest[1..], is_var_char))
        } else if is_word_start(c) {
            (TokenKind::Word, word_len(rest))
        } else {
            (TokenKind::Punct, c.len_utf8())
        };
        tokens.push(Token {
            kind,
            text: &rest[..len],
            start: pos,
        });
        pos += len;
    }
    tokens
}

/// Length of the string literal at the start of `text`, which begins with
/// `quote`. Escapes are honoured; an unterminated literal runs to the end.
fn string_len(text: &str, quote: char) -> usize {
    let triple: String = [quote; 3].iter().collect();
    let long = text.starts_with(&triple);
    let open = if long { 3 } else { 1 };
    let mut chars = text[open..].char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if long && text[open + i..].starts_with(&triple) {
            return open + i + 3;
        } else if !long && c == quote {
            return open + i + 1;
        }
    }
    text.len()
}

/// Length of the IRI at the start of `text`, or `None` if the `<` is an
/// operator. An IRI cannot contain whitespace or any of `<>"{}|^\``.
fn iri_len(text: &str) -> Option<usize> {
    for (i, c) in text.char_indices().skip(1) {
        match c {
            '>' => return Some(i + 1),
            '<' | '"' | '{' | '}' | '|' | '^' | '`' | '\\' => return None,
            c if c.is_whitespace() => return None,
            _ => {}
        }
    }
    None
}

fn is_var_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\u{00B7}'
}

fn is_word_start(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == ':' || c == '@'
}

fn name_len(text: &str, is_name_char: fn(char) -> bool) -> usize {
    text.char_indices()
        .find(|(_, c)| !is_name_char(*c))
        .map_or(text.len(), |(i, _)| i)
}

/// Length of the word at the start of `text`. A prefixed name may contain
/// `.`, `-`, `:` and `%`, and `\` escapes the next character, but it cannot
/// end in `.` — that is the triple terminator in `wd:Q5.`.
fn word_len(text: &str) -> usize {
    let mut len = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some((j, escaped)) => len = j + escaped.len_utf8(),
                None => len = i + 1,
            }
            continue;
        }
        let is_name_char =
            c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '%' | '@' | '\u{00B7}');
        if !is_name_char || (c == '@' && i > 0) {
            break;
        }
        len = i + c.len_utf8();
    }
    text[..len].trim_end_matches('.').len().max(1)
}

/// Index of the token closing the bracket opened at `tokens[open]`
/// (`{`/`}` or `(`/`)`), or `None` if it is never closed.
pub(crate) fn matching(tokens: &[Token], open: usize) -> Option<usize> {
    let (opening, closing) = match tokens.get(open)?.text {
        "{" => ('{', '}'),
        "(" => ('(', ')'),
        _ => return None,
    };
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        if token.is_punct(opening) {
            depth += 1;
        } else if token.is_punct(closing) {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sparql: &str) -> Vec<&str> {
        tokenize(sparql).iter().map(|t| t.text).collect()
    }

    fn kinds(sparql: &str) -> Vec<TokenKind> {
        tokenize(sparql).iter().map(|t| t.kind).collect()
    }

    #[test]
    fn test_basic_triple() {
        assert_eq!(
            texts("SELECT ?item WHERE { ?item wdt:P31 wd:Q5. }"),
            ["SELECT", "?item", "WHERE", "{", "?item", "wdt:P31", "wd:Q5", ".", "}"]
        );
    }

    #[test]
    fn test_offsets_point_into_the_text() {
        let sparql = "ASK { ?a  <http://x/> 'y' }";
        for token in tokenize(sparql) {
            assert_eq!(&sparql[token.start..token.end()], token.text);
        }
    }

    #[test]
    fn test_syntax_inside_strings_iris_and_comments_is_ignored() {
        let sparql = r#"SELECT ?x { # SERVICE wikibase:label { }
            ?x rdfs:label "{ SERVICE }" , 'it''s' , """a "{" b""" .
            BIND(<http://example.org/#{x}> AS ?y) }"#;
        let braces = tokenize(sparql).iter().filter(|t| t.is_punct('{')).count();
        assert_eq!(braces, 1);
        assert!(!texts(sparql).contains(&"SERVICE"));
    }

    #[test]
    fn test_less_than_is_an_operator_unless_it_forms_an_iri() {
        assert_eq!(
            kinds("FILTER(?a < 3 && ?b <= ?c)"),
            [
                TokenKind::Word,
                TokenKind::Punct,
                TokenKind::Var,
                TokenKind::Punct,
                TokenKind::Word,
                TokenKind::Punct,
                TokenKind::Punct,
                TokenKind::Var,
                TokenKind::Punct,
                TokenKind::Punct,
                TokenKind::Var,
                TokenKind::Punct,
            ]
        );
        assert_eq!(kinds("<http://x/y>"), [TokenKind::Iri]);
    }

    #[test]
    fn test_escaped_quote_and_newline_in_short_string() {
        let sparql = "BIND(\"a\\\"b\" AS ?x) BIND('line\nbreak' AS ?y)";
        assert_eq!(
            texts(sparql),
            [
                "BIND",
                "(",
                "\"a\\\"b\"",
                "AS",
                "?x",
                ")",
                "BIND",
                "(",
                "'line\nbreak'",
                "AS",
                "?y",
                ")"
            ]
        );
    }

    #[test]
    fn test_language_tags_datatypes_and_paths() {
        assert_eq!(
            texts(r#""x"@en-GB "1"^^xsd:integer wdt:P31/wdt:P279* ?y"#),
            [
                "\"x\"",
                "@en-GB",
                "\"1\"",
                "^",
                "^",
                "xsd:integer",
                "wdt:P31",
                "/",
                "wdt:P279",
                "*",
                "?y"
            ]
        );
    }

    #[test]
    fn test_prefixed_names_keep_inner_dots() {
        assert_eq!(texts("ex:a.b ex:c."), ["ex:a.b", "ex:c", "."]);
    }

    #[test]
    fn test_unterminated_input_does_not_panic() {
        for sparql in [
            "SELECT \"open",
            "'''open",
            "<http://open",
            "?",
            "ex:a\\",
            "é ü 漢字 {",
        ] {
            let _ = tokenize(sparql);
        }
    }

    #[test]
    fn test_matching() {
        let tokens = tokenize("{ ( { } ) } }");
        assert_eq!(matching(&tokens, 0), Some(5));
        assert_eq!(matching(&tokens, 1), Some(4));
        assert_eq!(matching(&tokens, 6), None);
        assert_eq!(matching(&tokenize("{ {"), 0), None);
    }
}
