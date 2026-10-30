//! The wiki replica databases, and the queries several tools share.
//!
//! Some wikis keep tables on a separate server (Commons its link tables), so
//! every connection is for a set of tables, and a query must not mix tables
//! from both sides: such joins happen here, in code.

use crate::toolforge::db::Cluster;
use crate::toolforge::pool::WikiPool;
use crate::wiki::Wiki;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use crate::mysql_async::prelude::{FromRow, Queryable};
use crate::mysql_async::{Conn, Params, Value};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STATEMENT_TIME: Duration = Duration::from_secs(600);
const POOL_MAX: usize = 8;
/// Values per `IN (…)` list.
const CHUNK: usize = 5000;
pub const CATEGORY_TABLES: &[&str] = &["page", "categorylinks", "linktarget"];
const NS_CATEGORY: i64 = 14;
/// Category trees can be enormous; stop growing one at this size.
const MAX_CATEGORIES: usize = 50_000;

#[derive(Debug)]
pub struct Replicas {
    pool: WikiPool,
}

impl Replicas {
    pub fn new(credentials: &Path) -> Result<Self> {
        let pool = WikiPool::builder(Cluster::Analytics)
            .my_cnf_path(credentials)
            .pool_max(POOL_MAX)
            .connect_timeout(CONNECT_TIMEOUT)
            .max_statement_time(MAX_STATEMENT_TIME)
            .build()
            .with_context(|| {
                format!(
                    "cannot read the database credentials from {}",
                    credentials.display()
                )
            })?;
        Ok(Self { pool })
    }

    /// A connection to the database of `wiki` that has `tables`.
    pub async fn conn(&self, wiki: &Wiki, tables: &[&str]) -> Result<Conn> {
        self.pool
            .connect_for_tables(wiki.dbname(), tables)
            .await
            .with_context(|| format!("cannot connect to the {} replica", wiki.dbname()))
    }

    /// `category` and its subcategories down to `depth` (0: just the
    /// category), as database titles.
    pub async fn category_tree(
        &self,
        wiki: &Wiki,
        category: &str,
        depth: u32,
    ) -> Result<Vec<String>> {
        let mut conn = self.conn(wiki, CATEGORY_TABLES).await?;
        let root = db_title(category.trim().trim_start_matches("Category:"));
        let mut tree = vec![root.clone()];
        let mut seen = HashSet::from([root.clone()]);
        let mut level = vec![root];
        for _ in 0..depth {
            let sql = "SELECT DISTINCT page_title FROM page
                JOIN categorylinks ON cl_from=page_id JOIN linktarget ON cl_target_id=lt_id
                WHERE lt_namespace=14 AND cl_type='subcat' AND lt_title IN";
            let subcategories: Vec<String> = select_in(&mut conn, sql, "", &[], &level).await?;
            level = subcategories
                .into_iter()
                .filter(|c| seen.insert(c.clone()))
                .collect();
            tree.extend(level.iter().cloned());
            if level.is_empty() || tree.len() >= MAX_CATEGORIES {
                break;
            }
        }
        Ok(tree)
    }

    /// Pages of `namespace` in a category tree, as database titles.
    pub async fn pages_in_tree(
        &self,
        wiki: &Wiki,
        category: &str,
        depth: u32,
        namespace: i64,
    ) -> Result<Vec<String>> {
        self.some_pages_in_tree(wiki, category, depth, namespace, None)
            .await
    }

    /// As [`Self::pages_in_tree`], but at most `limit` (any) of them, without
    /// the database reading more.
    pub async fn some_pages_in_tree(
        &self,
        wiki: &Wiki,
        category: &str,
        depth: u32,
        namespace: i64,
        limit: Option<usize>,
    ) -> Result<Vec<String>> {
        let mut categories = self.category_tree(wiki, category, depth).await?;
        if namespace == NS_CATEGORY {
            categories.truncate(limit.unwrap_or(usize::MAX));
            return Ok(categories);
        }
        let mut conn = self.conn(wiki, CATEGORY_TABLES).await?;
        let sql = "SELECT DISTINCT page_title FROM page
            JOIN categorylinks ON cl_from=page_id JOIN linktarget ON cl_target_id=lt_id
            WHERE page_namespace=? AND lt_namespace=14 AND lt_title IN";
        let suffix = limit.map(|n| format!("LIMIT {n}")).unwrap_or_default();
        let pages: Vec<String> =
            select_in(&mut conn, sql, &suffix, &[namespace.into()], &categories).await?;
        let mut seen = HashSet::new();
        let mut pages: Vec<String> = pages
            .into_iter()
            .filter(|p| seen.insert(p.clone()))
            .collect();
        pages.truncate(limit.unwrap_or(usize::MAX));
        Ok(pages)
    }

    /// A random non-redirect page of `namespace` in a category tree (of a
    /// huge tree, only in its first few thousand categories).
    pub async fn random_page_in_tree(
        &self,
        wiki: &Wiki,
        category: &str,
        depth: u32,
        namespace: i64,
    ) -> Result<Option<String>> {
        let categories = self.category_tree(wiki, category, depth).await?;
        let mut conn = self.conn(wiki, CATEGORY_TABLES).await?;
        let sql = format!(
            "SELECT page_title FROM page
            JOIN categorylinks ON cl_from=page_id JOIN linktarget ON cl_target_id=lt_id
            WHERE page_namespace=? AND page_is_redirect=0 AND lt_namespace=14 AND lt_title IN ({})
            ORDER BY RAND() LIMIT 1",
            placeholders(categories.len().min(CHUNK))
        );
        let mut params = vec![Value::from(namespace)];
        params.extend(categories.into_iter().take(CHUNK).map(Value::from));
        let title: Option<Vec<u8>> = conn.exec_first(sql, Params::Positional(params)).await?;
        Ok(title.map(|t| String::from_utf8_lossy(&t).into_owned()))
    }

    /// Those of `titles` (database form) that exist in `namespace`.
    pub async fn existing(
        &self,
        wiki: &Wiki,
        namespace: i64,
        titles: &[String],
    ) -> Result<HashSet<String>> {
        let mut conn = self.conn(wiki, &["page"]).await?;
        let sql = "SELECT page_title FROM page WHERE page_namespace=? AND page_title IN";
        let rows: Vec<String> = select_in(&mut conn, sql, "", &[namespace.into()], titles).await?;
        Ok(rows.into_iter().collect())
    }

    /// How many links each of `titles` (database form, namespace 0) has;
    /// with `from_articles`, only links from articles count.
    pub async fn link_counts(
        &self,
        wiki: &Wiki,
        titles: &[String],
        from_articles: bool,
    ) -> Result<HashMap<String, u64>> {
        let mut conn = self.conn(wiki, &["pagelinks", "linktarget"]).await?;
        let from = if from_articles {
            "AND pl_from_namespace=0"
        } else {
            ""
        };
        let sql = format!(
            "SELECT lt_title, COUNT(*) FROM linktarget JOIN pagelinks ON pl_target_id=lt_id
             WHERE lt_namespace=0 {from} AND lt_title IN"
        );
        let rows: Vec<(String, u64)> =
            select_in(&mut conn, &sql, "GROUP BY lt_title", &[], titles).await?;
        Ok(rows.into_iter().collect())
    }
}

/// Wikidata's entity namespaces: ID prefix, namespace, title prefix.
const ENTITY_NAMESPACES: &[(char, i64, &str)] =
    &[('Q', 0, ""), ('P', 120, "Property:"), ('L', 146, "Lexeme:")];

/// The pages of Wikidata entities (`Q42`, `P31`, `L7`; others are left
/// out, as are missing ones), as `(page ID, entity ID)`.
pub async fn entity_pages(conn: &mut Conn, ids: &[String]) -> Result<Vec<(u64, String)>> {
    let mut pages = vec![];
    for (prefix, namespace, _) in ENTITY_NAMESPACES {
        let titles: Vec<&str> = ids
            .iter()
            .filter(|id| id.starts_with(*prefix))
            .map(String::as_str)
            .collect();
        if !titles.is_empty() {
            let sql =
                "SELECT page_id, page_title FROM page WHERE page_namespace=? AND page_title IN";
            pages.extend(
                select_in::<(u64, String), _>(conn, sql, "", &[(*namespace).into()], &titles)
                    .await?,
            );
        }
    }
    Ok(pages)
}

/// The page title of a Wikidata entity: `Q42`, `Property:P31`, `Lexeme:L7`.
pub fn entity_page_title(id: &str) -> String {
    let prefix = ENTITY_NAMESPACES
        .iter()
        .find(|(p, ..)| id.starts_with(*p))
        .map_or("", |(.., title)| *title);
    format!("{prefix}{id}")
}

/// The `linktarget` IDs of pages; joining `pagelinks` (or `categorylinks`,
/// `templatelinks`) with these IDs is much faster than joining on `linktarget`.
pub async fn linktarget_ids(conn: &mut Conn, namespace: i64, titles: &[&str]) -> Result<Vec<u64>> {
    let sql = "SELECT lt_id FROM linktarget WHERE lt_namespace=? AND lt_title IN";
    select_in(conn, sql, "", &[namespace.into()], titles).await
}

/// Those of `titles` (namespace 0) that link to any of `targets` (see
/// [`linktarget_ids`]); on Wikidata, items with any of some statements.
pub async fn titles_linking_to(
    conn: &mut Conn,
    titles: &[String],
    targets: &[u64],
) -> Result<HashSet<String>> {
    if targets.is_empty() || titles.is_empty() {
        return Ok(HashSet::new());
    }
    let sql = format!(
        "SELECT DISTINCT page_title FROM page JOIN pagelinks ON pl_from=page_id
         WHERE pl_target_id IN ({}) AND page_namespace=0 AND page_title IN",
        placeholders(targets.len())
    );
    let params: Vec<Value> = targets.iter().map(|id| Value::from(*id)).collect();
    let rows: Vec<String> = select_in(conn, &sql, "", &params, titles).await?;
    Ok(rows.into_iter().collect())
}

/// `sql` ends in `… IN`; runs it for `values` in chunks, with `params`
/// before and `suffix` (e.g. `GROUP BY …`) after the list, and collects all rows.
pub async fn select_in<T: FromRow + Send + 'static, V: Into<Value> + Clone>(
    conn: &mut Conn,
    sql: &str,
    suffix: &str,
    params: &[Value],
    values: &[V],
) -> Result<Vec<T>> {
    let mut rows = vec![];
    for chunk in values.chunks(CHUNK) {
        let statement = format!("{sql} ({}) {suffix}", placeholders(chunk.len()));
        let mut all = params.to_vec();
        all.extend(chunk.iter().cloned().map(Into::into));
        rows.extend(
            conn.exec::<T, _, _>(statement, Params::Positional(all))
                .await?,
        );
    }
    Ok(rows)
}

pub fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// A title as the database has it: underscores for spaces, first letter upper case.
pub fn db_title(title: &str) -> String {
    let title = title.trim().replace(' ', "_");
    let mut chars = title.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A database timestamp (`20240102030405`) as `2024-01-02 03:04:05`.
pub fn format_timestamp(ts: &str) -> String {
    let part = |range: std::ops::Range<usize>| ts.get(range).unwrap_or_default();
    format!(
        "{}-{}-{} {}:{}:{}",
        part(0..4),
        part(4..6),
        part(6..8),
        part(8..10),
        part(10..12),
        part(12..14)
    )
}

/// A database timestamp's date, as `2024-01-02`.
pub fn format_date(ts: &str) -> String {
    format_timestamp(ts)
        .get(0..10)
        .unwrap_or_default()
        .to_string()
}

/// A database title for display.
pub fn display_title(title: &str) -> String {
    title.replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(
            db_title(" belgrade Aviation museum "),
            "Belgrade_Aviation_museum"
        );
        assert_eq!(display_title("A_b"), "A b");
        assert_eq!(placeholders(3), "?,?,?");
        assert_eq!(placeholders(0), "");
        assert_eq!(format_timestamp("20240102030405"), "2024-01-02 03:04:05");
        assert_eq!(format_date("20240102030405"), "2024-01-02");
        assert_eq!(entity_page_title("Q1"), "Q1");
        assert_eq!(entity_page_title("L7"), "Lexeme:L7");
    }
}
