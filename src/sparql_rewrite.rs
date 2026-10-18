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
//!   outputs it uses; one the caller does not use is left unbound if the
//!   query refers to it only where that cannot change which rows come back:
//!   where it is projected, unless the `SELECT` is `DISTINCT` (or `REDUCED`)
//!   without also projecting the entity, and in a `GROUP BY` that also groups
//!   by the entity. A service with no needed outputs is simply removed. That
//!   keeps the rewritten query as cheap as possible.
//! - `[AUTO_LANGUAGE]` is dropped. Only the WDQS web interface substitutes
//!   it; in a query sent to the endpoint it matches no language.
//! - aliases (`skos:altLabel`) are not rewritten, as they need a copy of the
//!   query's constraints in an aggregating subquery. A query needing one is
//!   left unchanged.
//!
//! # Named subqueries
//!
//! [`rewrite_named_subqueries`] inlines Blazegraph's `WITH { … } AS %name` /
//! `INCLUDE %name` as SPARQL 1.1 subqueries. The label service rewrite does
//! not look into named subqueries, so run this one first.

mod named_subqueries;

pub use named_subqueries::{
    rewrite_named_subqueries, NamedSubqueryRewrite, UnsupportedNamedSubquery,
};

use crate::sparql_endpoint::WIKIDATA_PREFIXES;
use crate::sparql_lexer::{tokenize, Brackets, Token, TokenKind};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// Blazegraph's named subqueries (`WITH { … } AS %name`, `INCLUDE %name`);
    /// [`rewrite_named_subqueries`] inlines them.
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
/// query itself does not need: return `true` if the caller reads
/// that variable from the results. Pass `|_| true` to rewrite every output.
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

/// A text edit: replace the byte range `start..end` of the query.
type Edit = (usize, usize, String);

/// A `SELECT`, with what its clauses say about the variables.
#[derive(Debug)]
struct SelectScope<'a> {
    /// Tokens of the projection, between `SELECT` and the `WHERE` group.
    projection: (usize, usize),
    /// Tokens of the `GROUP BY` clause, if any.
    group_by: Option<(usize, usize)>,
    /// Whether this is `SELECT DISTINCT` or `SELECT REDUCED`.
    distinct: bool,
    /// Variables projected as such, not inside an expression.
    projected: HashSet<&'a str>,
    /// Variables grouped by as such, not inside an expression.
    grouped: HashSet<&'a str>,
    /// The outputs of a label service in automatic mode.
    automatic: Vec<Output<'a>>,
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

impl Output<'_> {
    /// The entity's variable name, if it is a variable.
    fn entity_var(&self) -> Option<&str> {
        self.entity.strip_prefix(['?', '$'])
    }
}

/// One `SERVICE wikibase:label { … }`.
#[derive(Debug)]
struct Service<'a> {
    /// The `SERVICE` token and the `}` closing its body.
    span: (usize, usize),
    /// The `{` and `}` of the graph pattern the service is in.
    group: (usize, usize),
    /// The `{` of the `WHERE` group of the `SELECT` the service is in.
    select: usize,
    languages: Vec<&'a str>,
    /// The outputs the service names; if none, it has its `SELECT`'s
    /// automatic ones.
    manual: Vec<Output<'a>>,
}

impl Service<'_> {
    fn contains(&self, index: usize) -> bool {
        self.span.0 <= index && index <= self.span.1
    }
}

struct LabelServiceRewriter<'a> {
    sparql: &'a str,
    tokens: Vec<Token<'a>>,
    prefixes: HashMap<&'a str, &'a str>,
    /// The tokens where each variable occurs, by name.
    occurrences: HashMap<&'a str, Vec<usize>>,
    /// The `(` or `{` opening the constraint of each `FILTER`.
    filters: HashSet<usize>,
    /// Set by `rewrite`, once the brackets are known to balance.
    brackets: Brackets,
    /// Set by `rewrite`; by the `{` of their `WHERE` group.
    selects: HashMap<usize, SelectScope<'a>>,
}

impl<'a> LabelServiceRewriter<'a> {
    fn new(sparql: &'a str) -> Self {
        let tokens = tokenize(sparql);
        let mut occurrences: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, token) in tokens.iter().enumerate() {
            if let Some(name) = token.var_name() {
                occurrences.entry(name).or_default().push(i);
            }
        }
        Self {
            sparql,
            prefixes: declared_prefixes(&tokens),
            occurrences,
            filters: filter_constraints(&tokens),
            tokens,
            brackets: Brackets::default(),
            selects: HashMap::new(),
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
        self.brackets = Brackets::new(&self.tokens).ok_or(UnsupportedLabelService::Syntax)?;
        self.selects = self.select_scopes();
        let services = self.services()?;
        let mut taken: HashSet<String> = self.occurrences.keys().map(|v| v.to_string()).collect();
        let mut edits: Vec<Edit> = self
            .removed_runs(&services)
            .into_iter()
            .flat_map(|run| self.removal(run))
            .collect();
        let mut insertions: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        let mut bound: HashSet<&str> = HashSet::new();
        // Services in automatic mode share their `SELECT`'s outputs and have
        // no variables of their own, so what they need is decided once.
        let mut automatic_needed: HashMap<usize, Vec<&Output>> = HashMap::new();
        for service in &services {
            let needed = if service.manual.is_empty() {
                automatic_needed
                    .entry(service.select)
                    .or_insert_with(|| self.needed(service, is_needed))
                    .clone()
            } else {
                self.needed(service, is_needed)
            };
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
            insertions.entry(service.group.1).or_default().extend(lines);
        }
        for (group_close, lines) in insertions {
            edits.push(self.insertion(group_close, &lines));
        }
        Ok(Some(self.apply(edits)))
    }

    /// The query with `edits` made. Edits never overlap; an insertion at the
    /// offset where a removal starts goes before it, so it survives.
    fn apply(&self, mut edits: Vec<Edit>) -> String {
        edits.sort_by_key(|&(start, end, _)| (start, end));
        let mut query = String::with_capacity(self.sparql.len());
        let mut kept_from = 0;
        for (start, end, text) in edits {
            query.push_str(self.sparql.get(kept_from..start).unwrap_or_default());
            query.push_str(&text);
            kept_from = end;
        }
        query.push_str(self.sparql.get(kept_from..).unwrap_or_default());
        query
    }

    /// The service's outputs that are needed, by the query itself or by the
    /// caller.
    fn needed<'s>(
        &'s self,
        service: &'s Service<'a>,
        is_needed: &dyn Fn(&str) -> bool,
    ) -> Vec<&'s Output<'a>> {
        self.outputs(service)
            .iter()
            .filter(|o| self.is_used_in_query(o, service) || is_needed(o.output))
            .collect()
    }

    /// The outputs the service names, or else those of its `SELECT`.
    fn outputs<'s>(&'s self, service: &'s Service<'a>) -> &'s [Output<'a>] {
        if !service.manual.is_empty() {
            return &service.manual;
        }
        self.selects
            .get(&service.select)
            .map_or(&[], |select| &select.automatic)
    }

    // ── analysis ────────────────────────────────────────────────────────────

    fn select_scopes(&self) -> HashMap<usize, SelectScope<'a>> {
        let mut scopes = HashMap::new();
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
            let Some(close) = self.brackets.partner(open) else {
                continue;
            };
            let projection = (i + 1, open);
            let group_by = self.group_by_clause(close + 1);
            scopes.insert(
                open,
                SelectScope {
                    projection,
                    group_by,
                    distinct: self
                        .tokens
                        .get(i + 1)
                        .is_some_and(|t| t.is_keyword("DISTINCT") || t.is_keyword("REDUCED")),
                    projected: self.bare_vars(projection).collect(),
                    grouped: group_by.map_or_else(HashSet::new, |c| self.bare_vars(c).collect()),
                    automatic: self.automatic_outputs(projection),
                },
            );
        }
        scopes
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
            let close = self
                .brackets
                .partner(open)
                .ok_or(UnsupportedLabelService::Syntax)?;
            let select = self
                .select_of(i)
                .ok_or(UnsupportedLabelService::NotInSelect)?;
            let (languages, manual) = self.parse_body(open + 1, close)?;
            services.push(Service {
                span: (i, close),
                group: self.enclosing_group(i)?,
                select,
                languages,
                manual,
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

    /// Variables projected as such in `tokens[projection]` whose names end
    /// in `Label`, `Description` or `AltLabel`.
    fn automatic_outputs(&self, projection: (usize, usize)) -> Vec<Output<'a>> {
        self.bare_vars(projection)
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

    /// The variables in `tokens[start..end]` that are not inside an
    /// expression there.
    fn bare_vars<'s>(&'s self, (start, end): (usize, usize)) -> impl Iterator<Item = &'a str> + 's {
        (start..end)
            .filter(move |&i| self.is_bare(i, start))
            .filter_map(|i| self.tokens.get(i)?.var_name())
    }

    /// Whether `tokens[index]` is inside no bracket opened at or after
    /// `tokens[start]`.
    fn is_bare(&self, index: usize, start: usize) -> bool {
        self.brackets.parent(index).is_none_or(|b| b < start)
    }

    /// The innermost `SELECT` whose `WHERE` group contains `tokens[index]`,
    /// by the group's `{`.
    fn select_of(&self, index: usize) -> Option<usize> {
        self.brackets
            .ancestors(index)
            .find(|open| self.selects.contains_key(open))
    }

    /// The innermost `{ … }` around `tokens[index]`.
    fn enclosing_group(&self, index: usize) -> Result<(usize, usize)> {
        let open = self
            .brackets
            .ancestors(index)
            .find(|&b| self.tokens.get(b).is_some_and(|t| t.is_punct('{')))
            .ok_or(UnsupportedLabelService::Syntax)?;
        let close = self
            .brackets
            .partner(open)
            .ok_or(UnsupportedLabelService::Syntax)?;
        Ok((open, close))
    }

    /// The tokens where `?var` occurs.
    fn occurrences(&self, var: &str) -> &[usize] {
        self.occurrences.get(var).map_or(&[], Vec::as_slice)
    }

    /// Whether the query needs `output` bound for its own sake: whether it
    /// refers to it anywhere besides the service itself and the places in
    /// the service's `SELECT` where leaving it unbound cannot change which
    /// rows come back. Those are the projection, unless the `SELECT` is
    /// `DISTINCT` without projecting the entity as well, and a `GROUP BY`
    /// that groups by the entity as well — else rows or groups that differ
    /// only in the label would merge.
    fn is_used_in_query(&self, output: &Output, service: &Service) -> bool {
        let Some(select) = self.selects.get(&service.select) else {
            return true;
        };
        let with_entity =
            |vars: &HashSet<&str>| output.entity_var().is_some_and(|e| vars.contains(e));
        let (start, end) = select.projection;
        self.occurrences(output.output).iter().any(|&i| {
            let projected = start <= i
                && i < end
                && self.is_bare(i, start)
                && (!select.distinct || with_entity(&select.projected));
            let grouped = select.group_by.is_some_and(|(s, e)| s <= i && i < e)
                && with_entity(&select.grouped);
            !(service.contains(i) || projected || grouped)
        })
    }

    /// Whether `?output` occurs in the service's graph pattern other than in
    /// a `FILTER`, so it is bound there, which would clash with the rewrite's
    /// `BIND`.
    fn is_prebound(&self, output: &str, service: &Service) -> bool {
        let (open, close) = service.group;
        let in_filter = |i: usize| {
            self.brackets
                .ancestors(i)
                .take_while(|&b| b > open)
                .any(|b| self.filters.contains(&b))
        };
        self.occurrences(output)
            .iter()
            .any(|&i| open < i && i < close && !service.contains(i) && !in_filter(i))
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
            let is_var = output.entity_var().is_some();
            let base = output.entity_var().unwrap_or(output.output);
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

    /// The token ranges to delete, one per run of adjacent services: each
    /// service and a `.` right after it.
    fn removed_runs(&self, services: &[Service]) -> Vec<(usize, usize)> {
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for service in services {
            let (first, close) = service.span;
            let last = if self.tokens.get(close + 1).is_some_and(|t| t.is_punct('.')) {
                close + 1
            } else {
                close
            };
            match runs.last_mut() {
                Some(run) if run.1 + 1 == first => run.1 = last,
                _ => runs.push((first, last)),
            }
        }
        runs
    }

    /// Edits deleting `tokens[first..=last]`, and the whole line if nothing
    /// else is left on it.
    fn removal(&self, (first, last): (usize, usize)) -> Vec<Edit> {
        let start = self.tokens[first].start;
        let end = self.tokens[last].end();
        let (start, end) = match (self.blank_line_start(start), self.blank_line_end(end)) {
            (Some(line_start), Some(line_end)) => (line_start, line_end),
            _ => (start, end),
        };
        let mut edits = vec![(start, end, String::new())];
        // SERVICE needs no `.` before it, so it may be all that separates two
        // triples (`?a ?b ?c SERVICE … { … } ?d ?e ?f`). Keep them apart with
        // a `.`, which may follow anything but `{` or another `.`.
        let previous = first.checked_sub(1).and_then(|i| self.tokens.get(i));
        if let Some(previous) = previous.filter(|t| !t.is_punct('{') && !t.is_punct('.')) {
            edits.push((previous.end(), previous.end(), " .".to_string()));
        }
        edits
    }

    /// The text to insert before the `}` at `tokens[group_close]`.
    fn insertion(&self, group_close: usize, lines: &[String]) -> Edit {
        let at = self.tokens[group_close].start;
        match self.blank_line_start(at) {
            Some(line_start) => {
                // `}` on its own line: add indented lines above it.
                let indent = format!("{}  ", &self.sparql[line_start..at]);
                let text: String = lines.iter().map(|l| format!("{indent}{l}\n")).collect();
                (line_start, line_start, text)
            }
            None => (at, at, format!(" {} ", lines.join(" "))),
        }
    }

    /// The start of the line `offset` is on, if only blanks precede it there.
    fn blank_line_start(&self, offset: usize) -> Option<usize> {
        let before = self.sparql.get(..offset)?.trim_end_matches(is_blank);
        (before.is_empty() || before.ends_with('\n')).then_some(before.len())
    }

    /// The end of the line `offset` is on, past its `\n`, if only blanks
    /// follow it there.
    fn blank_line_end(&self, offset: usize) -> Option<usize> {
        let after = self.sparql.get(offset..)?.trim_start_matches(is_blank);
        let line_end = self.sparql.len() - after.len();
        match after.chars().next() {
            None => Some(line_end),
            Some('\n') => Some(line_end + 1),
            Some(_) => None,
        }
    }
}

/// Whitespace within a line.
fn is_blank(c: char) -> bool {
    c != '\n' && c.is_whitespace()
}

/// The `(` or `{` that opens the constraint of each `FILTER`: `( … )`,
/// `fn( … )` or `[NOT] EXISTS { … }`.
fn filter_constraints(tokens: &[Token]) -> HashSet<usize> {
    let mut constraints = HashSet::new();
    let mut after_filter = false;
    for (i, token) in tokens.iter().enumerate() {
        if token.is_keyword("FILTER") {
            after_filter = true;
        } else if after_filter && (token.is_punct('(') || token.is_punct('{')) {
            constraints.insert(i);
            after_filter = false;
        }
    }
    constraints
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

    #[test]
    fn test_removed_service_keeps_triples_apart() {
        // SERVICE needs no `.` before it, and the one after it is optional,
        // so removing it must not glue the triples around it together.
        let svc = "SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" }";
        let manual = "SERVICE wikibase:label { bd:serviceParam wikibase:language \"de\" . ?item schema:description ?d }";
        for sparql in [
            format!("SELECT ?item ?itemLabel WHERE {{ ?item wdt:P31 wd:Q5 {svc} . ?item wdt:P21 ?g }}"),
            format!("SELECT ?item ?itemLabel WHERE {{ ?item wdt:P31 wd:Q5 {svc} ?item wdt:P21 ?g }}"),
            format!("SELECT ?item ?itemLabel WHERE {{\n  ?item wdt:P31 wd:Q5\n  {svc}\n  ?item wdt:P21 ?g\n}}"),
            format!("SELECT ?item ?itemLabel WHERE {{ ?item wdt:P31 wd:Q5 {svc} {manual} ?item wdt:P21 ?g }}"),
            format!("SELECT ?item ?itemLabel WHERE {{\n  ?item wdt:P31 wd:Q5\n  {svc} .\n  {manual}\n  ?item wdt:P21 ?g\n}}"),
            format!("SELECT ?item ?itemLabel WHERE {{ OPTIONAL {{ ?item wdt:P21 ?g }} {svc} ?item wdt:P31 wd:Q5 }}"),
        ] {
            for needed in [&[][..], &["itemLabel"]] {
                // `rewritten` checks the result parses.
                let query = rewritten(&sparql, needed);
                assert!(!query.contains("SERVICE"), "{query}");
            }
        }
        let sparql = format!("SELECT ?item ?itemLabel WHERE {{\n  ?item wdt:P31 wd:Q5\n  {svc}\n  ?item wdt:P21 ?g\n}}");
        assert_eq!(
            rewritten(&sparql, &[]),
            "SELECT ?item ?itemLabel WHERE {\n  ?item wdt:P31 wd:Q5 .\n  ?item wdt:P21 ?g\n}"
        );
    }

    #[test]
    fn test_label_that_determines_the_rows_is_needed() {
        // Without the label, all countries would form one group, or one
        // distinct row.
        let svc = "SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" }";
        for sparql in [
            format!("SELECT ?countryLabel (COUNT(*) AS ?n) WHERE {{ ?p wdt:P27 ?country . {svc} }} GROUP BY ?countryLabel"),
            format!("SELECT DISTINCT ?countryLabel WHERE {{ ?p wdt:P27 ?country . {svc} }}"),
            format!("SELECT REDUCED ?countryLabel WHERE {{ ?p wdt:P27 ?country . {svc} }}"),
            format!("SELECT DISTINCT ?countryLabel ?n WHERE {{ {{ SELECT ?country (COUNT(*) AS ?n) WHERE {{ ?p wdt:P27 ?country }} GROUP BY ?country }} {svc} }}"),
        ] {
            let query = rewritten(&sparql, &["n"]);
            assert!(query.contains("AS ?countryLabel)"), "{query}");
        }
    }

    #[test]
    fn test_label_alongside_its_entity_is_not_needed() {
        let svc = "SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" }";
        for sparql in [
            format!("SELECT DISTINCT ?country ?countryLabel WHERE {{ ?p wdt:P27 ?country . {svc} }}"),
            format!("SELECT ?countryLabel (COUNT(*) AS ?n) WHERE {{ ?p wdt:P27 ?country . {svc} }} GROUP BY ?country ?countryLabel"),
            // `*` projects the entity too.
            "SELECT DISTINCT * WHERE { ?p wdt:P27 ?country . SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\" . ?country rdfs:label ?name } }".to_string(),
        ] {
            let query = rewritten(&sparql, &["n"]);
            assert!(
                !query.contains("SERVICE") && !query.contains("OPTIONAL"),
                "{query}"
            );
        }
    }

    #[test]
    fn test_many_services_are_rewritten_quickly() {
        // About 200 KB each. When every output was checked against every
        // token, the first of these took over a minute in a release build;
        // both now take milliseconds.
        const SERVICES: usize = 2000;
        let svc = "SERVICE wikibase:label { bd:serviceParam wikibase:language \"en\"";
        let projected: String = (0..SERVICES).map(|n| format!(" ?v{n}Label")).collect();
        let automatic: String = (0..SERVICES)
            .map(|n| format!("?v{n} wdt:P31 wd:Q5 . {svc} }}\n"))
            .collect();
        let manual: String = (0..SERVICES)
            .map(|n| format!("?v{n} wdt:P31 wd:Q5 . {svc} . ?v{n} rdfs:label ?l{n} }}\n"))
            .collect();
        let started = std::time::Instant::now();
        let unneeded = rewrite_label_service(
            &format!("SELECT{projected} WHERE {{\n{automatic}}}"),
            |_| false,
        );
        let needed = rewrite_label_service(&format!("SELECT * WHERE {{\n{manual}}}"), |_| true);
        let elapsed = started.elapsed();
        assert!(elapsed.as_secs() < 10, "took {elapsed:?}");
        let LabelServiceRewrite::Rewritten(query) = unneeded else {
            panic!("expected a rewrite, got {unneeded:?}");
        };
        assert_valid(&query);
        assert!(matches!(needed, LabelServiceRewrite::Rewritten(q) if q.contains("AS ?l1999)")));
    }
}
