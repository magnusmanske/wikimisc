//! The wiki replica databases, and the queries several tools share.
//!
//! Some wikis keep tables on a separate server (Commons its link tables), so
//! every connection is for a set of tables, and a query must not mix tables
//! from both sides: such joins happen here, in code.

use crate::toolforge::db::Cluster;
use crate::toolforge::pool::WikiPool;
use crate::wiki::Wiki;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

use crate::mysql_async::prelude::{FromRow, Queryable};
use crate::mysql_async::{Conn, Params, Value};

/// Failure modes of [`Replicas`] and the shared replica queries.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReplicasError {
    /// The `replica.my.cnf` given to the builder could not be used.
    #[error("cannot read the database credentials from {path}: {source}")]
    CredentialsFile {
        path: String,
        #[source]
        source: crate::toolforge::Error,
    },

    /// No usable database credentials could be discovered.
    #[error("cannot discover the database credentials: {source}")]
    CredentialsDiscovery {
        #[source]
        source: crate::toolforge::Error,
    },

    /// Connecting to a wiki's replica database failed.
    #[error("cannot connect to the {dbname} replica: {source}")]
    Connect {
        dbname: String,
        #[source]
        source: crate::toolforge::Error,
    },

    /// Closing the pool failed.
    #[error(transparent)]
    Pool(#[from] crate::toolforge::Error),

    /// A query failed.
    #[error(transparent)]
    Query(#[from] crate::mysql_async::Error),
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STATEMENT_TIME: Duration = Duration::from_secs(600);
const POOL_MAX: usize = 8;
/// Values per `IN (…)` list.
const CHUNK: usize = 5000;
pub const CATEGORY_TABLES: &[&str] = &["page", "categorylinks", "linktarget"];
const NS_CATEGORY: i64 = 14;
/// Category trees can be enormous; stop growing one at this size.
const MAX_CATEGORIES: usize = 50_000;

#[derive(Debug, Clone)]
pub struct Replicas {
    pool: WikiPool,
}

impl Replicas {
    /// The default pool: the analytics cluster, credentials discovered as the
    /// `toolforge` crate does, at most `POOL_MAX` connections.
    pub fn new(credentials: &Path) -> Result<Self, ReplicasError> {
        Self::builder(Cluster::Analytics)
            .my_cnf_path(credentials)
            .build()
    }

    /// A pool for `cluster`, with the default timeouts and pool size.
    pub fn builder(cluster: Cluster) -> ReplicasBuilder {
        ReplicasBuilder {
            cluster,
            my_cnf: None,
            pool_max: POOL_MAX,
            connect_timeout: CONNECT_TIMEOUT,
            max_statement_time: MAX_STATEMENT_TIME,
        }
    }

    /// The underlying `toolforge` pool, for callers with their own retry or
    /// timeout handling.
    pub fn pool(&self) -> &WikiPool {
        &self.pool
    }

    /// Close all pooled connections. Clones share the pool, so this closes
    /// it for all of them.
    pub async fn disconnect(self) -> Result<(), ReplicasError> {
        self.pool.disconnect().await?;
        Ok(())
    }

    /// A connection to `dbname` that has `tables`, with `toolforge`'s error
    /// type, for callers that inspect e.g. `max_user_connections` themselves.
    pub async fn connect_for_tables(
        &self,
        dbname: &str,
        tables: &[&str],
    ) -> std::result::Result<Conn, crate::toolforge::Error> {
        self.pool.connect_for_tables(dbname, tables).await
    }

    /// As [`connect_for_tables`](Self::connect_for_tables), with context.
    pub async fn conn_for_dbname(
        &self,
        dbname: &str,
        tables: &[&str],
    ) -> Result<Conn, ReplicasError> {
        self.connect_for_tables(dbname, tables)
            .await
            .map_err(|source| ReplicasError::Connect {
                dbname: dbname.to_string(),
                source,
            })
    }

    /// A connection to the database of `wiki` that has `tables`.
    pub async fn conn(&self, wiki: &Wiki, tables: &[&str]) -> Result<Conn, ReplicasError> {
        self.conn_for_dbname(wiki.dbname(), tables).await
    }
}

/// How to build a [`Replicas`] pool.
#[derive(Debug, Clone)]
pub struct ReplicasBuilder {
    cluster: Cluster,
    my_cnf: Option<PathBuf>,
    pool_max: usize,
    connect_timeout: Duration,
    max_statement_time: Duration,
}

impl ReplicasBuilder {
    /// Read the credentials from this `replica.my.cnf` instead of discovering them.
    pub fn my_cnf_path(mut self, path: &Path) -> Self {
        self.my_cnf = Some(path.to_path_buf());
        self
    }

    /// Connections per database server.
    pub fn pool_max(mut self, pool_max: usize) -> Self {
        self.pool_max = pool_max;
        self
    }

    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// The server-side limit for each statement.
    pub fn max_statement_time(mut self, timeout: Duration) -> Self {
        self.max_statement_time = timeout;
        self
    }

    pub fn build(self) -> Result<Replicas, ReplicasError> {
        let mut builder = WikiPool::builder(self.cluster)
            .pool_max(self.pool_max)
            .connect_timeout(self.connect_timeout)
            .max_statement_time(self.max_statement_time);
        if let Some(path) = &self.my_cnf {
            builder = builder.my_cnf_path(path);
        }
        let pool = builder.build().map_err(|source| match &self.my_cnf {
            Some(path) => ReplicasError::CredentialsFile {
                path: path.display().to_string(),
                source,
            },
            None => ReplicasError::CredentialsDiscovery { source },
        })?;
        Ok(Replicas { pool })
    }
}

impl Replicas {
    /// `category` and its subcategories down to `depth` (0: just the
    /// category), as database titles.
    pub async fn category_tree(
        &self,
        wiki: &Wiki,
        category: &str,
        depth: u32,
    ) -> Result<Vec<String>, ReplicasError> {
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
    ) -> Result<Vec<String>, ReplicasError> {
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
    ) -> Result<Vec<String>, ReplicasError> {
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
    ) -> Result<Option<String>, ReplicasError> {
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
    ) -> Result<HashSet<String>, ReplicasError> {
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
    ) -> Result<HashMap<String, u64>, ReplicasError> {
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
pub async fn entity_pages(
    conn: &mut Conn,
    ids: &[String],
) -> Result<Vec<(u64, String)>, ReplicasError> {
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
pub async fn linktarget_ids(
    conn: &mut Conn,
    namespace: i64,
    titles: &[&str],
) -> Result<Vec<u64>, ReplicasError> {
    let sql = "SELECT lt_id FROM linktarget WHERE lt_namespace=? AND lt_title IN";
    select_in(conn, sql, "", &[namespace.into()], titles).await
}

/// Those of `titles` (namespace 0) that link to any of `targets` (see
/// [`linktarget_ids`]); on Wikidata, items with any of some statements.
pub async fn titles_linking_to(
    conn: &mut Conn,
    titles: &[String],
    targets: &[u64],
) -> Result<HashSet<String>, ReplicasError> {
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
) -> Result<Vec<T>, ReplicasError> {
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
