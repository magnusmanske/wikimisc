//! SPARQL results in the W3C [TSV format](https://www.w3.org/TR/sparql11-results-csv-tsv/).
//!
//! TSV is about a third the size of JSON and keeps the RDF term syntax, which
//! makes it the format for very large results. Terms are written in Turtle
//! form: `<IRI>`, `"literal"@lang`, `"literal"^^<datatype>`, bare numbers and
//! booleans — and, from QLever, other typed literals bare too
//! (`POINT(13.38 52.51)`, `1244-01-01T00:00:00Z`). [`decode_cell`] turns a
//! term into its plain value.
//!
//! The header holds the variable names, with their `?`.

/// A decoded TSV result: the variable names and one row of plain values per
/// result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SparqlTsv {
    vars: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl SparqlTsv {
    /// Parses a TSV result. Every cell is decoded with [`decode_cell`]; blank
    /// lines are skipped.
    pub fn parse(body: &str) -> Self {
        let mut lines = body.lines().filter(|line| !line.is_empty());
        let vars = lines
            .next()
            .map(|header| {
                header
                    .split('\t')
                    .map(|var| var.trim().trim_start_matches(['?', '$']).to_string())
                    .collect()
            })
            .unwrap_or_default();
        let rows = lines
            .map(|line| line.split('\t').map(decode_cell).collect())
            .collect();
        Self { vars, rows }
    }

    /// The variable names, without `?`.
    pub fn vars(&self) -> &[String] {
        &self.vars
    }

    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    pub fn into_rows(self) -> Vec<Vec<String>> {
        self.rows
    }
}

/// The plain value of one TSV cell: an IRI without its `<>`, a literal's text
/// with its escapes resolved and any `@lang` or `^^<datatype>` dropped, and
/// anything else (numbers, booleans, blank nodes, an empty cell) as it is.
///
/// Accepts plain values too, so a file that is "TSV" only in its separators
/// reads the same: a cell is only decoded as a literal if it is one whole, so
/// `"Weird" Al` stays as it is.
pub fn decode_cell(cell: &str) -> String {
    let cell = cell.trim();
    if let Some(iri) = cell.strip_prefix('<').and_then(|c| c.strip_suffix('>')) {
        iri.to_string()
    } else if let Some(text) = cell.strip_prefix('"').and_then(unescape_literal) {
        text
    } else {
        cell.to_string()
    }
}

/// The text of a literal, read up to its closing quote, with the Turtle
/// escapes (`\t`, `\"`, `é`, …) resolved. An unknown or broken escape is
/// kept as written. `None` unless the closing quote is followed by nothing, a
/// language tag, or a datatype.
fn unescape_literal(literal: &str) -> Option<String> {
    let mut out = String::with_capacity(literal.len());
    let mut chars = literal.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return is_literal_suffix(chars.as_str()).then_some(out),
            '\\' => match chars.next() {
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{8}'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('f') => out.push('\u{c}'),
                Some(quote @ ('"' | '\'' | '\\')) => out.push(quote),
                Some(u @ ('u' | 'U')) => {
                    let digits: String =
                        chars.by_ref().take(if u == 'u' { 4 } else { 8 }).collect();
                    match u32::from_str_radix(&digits, 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        Some(decoded) => out.push(decoded),
                        None => {
                            out.push('\\');
                            out.push(u);
                            out.push_str(&digits);
                        }
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            },
            c => out.push(c),
        }
    }
    None
}

/// Whether `rest` may follow a literal's closing quote: nothing, `@lang`, or
/// `^^<datatype>`.
fn is_literal_suffix(rest: &str) -> bool {
    if let Some(tag) = rest.strip_prefix('@') {
        !tag.is_empty() && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    } else if let Some(datatype) = rest.strip_prefix("^^") {
        datatype.starts_with('<') && datatype.ends_with('>')
    } else {
        rest.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from WDQS v2 (QLever behind Wikimedia's front end), 2026-10-17.
    /// Typed literals come back bare, not as `"…"^^<datatype>`.
    const WDQS_V2_TSV: &str = "?item\t?label\t?coord\t?pop\t?date\n\
<http://www.wikidata.org/entity/Q64>\t\"Berlin\"@en\tPOINT(13.383333 52.516667)\t3700577.0\t1244-01-01T00:00:00Z\n";

    #[test]
    fn test_parse_wdqs_v2_result() {
        let tsv = SparqlTsv::parse(WDQS_V2_TSV);
        assert_eq!(tsv.vars(), ["item", "label", "coord", "pop", "date"]);
        assert_eq!(
            tsv.rows(),
            [[
                "http://www.wikidata.org/entity/Q64",
                "Berlin",
                "POINT(13.383333 52.516667)",
                "3700577.0",
                "1244-01-01T00:00:00Z"
            ]]
        );
    }

    #[test]
    fn test_decode_cell_typed_literal() {
        assert_eq!(
            decode_cell("\"POINT(1 2)\"^^<http://www.opengis.net/ont/geosparql#wktLiteral>"),
            "POINT(1 2)"
        );
    }

    #[test]
    fn test_parse_skips_blank_lines_and_keeps_empty_cells() {
        let tsv = SparqlTsv::parse("?a\t?b\n\n<x>\t\n");
        assert_eq!(tsv.rows(), [["x", ""]]);
        assert_eq!(tsv.into_rows().len(), 1);
    }

    #[test]
    fn test_parse_empty_body() {
        assert_eq!(SparqlTsv::parse(""), SparqlTsv::default());
    }

    #[test]
    fn test_decode_cell_escapes() {
        assert_eq!(decode_cell(r#""a\tb\nc\"d\\e""#), "a\tb\nc\"d\\e");
        assert_eq!(decode_cell(r#""café \U0001F600""#), "café 😀");
        assert_eq!(decode_cell(r#""bad \uZZZZ \q""#), r"bad \uZZZZ \q");
        // Not a whole literal, so not decoded.
        assert_eq!(decode_cell(r#""cut \"#), r#""cut \"#);
        assert_eq!(decode_cell(r#""Weird" Al"#), r#""Weird" Al"#);
        assert_eq!(decode_cell(r#""a"@"#), r#""a"@"#);
        assert_eq!(decode_cell(r#""a"^^x"#), r#""a"^^x"#);
        assert_eq!(decode_cell(r#""a"@de-CH"#), "a");
    }

    #[test]
    fn test_decode_cell_plain_values() {
        assert_eq!(decode_cell(" Q42 "), "Q42");
        assert_eq!(decode_cell("_:b0"), "_:b0");
        assert_eq!(decode_cell("true"), "true");
        assert_eq!(decode_cell("<>"), "");
        assert_eq!(decode_cell("<"), "<");
    }
}
