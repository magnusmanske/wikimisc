//! Rewrites of Blazegraph-only query features into standard SPARQL 1.1, for
//! running queries written for WDQS v1 (Blazegraph) on QLever, as WDQS v2
//! does.
//!
//! Rewrites splice the original query text rather than reserialising it, so
//! everything they do not touch — formatting, comments, prefixes — survives
//! byte for byte. A query a rewrite does not support is left unchanged; the
//! endpoint then reports its own error, as it would have anyway.
//!
//! # `SERVICE wikibase:label`
//!
//! [`rewrite_label_service`] follows Wikimedia's rewrite of the label service
//! (<https://wikitech.wikimedia.org/wiki/Wikidata_Query_Service/Migration/Rewrite_of_Label_Service>):
//! each requested label or description becomes a chain of `OPTIONAL`s, one
//! per language in priority order, appended to the end of the graph pattern
//! the service sat in, plus a `BIND` of the output variable. As there:
//!
//! - outputs are the service's explicit `rdfs:label` / `schema:description` /
//!   `skos:altLabel` triples (manual mode), or else the variables the
//!   enclosing `SELECT` projects as `?xLabel` / `?xDescription` /
//!   `?xAltLabel` (automatic mode) — Blazegraph binds no others;
//! - labels fall back to `mul`, then to the entity ID (or the value itself,
//!   for something that is not an entity); descriptions do not;
//! - an entity variable is guarded, so that an unbound one cannot match the
//!   label of every entity.
//!
//! Unlike there:
//!
//! - an output nobody needs is not computed at all. The caller says which
//!   outputs it uses; one that is neither used by the caller nor referenced
//!   anywhere in the query except where it is projected or grouped by is
//!   left unbound, and a service with no needed outputs is simply removed.
//!   That keeps the rewritten query as cheap as possible.
//! - `[AUTO_LANGUAGE]` is dropped. Only the WDQS web interface substitutes
//!   it; in a query sent to the endpoint it matches no language.
//! - aliases (`skos:altLabel`) are not rewritten, as they need a copy of the
//!   query's constraints in an aggregating subquery. A query needing one is
//!   left unchanged.

use crate::sparql_endpoint::WIKIDATA_PREFIXES;
use crate::sparql_lexer::{matching, tokenize, Token, TokenKind};
use std::collections::{HashMap, HashSet};
use std::fmt;

const LABEL_SERVICE: &str = "http://wikiba.se/ontology#label";
const SERVICE_PARAM: &str = "http://www.bigdata.com/rdf#serviceParam";
const LANGUAGE: &str = "http://wikiba.se/ontology#language";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const SCHEMA_DESCRIPTION: &str = "http://schema.org/description";
const SKOS_ALT_LABEL: &str = "http://www.w3.org/2004/02/skos/core#altLabel";
const AUTO_LANGUAGE: &str = "[AUTO_LANGUAGE]";
const WIKIDATA_ENTITY: &str = "http://www.wikidata.org/entity/";

/// The outcome of [`rewrite_label_service`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelServiceRewrite {
    /// The query uses no label service.
    Unchanged,
    /// The rewritten query.
    Rewritten(String),
    /// The query uses the label service in a way this rewrite does not
    /// support; it should be sent as it is.
    Unsupported(UnsupportedLabelService),
}

/// Why [`rewrite_label_service`] left a query unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnsupportedLabelService {
    /// Brackets do not balance, or the service has no body.
    Syntax,
    /// Blazegraph's named subqueries (`WITH { … } AS %name`, `INCLUDE %name`).
    NamedSubqueries,
    /// The service is not inside a `SELECT` query or subquery.
    NotInSelect,
    /// The service contains something other than language parameters and
    /// label, description or alias triples.
    ServiceContent,
    /// The service names no language.
    MissingLanguage,
    /// A language is not a plain language code.
    Language,
    /// An alias output (`skos:altLabel`, `?xAltLabel`) is needed.
    Aliases,
    /// An output variable is also bound elsewhere in its graph pattern, or by
    /// another label service.
    PreboundOutput,
}

impl fmt::Display for UnsupportedLabelService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Syntax => "unbalanced brackets or a service without a body",
            Self::NamedSubqueries => "named subqueries (WITH … AS %name / INCLUDE %name)",
            Self::NotInSelect => "label service outside a SELECT query",
            Self::ServiceContent => "unsupported content in the label service",
            Self::MissingLanguage => "label service without a language",
            Self::Language => "malformed language in the label service",
            Self::Aliases => "alias (skos:altLabel) outputs",
            Self::PreboundOutput => "label output variable bound elsewhere in the query",
        })
    }
}

/// Rewrites every `SERVICE wikibase:label` in `sparql` into standard SPARQL
/// 1.1; see the [module documentation](self).
///
/// `is_needed` is asked about each output variable (without `?`) that the
/// query itself does not use apart from projecting or grouping by it: return
/// `true` if the caller reads that variable from the results. Pass
/// `|_| true` to rewrite every output.
pub fn rewrite_label_service(
    sparql: &str,
    is_needed: impl Fn(&str) -> bool,
) -> LabelServiceRewrite {
    let mut rewriter = LabelServiceRewriter::new(sparql);
    if !rewriter.has_label_service() {
        return LabelServiceRewrite::Unchanged;
    }
    match rewriter.rewrite(&is_needed) {
        Ok(Some(query)) => LabelServiceRewrite::Rewritten(query),
        Ok(None) => LabelServiceRewrite::Unchanged,
        Err(reason) => LabelServiceRewrite::Unsupported(reason),
    }
}

type Result<T> = std::result::Result<T, UnsupportedLabelService>;

/// A `SELECT` and the token ranges of its clauses.
#[derive(Debug)]
struct SelectScope {
    /// Tokens of the projection, between `SELECT` and the `WHERE` group.
    projection: (usize, usize),
    /// The `{` and `}` of the `WHERE` group.
    group: (usize, usize),
    /// Tokens of the `GROUP BY` clause, if any.
    group_by: Option<(usize, usize)>,
}

impl SelectScope {
    fn contains(&self, index: usize) -> bool {
        self.group.0 < index && index < self.group.1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Label,
    Description,
    Alias,
}

/// One output of a label service: `entity` (a variable or IRI, as written)
/// has its `kind` bound to `?output`.
#[derive(Debug)]
struct Output<'a> {
    output: &'a str,
    kind: Kind,
    entity: String,
}

/// One `SERVICE wikibase:label { … }`.
#[derive(Debug)]
struct Service<'a> {
    /// The `SERVICE` token and the `}` closing its body.
    span: (usize, usize),
    /// The `{` and `}` of the graph pattern the service is in.
    group: (usize, usize),
    languages: Vec<&'a str>,
    outputs: Vec<Output<'a>>,
}

struct LabelServiceRewriter<'a> {
    sparql: &'a str,
    tokens: Vec<Token<'a>>,
    prefixes: HashMap<&'a str, &'a str>,
    selects: Vec<SelectScope>,
}

impl<'a> LabelServiceRewriter<'a> {
    fn new(sparql: &'a str) -> Self {
        let tokens = tokenize(sparql);
        Self {
            sparql,
            prefixes: declared_prefixes(&tokens),
            tokens,
            selects: Vec::new(),
        }
    }

    fn has_label_service(&self) -> bool {
        (0..self.tokens.len()).any(|i| self.service_name_at(i).is_some())
    }

    /// If `tokens[i]` starts `SERVICE [SILENT] wikibase:label`, the index of
    /// the service name.
    fn service_name_at(&self, i: usize) -> Option<usize> {
        if !self.tokens.get(i)?.is_keyword("SERVICE") {
            return None;
        }
        let mut name = i + 1;
        if self.tokens.get(name)?.is_keyword("SILENT") {
            name += 1;
        }
        (self.resolve(self.tokens.get(name)?).as_deref() == Some(LABEL_SERVICE)).then_some(name)
    }

    /// The rewritten query. Only called if there is a label service.
    fn rewrite(&mut self, is_needed: &dyn Fn(&str) -> bool) -> Result<Option<String>> {
        if self.uses_named_subqueries() {
            return Err(UnsupportedLabelService::NamedSubqueries);
        }
        self.selects = self.select_scopes()?;
        let services = self.services()?;
        let mut taken: HashSet<String> = self
            .tokens
            .iter()
            .filter_map(Token::var_name)
            .map(str::to_string)
            .collect();
        let mut edits = Vec::new();
        let mut insertions: Vec<(usize, Vec<String>)> = Vec::new();
        let mut bound: HashSet<&str> = HashSet::new();
        for service in &services {
            edits.push(self.removal(service));
            let needed: Vec<&Output> = service
                .outputs
                .iter()
                .filter(|o| self.is_used_in_query(o.output, service) || is_needed(o.output))
                .collect();
            if needed.is_empty() {
                continue;
            }
            if needed.iter().any(|o| o.kind == Kind::Alias) {
                return Err(UnsupportedLabelService::Aliases);
            }
            // Bound elsewhere in the group, or by another label service.
            if needed
                .iter()
                .any(|o| self.is_prebound(o.output, service) || !bound.insert(o.output))
            {
                return Err(UnsupportedLabelService::PreboundOutput);
            }
            let lines = self.replacement(service, &needed, &mut taken);
            match insertions.iter_mut().find(|(at, _)| *at == service.group.1) {
                Some((_, existing)) => existing.extend(lines),
                None => insertions.push((service.group.1, lines)),
            }
        }
        for (group_close, lines) in insertions {
            edits.push(self.insertion(group_close, &lines));
        }
        // Apply from the back, so earlier offsets stay valid. Edits never
        // overlap; should a removal and an insertion start at the same
        // offset, remove first, so the inserted text survives.
        edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut query = self.sparql.to_string();
        for (start, end, text) in edits {
            query.replace_range(start..end, &text);
        }
        Ok(Some(query))
    }

    // ── analysis ────────────────────────────────────────────────────────────

    fn select_scopes(&self) -> Result<Vec<SelectScope>> {
        let mut scopes = Vec::new();
        for (i, token) in self.tokens.iter().enumerate() {
            if !token.is_keyword("SELECT") {
                continue;
            }
            // The WHERE group is the first `{` outside parentheses; a
            // projection may contain `(EXISTS { … } AS ?x)`.
            let mut depth = 0usize;
            let mut open = None;
            for (j, t) in self.tokens.iter().enumerate().skip(i + 1) {
                if t.is_punct('(') {
                    depth += 1;
                } else if t.is_punct(')') {
                    depth = depth.saturating_sub(1);
                } else if t.is_punct('{') && depth == 0 {
                    open = Some(j);
                    break;
                }
            }
            let Some(open) = open else { continue };
            let close = matching(&self.tokens, open).ok_or(UnsupportedLabelService::Syntax)?;
            let group_by = self.group_by_clause(close + 1);
            scopes.push(SelectScope {
                projection: (i + 1, open),
                group: (open, close),
                group_by,
            });
        }
        Ok(scopes)
    }

    fn group_by_clause(&self, start: usize) -> Option<(usize, usize)> {
        let group = self.tokens.get(start)?;
        let by = self.tokens.get(start + 1)?;
        if !(group.is_keyword("GROUP") && by.is_keyword("BY")) {
            return None;
        }
        let end = self.tokens[start + 2..]
            .iter()
            .position(|t| {
                t.is_punct('}')
                    || ["HAVING", "ORDER", "LIMIT", "OFFSET", "VALUES"]
                        .iter()
                        .any(|k| t.is_keyword(k))
            })
            .map_or(self.tokens.len(), |p| start + 2 + p);
        Some((start, end))
    }

    fn uses_named_subqueries(&self) -> bool {
        self.tokens.windows(2).any(|pair| {
            (pair[0].is_keyword("WITH") && pair[1].is_punct('{'))
                || (pair[0].is_keyword("INCLUDE") && pair[1].is_punct('%'))
        })
    }

    fn services(&self) -> Result<Vec<Service<'a>>> {
        let mut services = Vec::new();
        for i in 0..self.tokens.len() {
            let Some(name) = self.service_name_at(i) else {
                continue;
            };
            let open = name + 1;
            if !self.tokens.get(open).is_some_and(|t| t.is_punct('{')) {
                return Err(UnsupportedLabelService::Syntax);
            }
            let close = matching(&self.tokens, open).ok_or(UnsupportedLabelService::Syntax)?;
            let scope = self
                .scope_of(i)
                .ok_or(UnsupportedLabelService::NotInSelect)?;
            let (languages, manual) = self.parse_body(open + 1, close)?;
            let outputs = if manual.is_empty() {
                self.automatic_outputs(scope)
            } else {
                manual
            };
            services.push(Service {
                span: (i, close),
                group: self.enclosing_group(i)?,
                languages,
                outputs,
            });
        }
        Ok(services)
    }

    /// Languages and manual outputs from the service body
    /// `tokens[start..end]`.
    fn parse_body(&self, start: usize, end: usize) -> Result<(Vec<&'a str>, Vec<Output<'a>>)> {
        let mut languages = Vec::new();
        let mut outputs = Vec::new();
        for statement in self.tokens[start..end].split(|t| t.is_punct('.')) {
            let Some((subject, rest)) = statement.split_first() else {
                continue;
            };
            for pair in rest.split(|t| t.is_punct(';')) {
                let [predicate, object] = pair else {
                    if pair.is_empty() {
                        continue;
                    }
                    return Err(UnsupportedLabelService::ServiceContent);
                };
                let predicate_iri = self.resolve(predicate);
                if self.resolve(subject).as_deref() == Some(SERVICE_PARAM)
                    && predicate_iri.as_deref() == Some(LANGUAGE)
                {
                    if object.kind != TokenKind::String {
                        return Err(UnsupportedLabelService::Language);
                    }
                    for language in string_value(object.text).split(',').map(str::trim) {
                        let is_code = language
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-');
                        if !is_code && language != AUTO_LANGUAGE {
                            return Err(UnsupportedLabelService::Language);
                        }
                        languages.push(language);
                    }
                    continue;
                }
                let kind = match predicate_iri.as_deref() {
                    Some(RDFS_LABEL) => Kind::Label,
                    Some(SCHEMA_DESCRIPTION) => Kind::Description,
                    Some(SKOS_ALT_LABEL) => Kind::Alias,
                    _ => return Err(UnsupportedLabelService::ServiceContent),
                };
                let entity_ok = subject.kind == TokenKind::Var || self.resolve(subject).is_some();
                match (object.var_name(), entity_ok) {
                    (Some(output), true) => outputs.push(Output {
                        output,
                        kind,
                        entity: subject.text.to_string(),
                    }),
                    _ => return Err(UnsupportedLabelService::ServiceContent),
                }
            }
        }
        if languages.is_empty() {
            return Err(UnsupportedLabelService::MissingLanguage);
        }
        Ok((languages, outputs))
    }

    /// Variables the scope projects as such (not inside an expression) whose
    /// names end in `Label`, `Description` or `AltLabel`.
    fn automatic_outputs(&self, scope: &SelectScope) -> Vec<Output<'a>> {
        self.bare_projection(scope)
            .filter_map(|output| {
                [
                    ("AltLabel", Kind::Alias),
                    ("Label", Kind::Label),
                    ("Description", Kind::Description),
                ]
                .into_iter()
                .find_map(|(suffix, kind)| {
                    let base = output.strip_suffix(suffix).filter(|b| !b.is_empty())?;
                    Some(Output {
                        output,
                        kind,
                        entity: format!("?{base}"),
                    })
                })
            })
            .collect()
    }

    fn bare_projection<'s>(&'s self, scope: &SelectScope) -> impl Iterator<Item = &'a str> + 's {
        let mut depth = 0usize;
        self.tokens[scope.projection.0..scope.projection.1]
            .iter()
            .filter_map(move |t| {
                if t.is_punct('(') {
                    depth += 1;
                } else if t.is_punct(')') {
                    depth = depth.saturating_sub(1);
                } else if depth == 0 {
                    return t.var_name();
                }
                None
            })
    }

    /// The innermost `SELECT` whose `WHERE` group contains `tokens[index]`.
    /// Scopes are in source order, so a nested one comes after its parent.
    fn scope_of(&self, index: usize) -> Option<&SelectScope> {
        self.selects.iter().rev().find(|s| s.contains(index))
    }

    /// The innermost `{ … }` around `tokens[index]`.
    fn enclosing_group(&self, index: usize) -> Result<(usize, usize)> {
        let mut stack = Vec::new();
        for (i, token) in self.tokens[..index].iter().enumerate() {
            if token.is_punct('{') {
                stack.push(i);
            } else if token.is_punct('}') {
                stack.pop();
            }
        }
        let open = stack.pop().ok_or(UnsupportedLabelService::Syntax)?;
        let close = matching(&self.tokens, open).ok_or(UnsupportedLabelService::Syntax)?;
        Ok((open, close))
    }

    /// Whether the query references `?output` anywhere besides the service
    /// itself, the bare projection of the service's `SELECT`, and that
    /// `SELECT`'s `GROUP BY` — places where an unbound variable is harmless.
    fn is_used_in_query(&self, output: &str, service: &Service) -> bool {
        let Some(scope) = self.scope_of(service.span.0) else {
            return true;
        };
        let mut paren_depth = 0usize;
        self.tokens.iter().enumerate().any(|(i, t)| {
            let in_projection = scope.projection.0 <= i && i < scope.projection.1;
            if in_projection && t.is_punct('(') {
                paren_depth += 1;
            } else if in_projection && t.is_punct(')') {
                paren_depth = paren_depth.saturating_sub(1);
            }
            if t.var_name() != Some(output) {
                return false;
            }
            let in_service = service.span.0 <= i && i <= service.span.1;
            let bare_projected = in_projection && paren_depth == 0;
            let grouped = scope.group_by.is_some_and(|(s, e)| s <= i && i < e);
            !(in_service || bare_projected || grouped)
        })
    }

    /// Whether `?output` is bound somewhere in the service's graph pattern
    /// other than in a `FILTER`, which would clash with the rewrite's `BIND`.
    fn is_prebound(&self, output: &str, service: &Service) -> bool {
        let (open, close) = service.group;
        let mut i = open + 1;
        while i < close {
            let token = &self.tokens[i];
            if i == service.span.0 {
                i = service.span.1 + 1;
                continue;
            }
            if token.is_keyword("FILTER") {
                i = self.filter_end(i + 1).map_or(close, |end| end + 1);
                continue;
            }
            if token.var_name() == Some(output) {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Index of the last token of the constraint after `FILTER`:
    /// `( … )`, `fn( … )` or `[NOT] EXISTS { … }`.
    fn filter_end(&self, start: usize) -> Option<usize> {
        let mut i = start;
        loop {
            let token = self.tokens.get(i)?;
            if token.is_punct('(') || token.is_punct('{') {
                return matching(&self.tokens, i);
            }
            i += 1;
        }
    }

    /// The full IRI a token names, if it is an IRI or a prefixed name with a
    /// known prefix.
    fn resolve(&self, token: &Token) -> Option<String> {
        match token.kind {
            TokenKind::Iri => token.text.get(1..token.text.len() - 1).map(str::to_string),
            TokenKind::Word => {
                let (prefix, local) = token.text.split_once(':')?;
                let namespace = self.prefixes.get(prefix).copied().or_else(|| {
                    WIKIDATA_PREFIXES
                        .iter()
                        .find(|(name, _)| *name == prefix)
                        .map(|(_, iri)| *iri)
                })?;
                Some(format!("{namespace}{local}"))
            }
            _ => None,
        }
    }

    /// How to write `iri` in this query: as a prefixed name if `name`'s
    /// prefix means the same here, else in full.
    fn term(&self, name: &str, iri: &str) -> String {
        let token = Token {
            kind: TokenKind::Word,
            text: name,
            start: 0,
        };
        if self.resolve(&token).as_deref() == Some(iri) {
            name.to_string()
        } else {
            format!("<{iri}>")
        }
    }

    // ── generation ──────────────────────────────────────────────────────────

    /// SPARQL replacing `service`, for the outputs that are needed.
    fn replacement(
        &self,
        service: &Service,
        needed: &[&Output],
        taken: &mut HashSet<String>,
    ) -> Vec<String> {
        let languages = normalized_languages(&service.languages);
        let label = self.term("rdfs:label", RDFS_LABEL);
        let description = self.term("schema:description", SCHEMA_DESCRIPTION);
        let mut lines = Vec::new();
        // Per entity: its guarded subject; per entity and kind: the value.
        let mut subjects: HashMap<&str, String> = HashMap::new();
        let mut values: HashMap<(&str, Kind), String> = HashMap::new();
        for output in needed {
            let entity = output.entity.as_str();
            let is_var = entity.starts_with('?') || entity.starts_with('$');
            let base = if is_var { &entity[1..] } else { output.output };
            let subject = match subjects.get(entity) {
                Some(subject) => subject.clone(),
                None if is_var => {
                    let subject = fresh(taken, &format!("{base}__label_subject"));
                    lines.push(format!(
                        "BIND(IF(BOUND({entity}), {entity}, \"unbound\") AS ?{subject})"
                    ));
                    subjects.insert(entity, subject.clone());
                    subject
                }
                None => entity.to_string(),
            };
            let subject = if is_var {
                format!("?{subject}")
            } else {
                subject
            };
            let (predicate, suffix, kind_languages) = match output.kind {
                Kind::Label => (&label, "label_value", with_mul(&languages)),
                // Aliases are rejected before generation.
                Kind::Description | Kind::Alias => {
                    (&description, "description_value", languages.clone())
                }
            };
            let value = match values.get(&(entity, output.kind)) {
                Some(value) => value.clone(),
                None => {
                    let value = fresh(taken, &format!("{base}__{suffix}"));
                    for language in &kind_languages {
                        lines.push(format!(
                            "OPTIONAL {{ {subject} {predicate} ?{value} . FILTER(LANG(?{value}) = \"{language}\") }}"
                        ));
                    }
                    values.insert((entity, output.kind), value.clone());
                    value
                }
            };
            lines.push(match output.kind {
                Kind::Label if is_var => format!(
                    "BIND(IF(BOUND(?{value}), ?{value}, IF(ISURI({entity}) && STRSTARTS(STR({entity}), \"{WIKIDATA_ENTITY}\"), STRAFTER(STR({entity}), \"entity/\"), STR({entity}))) AS ?{})",
                    output.output
                ),
                Kind::Label => format!(
                    "BIND(IF(BOUND(?{value}), ?{value}, STRAFTER(STR({entity}), \"entity/\")) AS ?{})",
                    output.output
                ),
                Kind::Description | Kind::Alias => format!("BIND(?{value} AS ?{})", output.output),
            });
        }
        lines
    }

    /// The text range to delete for `service`: the service and a `.` right
    /// after it, and the whole line if nothing else is left on it.
    fn removal(&self, service: &Service) -> (usize, usize, String) {
        let start = self.tokens[service.span.0].start;
        let mut end = self.tokens[service.span.1].end();
        if let Some(dot) = self
            .tokens
            .get(service.span.1 + 1)
            .filter(|t| t.is_punct('.'))
        {
            end = dot.end();
        }
        let line_start = self.sparql[..start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = self.sparql[end..]
            .find('\n')
            .map_or(self.sparql.len(), |i| end + i);
        if self.sparql[line_start..start].trim().is_empty()
            && self.sparql[end..line_end].trim().is_empty()
        {
            (
                line_start,
                (line_end + 1).min(self.sparql.len()),
                String::new(),
            )
        } else {
            (start, end, String::new())
        }
    }

    /// The text to insert before the `}` at `tokens[group_close]`.
    fn insertion(&self, group_close: usize, lines: &[String]) -> (usize, usize, String) {
        let at = self.tokens[group_close].start;
        let line_start = self.sparql[..at].rfind('\n').map_or(0, |i| i + 1);
        let before = &self.sparql[line_start..at];
        if before.trim().is_empty() {
            // `}` on its own line: add indented lines above it.
            let indent = format!("{before}  ");
            let text: String = lines.iter().map(|l| format!("{indent}{l}\n")).collect();
            (line_start, line_start, text)
        } else {
            (at, at, format!(" {} ", lines.join(" ")))
        }
    }
}

/// The prefixes the query declares, by name (without `:`).
fn declared_prefixes<'a>(tokens: &[Token<'a>]) -> HashMap<&'a str, &'a str> {
    tokens
        .windows(3)
        .filter(|w| w[0].is_keyword("PREFIX") && w[2].kind == TokenKind::Iri)
        .filter_map(|w| {
            let name = w[1].text.strip_suffix(':')?;
            let iri = w[2].text.get(1..w[2].text.len() - 1)?;
            Some((name, iri))
        })
        .collect()
}

/// The text of a string literal token, without its quotes.
fn string_value(literal: &str) -> &str {
    for quotes in ["\"\"\"", "'''", "\"", "'"] {
        if let Some(inner) = literal
            .strip_prefix(quotes)
            .and_then(|s| s.strip_suffix(quotes))
        {
            return inner;
        }
    }
    literal
}

/// The service's languages in priority order, without empties, duplicates
/// and `[AUTO_LANGUAGE]`. (Each is a plain language code, checked when the
/// service was parsed, so no other text reaches the generated SPARQL.)
fn normalized_languages(languages: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for language in languages {
        if !language.is_empty() && *language != AUTO_LANGUAGE && !out.iter().any(|l| l == language)
        {
            out.push((*language).to_string());
        }
    }
    out
}

/// Labels fall back to `mul` after the requested languages.
fn with_mul(languages: &[String]) -> Vec<String> {
    let mut out = languages.to_vec();
    if !out.iter().any(|l| l == "mul") {
        out.push("mul".to_string());
    }
    out
}

/// `name`, or `name_2`, `name_3`, … — whichever is not yet a variable.
fn fresh(taken: &mut HashSet<String>, name: &str) -> String {
    let mut candidate = name.to_string();
    let mut n = 1;
    while taken.contains(&candidate) {
        n += 1;
        candidate = format!("{name}_{n}");
    }
    taken.insert(candidate.clone());
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rewrites with `needed` as the caller's variables, asserting that the
    /// result is valid SPARQL 1.1 (with the prefixes WDQS predeclares).
    fn rewrite(sparql: &str, needed: &[&str]) -> LabelServiceRewrite {
        let result = rewrite_label_service(sparql, |v| needed.contains(&v));
        if let LabelServiceRewrite::Rewritten(query) = &result {
            assert_valid(query);
        }
        result
    }

    fn rewritten(sparql: &str, needed: &[&str]) -> String {
        match rewrite(sparql, needed) {
            LabelServiceRewrite::Rewritten(query) => query,
            other => panic!("expected a rewrite, got {other:?}"),
        }
    }

    fn assert_valid(query: &str) {
        let mut parser = spargebra::SparqlParser::new();
        for (name, iri) in WIDATA_PREFIXES_FOR_TESTS {
            parser = parser.with_prefix(*name, *iri).unwrap();
        }
        if let Err(e) = parser.parse_query(query) {
            panic!("rewritten query is not valid SPARQL 1.1: {e}\n{query}");
        }
    }

    const WIDATA_PREFIXES_FOR_TESTS: &[(&str, &str)] = WIKIDATA_PREFIXES;

    #[test]
    fn test_query_without_label_service_is_unchanged() {
        for sparql in [
            "SELECT ?item WHERE { ?item wdt:P31 wd:Q5 }",
            // Look-alikes in a comment, a string and an IRI.
            "SELECT ?x WHERE { # SERVICE wikibase:label { }\n ?x rdfs:label \"SERVICE wikibase:label {\" }",
            "SELECT ?x WHERE { SERVICE <http://example.org/label> { ?x ?p ?o } }",
            // Not valid SPARQL, but no label service either.
            "SELECT ?x WHERE { {",
        ] {
            assert_eq!(rewrite(sparql, &[]), LabelServiceRewrite::Unchanged, "{sparql}");
        }
    }

    #[test]
    fn test_unused_outputs_remove_the_service() {
        let sparql = "SELECT ?item ?itemLabel WHERE {\n  ?item wdt:P31 wd:Q5 .\n  SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" . }\n}";
        assert_eq!(
            rewritten(sparql, &[]),
            "SELECT ?item ?itemLabel WHERE {\n  ?item wdt:P31 wd:Q5 .\n}"
        );
    }

    #[test]
    fn test_group_by_alone_does_not_need_the_label() {
        let sparql = "SELECT ?item ?itemLabel (COUNT(?x) AS ?n) WHERE { ?item wdt:P31 ?x . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\". } } GROUP BY ?item ?itemLabel";
        let query = rewritten(sparql, &[]);
        assert!(!query.contains("SERVICE"));
        assert!(!query.contains("OPTIONAL"));
    }

    #[test]
    fn test_needed_label_is_rewritten() {
        let sparql = "SELECT ?item ?itemLabel WHERE {\n  ?item wdt:P31 wd:Q5 .\n  SERVICE wikibase:label { bd:serviceParam wikibase:language \"de,en\" . }\n}";
        assert_eq!(
            rewritten(sparql, &["itemLabel"]),
            "SELECT ?item ?itemLabel WHERE {\n  ?item wdt:P31 wd:Q5 .\n  \
             BIND(IF(BOUND(?item), ?item, \"unbound\") AS ?item__label_subject)\n  \
             OPTIONAL { ?item__label_subject rdfs:label ?item__label_value . FILTER(LANG(?item__label_value) = \"de\") }\n  \
             OPTIONAL { ?item__label_subject rdfs:label ?item__label_value . FILTER(LANG(?item__label_value) = \"en\") }\n  \
             OPTIONAL { ?item__label_subject rdfs:label ?item__label_value . FILTER(LANG(?item__label_value) = \"mul\") }\n  \
             BIND(IF(BOUND(?item__label_value), ?item__label_value, IF(ISURI(?item) && STRSTARTS(STR(?item), \"http://www.wikidata.org/entity/\"), STRAFTER(STR(?item), \"entity/\"), STR(?item))) AS ?itemLabel)\n}"
        );
    }

    #[test]
    fn test_use_inside_the_query_makes_an_output_needed() {
        for clause in ["ORDER BY ?itemLabel", "HAVING(STRLEN(?itemLabel) > 1)"] {
            let group_by = if clause.starts_with("HAVING") {
                "GROUP BY ?item ?itemLabel "
            } else {
                ""
            };
            let sparql = format!(
                "SELECT ?item ?itemLabel WHERE {{ ?item wdt:P31 wd:Q5 . SERVICE wikibase:label {{ bd:serviceParam wikibase:language \"en\". }} }} {group_by}{clause}"
            );
            assert!(
                rewritten(&sparql, &[]).contains("AS ?itemLabel)"),
                "{clause}"
            );
        }
        let filtered = "SELECT ?item ?itemLabel WHERE { ?item wdt:P31 wd:Q5 . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\". } FILTER(CONTAINS(?itemLabel, \"a\")) }";
        assert!(rewritten(filtered, &[]).contains("AS ?itemLabel)"));
    }

    #[test]
    fn test_label_used_only_in_an_expression_is_not_an_output() {
        // Blazegraph binds only bare projected variables, so `?itemLabel`
        // here is unbound on WDQS v1 too, and the service can go.
        let sparql = "SELECT ?item (SAMPLE(?itemLabel) AS ?l) WHERE { ?item wdt:P31 wd:Q5 . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\". } } GROUP BY ?item";
        let query = rewritten(sparql, &[]);
        assert!(
            !query.contains("SERVICE") && !query.contains("OPTIONAL"),
            "{query}"
        );
    }

    #[test]
    fn test_description_has_no_mul_fallback() {
        let sparql = "SELECT ?item ?itemDescription WHERE { ?item wdt:P31 wd:Q5 . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\". } }";
        let query = rewritten(sparql, &["itemDescription"]);
        assert!(query.contains("schema:description ?item__description_value . FILTER(LANG(?item__description_value) = \"en\")"));
        assert!(!query.contains("\"mul\""));
        assert!(query.contains("BIND(?item__description_value AS ?itemDescription)"));
    }

    #[test]
    fn test_manual_mode() {
        let sparql = "SELECT ?item ?name WHERE { ?item wdt:P31 wd:Q5 . SERVICE wikibase:label { bd:serviceParam wikibase:language \"fr\" . ?item rdfs:label ?name . wd:Q42 schema:description ?d } }";
        let query = rewritten(sparql, &["name", "d"]);
        assert!(query.contains("AS ?name)"));
        // A fixed entity needs no guard, and falls back to its ID directly.
        assert!(query.contains("OPTIONAL { wd:Q42 schema:description ?d__description_value"));
        assert!(query.contains("BIND(?d__description_value AS ?d)"));
    }

    #[test]
    fn test_languages_are_normalized() {
        // `[AUTO_LANGUAGE]` matches nothing outside the WDQS UI; parameters
        // concatenate; duplicates and empties go.
        let sparql = "SELECT ?itemLabel WHERE { ?item wdt:P31 wd:Q5 . SERVICE wikibase:label { bd:serviceParam wikibase:language \"[AUTO_LANGUAGE], de,,en\" . bd:serviceParam wikibase:language \"de,mul\" } }";
        let query = rewritten(sparql, &["itemLabel"]);
        let languages: Vec<&str> = query
            .match_indices("= \"")
            .map(|(i, _)| &query[i + 3..i + 3 + query[i + 3..].find('"').unwrap()])
            .collect();
        assert_eq!(languages, ["de", "en", "mul"]);
    }

    #[test]
    fn test_service_in_a_nested_group_is_replaced_there() {
        let sparql = "SELECT ?item ?other ?otherLabel WHERE { ?item wdt:P31 wd:Q5 . OPTIONAL { ?item wdt:P26 ?other . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" } } }";
        let query = rewritten(sparql, &["otherLabel"]);
        let optional = query.find("OPTIONAL {").unwrap();
        let bind = query.find("AS ?otherLabel)").unwrap();
        let inner_close = query[optional..].find(" } }").unwrap() + optional;
        assert!(bind < inner_close, "{query}");
    }

    #[test]
    fn test_service_listed_first_is_replaced_at_the_end_of_its_group() {
        // A BIND only sees what comes before it in its group.
        let sparql = "SELECT ?item ?itemLabel WHERE { SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" } ?item wdt:P31 wd:Q5 }";
        let query = rewritten(sparql, &["itemLabel"]);
        assert!(
            query.find("wdt:P31").unwrap() < query.find("BIND").unwrap(),
            "{query}"
        );
    }

    #[test]
    fn test_trailing_dot_and_full_iris() {
        let sparql = "SELECT ?item ?itemLabel WHERE { SERVICE <http://wikiba.se/ontology#label> { <http://www.bigdata.com/rdf#serviceParam> <http://wikiba.se/ontology#language> \"en\" } . ?item wdt:P31 wd:Q5 }";
        let query = rewritten(sparql, &[]);
        assert_eq!(
            query,
            "SELECT ?item ?itemLabel WHERE {  ?item wdt:P31 wd:Q5 }"
        );
    }

    #[test]
    fn test_generated_names_avoid_existing_variables() {
        let sparql = "SELECT ?item ?itemLabel ?item__label_value WHERE { ?item wdt:P31 wd:Q5 . BIND(1 AS ?item__label_value) SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" } }";
        let query = rewritten(sparql, &["itemLabel"]);
        assert!(
            query.contains("rdfs:label ?item__label_value_2 ."),
            "{query}"
        );
    }

    #[test]
    fn test_services_in_outer_query_and_subquery() {
        let sparql = "SELECT ?item ?itemLabel ?n WHERE { ?item wdt:P31 wd:Q5 . { SELECT ?item ?country ?countryLabel WHERE { ?item wdt:P27 ?country . SERVICE wikibase:label { bd:serviceParam wikibase:language \"de\" } } } SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" } }";
        let query = rewritten(sparql, &["itemLabel"]);
        assert!(query.contains("AS ?itemLabel)"), "{query}");
        // The subquery's ?countryLabel is not projected by the outer query.
        assert!(!query.contains("AS ?countryLabel)"), "{query}");
        assert!(!query.contains("SERVICE"), "{query}");
    }

    #[test]
    fn test_one_output_claimed_by_two_services_is_unsupported() {
        let svc = "SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" }";
        let sparql = format!("SELECT ?item ?itemLabel WHERE {{ ?item wdt:P31 wd:Q5 . OPTIONAL {{ ?item wdt:P26 ?x . {svc} }} {svc} }}");
        assert_eq!(
            rewrite(&sparql, &["itemLabel"]),
            LabelServiceRewrite::Unsupported(UnsupportedLabelService::PreboundOutput)
        );
    }

    #[test]
    fn test_select_star_has_no_outputs() {
        let sparql = "SELECT * WHERE { ?item wdt:P31 wd:Q5 . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" } }";
        assert!(!rewritten(sparql, &["itemLabel"]).contains("SERVICE"));
    }

    #[test]
    fn test_rebound_wikibase_prefix_is_not_the_label_service() {
        let sparql = "PREFIX wikibase: <http://example.org/> SELECT ?x WHERE { SERVICE wikibase:label { ?x ?p ?o } }";
        assert_eq!(rewrite(sparql, &[]), LabelServiceRewrite::Unchanged);
    }

    #[test]
    fn test_unsupported_cases() {
        let svc = "SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" }";
        for (sparql, needed, reason) in [
            (
                format!("SELECT ?item ?itemAltLabel WHERE {{ ?item wdt:P31 wd:Q5 . {svc} }}"),
                vec!["itemAltLabel"],
                UnsupportedLabelService::Aliases,
            ),
            (
                format!("SELECT ?item ?itemLabel WHERE {{ ?item rdfs:label ?itemLabel . {svc} }}"),
                vec!["itemLabel"],
                UnsupportedLabelService::PreboundOutput,
            ),
            (
                format!("SELECT ?item WITH {{ SELECT ?item {{ ?item wdt:P31 wd:Q5 }} }} AS %a WHERE {{ INCLUDE %a . {svc} }}"),
                vec![],
                UnsupportedLabelService::NamedSubqueries,
            ),
            (
                format!("ASK {{ ?item wdt:P31 wd:Q5 . {svc} }}"),
                vec![],
                UnsupportedLabelService::NotInSelect,
            ),
            (
                "SELECT ?itemLabel WHERE { SERVICE wikibase:label { ?item wdt:P31 ?x } }".to_string(),
                vec![],
                UnsupportedLabelService::ServiceContent,
            ),
            (
                "SELECT ?itemLabel WHERE { SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\\\") }\" } }".to_string(),
                vec![],
                UnsupportedLabelService::Language,
            ),
            (
                "SELECT ?itemLabel WHERE { SERVICE wikibase:label { } }".to_string(),
                vec![],
                UnsupportedLabelService::MissingLanguage,
            ),
            (
                format!("SELECT ?itemLabel WHERE {{ {svc}"),
                vec![],
                UnsupportedLabelService::Syntax,
            ),
        ] {
            assert_eq!(
                rewrite(&sparql, &needed),
                LabelServiceRewrite::Unsupported(reason),
                "{sparql}"
            );
        }
    }

    #[test]
    fn test_alias_that_is_not_needed_is_dropped() {
        let sparql = "SELECT ?item ?itemAltLabel WHERE { ?item wdt:P31 wd:Q5 . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" } }";
        assert!(!rewritten(sparql, &[]).contains("SERVICE"));
    }

    #[test]
    fn test_rewritten_query_has_nothing_left_to_rewrite() {
        let sparql = "SELECT ?item ?itemLabel WHERE { ?item wdt:P31 wd:Q5 . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" } }";
        let query = rewritten(sparql, &["itemLabel"]);
        assert_eq!(
            rewrite(&query, &["itemLabel"]),
            LabelServiceRewrite::Unchanged
        );
    }

    #[test]
    fn test_unsupported_reason_is_displayed() {
        assert!(UnsupportedLabelService::Aliases
            .to_string()
            .contains("alias"));
    }
}
