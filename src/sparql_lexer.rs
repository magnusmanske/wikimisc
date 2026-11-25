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

/// How the brackets `( … )` and `{ … }` of a token list nest, worked out in
/// one pass so that looking a bracket up is cheap.
#[derive(Debug, Default)]
pub(crate) struct Brackets {
    /// For each token that is a bracket, the bracket matching it.
    partner: Vec<Option<usize>>,
    /// For each token, the innermost opening bracket around it.
    parent: Vec<Option<usize>>,
}

impl Brackets {
    /// The brackets of `tokens`, or `None` if they do not balance.
    pub fn new(tokens: &[Token]) -> Option<Self> {
        let mut partner = vec![None; tokens.len()];
        let mut parent = Vec::with_capacity(tokens.len());
        let mut open: Vec<(usize, char)> = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            let closes = if token.is_punct(')') {
                Some('(')
            } else if token.is_punct('}') {
                Some('{')
            } else {
                None
            };
            if let Some(expected) = closes {
                let (opening, kind) = open.pop()?;
                if kind != expected {
                    return None;
                }
                partner[opening] = Some(i);
                partner[i] = Some(opening);
            }
            parent.push(open.last().map(|&(opening, _)| opening));
            for kind in ['(', '{'] {
                if token.is_punct(kind) {
                    open.push((i, kind));
                }
            }
        }
        open.is_empty().then_some(Self { partner, parent })
    }

    /// The bracket matching the one at `tokens[index]`.
    pub fn partner(&self, index: usize) -> Option<usize> {
        self.partner.get(index).copied().flatten()
    }

    /// The innermost opening bracket around `tokens[index]`.
    pub fn parent(&self, index: usize) -> Option<usize> {
        self.parent.get(index).copied().flatten()
    }

    /// The opening brackets around `tokens[index]`, innermost first.
    pub fn ancestors(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.parent(index), |&b| self.parent(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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
    fn test_brackets() {
        // Indices:                            0 1 2 3 4 5 6 7
        let brackets = Brackets::new(&tokenize("{ ( { y } ) x }")).unwrap();
        assert_eq!(brackets.partner(0), Some(7));
        assert_eq!(brackets.partner(7), Some(0));
        assert_eq!(brackets.partner(1), Some(5));
        assert_eq!(brackets.partner(6), None);
        assert_eq!(brackets.parent(0), None);
        assert_eq!(brackets.parent(4), Some(1));
        assert_eq!(brackets.parent(6), Some(0));
        assert_eq!(brackets.ancestors(3).collect::<Vec<_>>(), [2, 1, 0]);
    }

    #[test]
    fn test_unbalanced_brackets() {
        for sparql in ["{ {", "{ } }", "{ ( } )", "( }"] {
            assert!(Brackets::new(&tokenize(sparql)).is_none(), "{sparql}");
        }
    }

    // --- properties -----------------------------------------------------

    /// Characters the tokenizer treats specially, plus arbitrary Unicode so
    /// the shrinker can probe any boundary it likes.
    fn lexer_input() -> impl Strategy<Value = String> {
        const TRICKY: &[char] = &[
            '?', '$', '<', '>', '"', '\'', '#', '\n', '\r', ' ', '\t', '{', '}', '(', ')', '.',
            ',', '\\', '%', '^', '`', '|', '@', ':', '/', '*', '-', '_', 'a', 'Z', '0', 'é', '漢',
            '😀', '\0', '\u{00B7}',
        ];
        let alphabet =
            proptest::prop_oneof![proptest::sample::select(TRICKY.to_vec()), any::<char>(),];
        proptest::collection::vec(alphabet, 0..48).prop_map(|chars| chars.into_iter().collect())
    }

    /// Whitespace and comments are the only text the tokenizer may skip.
    fn gap_is_skippable(gap: &str) -> bool {
        gap.lines().all(|line| {
            let line = line.trim_start();
            line.is_empty() || line.starts_with('#')
        })
    }

    fn is_opening(token: &Token) -> bool {
        token.is_punct('(') || token.is_punct('{')
    }

    fn is_closing(token: &Token) -> bool {
        token.is_punct(')') || token.is_punct('}')
    }

    proptest! {
        /// Tokens are ordered, non-empty and in bounds, each is exactly the
        /// slice it claims, and everything between them is whitespace or a
        /// comment.
        #[test]
        fn tokens_tile_the_input(input in lexer_input()) {
            let tokens = tokenize(&input);
            let mut previous_end = 0;
            for token in &tokens {
                prop_assert!(token.start >= previous_end, "tokens overlap in {input:?}");
                prop_assert!(token.end() <= input.len(), "token past end in {input:?}");
                prop_assert!(!token.text.is_empty(), "empty token in {input:?}");
                prop_assert!(input.is_char_boundary(token.start));
                prop_assert!(input.is_char_boundary(token.end()));
                prop_assert_eq!(token.text, &input[token.start..token.end()]);
                previous_end = token.end();
            }
            let mut cursor = 0;
            for token in &tokens {
                let gap = &input[cursor..token.start];
                prop_assert!(
                    gap_is_skippable(gap),
                    "skipped non-comment text {gap:?} in {input:?}"
                );
                cursor = token.end();
            }
            let tail = &input[cursor..];
            prop_assert!(
                gap_is_skippable(tail),
                "skipped non-comment text {tail:?} in {input:?}"
            );
        }

        /// Token boundaries are real: a token re-tokenizes to itself alone.
        #[test]
        fn tokens_are_self_contained(input in lexer_input()) {
            for token in tokenize(&input) {
                let inner = tokenize(token.text);
                prop_assert_eq!(inner.len(), 1, "token {:?} re-tokenizes as {:?}", token, inner);
                if let Some(single) = inner.first() {
                    prop_assert_eq!(single.kind, token.kind);
                    prop_assert_eq!(single.text, token.text);
                }
            }
        }

        /// A token's kind is consistent with its text.
        #[test]
        fn token_shapes_match_kinds(input in lexer_input()) {
            for token in tokenize(&input) {
                match token.kind {
                    TokenKind::Iri => prop_assert!(
                        token.text.len() >= 2
                            && token.text.starts_with('<')
                            && token.text.ends_with('>'),
                        "{token:?}"
                    ),
                    TokenKind::String => prop_assert!(
                        matches!(token.text.chars().next(), Some('"') | Some('\'')),
                        "{token:?}"
                    ),
                    TokenKind::Var => {
                        prop_assert!(
                            matches!(token.text.chars().next(), Some('?') | Some('$')),
                            "{token:?}"
                        );
                        prop_assert!(token.text.len() > 1, "bare sigil token {token:?}");
                        prop_assert_eq!(token.var_name(), Some(&token.text[1..]));
                    }
                    TokenKind::Word => prop_assert!(
                        token.text.chars().next().is_some_and(is_word_start),
                        "word starting with the wrong character: {token:?}"
                    ),
                    TokenKind::Punct => {
                        prop_assert_eq!(token.text.chars().count(), 1, "{:?}", token);
                    }
                }
            }
        }

        /// Where the brackets balance, every pair is open/close of the same
        /// kind and encloses what lies between it.
        #[test]
        fn brackets_pair_openings_with_closings(input in lexer_input()) {
            let tokens = tokenize(&input);
            let Some(brackets) = Brackets::new(&tokens) else {
                return Ok(());
            };
            for (i, token) in tokens.iter().enumerate() {
                if let Some(parent) = brackets.parent(i) {
                    prop_assert!(parent < i);
                    prop_assert!(is_opening(&tokens[parent]), "{:?}", tokens[parent]);
                    prop_assert!(
                        brackets.partner(parent).is_some_and(|close| close > i),
                        "{:?} is not closed after {i}",
                        tokens[parent]
                    );
                }
                if let Some(j) = brackets.partner(i) {
                    prop_assert_ne!(i, j);
                    prop_assert_eq!(brackets.partner(j), Some(i));
                    let (opening, closing) = if is_opening(token) { (i, j) } else { (j, i) };
                    prop_assert!(is_opening(&tokens[opening]));
                    prop_assert!(is_closing(&tokens[closing]));
                    prop_assert!(opening < closing);
                    prop_assert!(
                        (tokens[opening].is_punct('(') && tokens[closing].is_punct(')'))
                            || (tokens[opening].is_punct('{') && tokens[closing].is_punct('}')),
                        "{:?} pairs with {:?}",
                        tokens[opening],
                        tokens[closing]
                    );
                }
            }
        }
    }
}
