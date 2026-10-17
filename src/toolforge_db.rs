//! MySQL connection pool registry for Toolforge replicas and tool databases.
//!
//! [`ToolforgeDB`] owns a `HashMap<String, mysql_async::Pool>` keyed by
//! caller-chosen names. Pools are constructed from `serde_json::Value` config
//! blobs via [`ToolforgeDB::add_mysql_pool`], so configuration can live in a
//! JSON file alongside other settings.
//!
//! Mind the Toolforge [connection handling
//! policy](https://wikitech.wikimedia.org/wiki/Help:Wiki_Replicas#Connection_handling_policy)
//! when writing those blobs: holding connections open while they are unused is
//! *not permitted* against the Wiki Replicas or ToolsDB, and idle connections
//! get killed. The defaults of [`PoolSettings`] comply; raising
//! `min_connections` or `keep_sec` much for a shared instance does not.
//!
//! [`ToolforgeDB::db_host_and_schema_for_wiki`] handles the
//! Toolforge-vs-local-dev fork: on Toolforge it produces the
//! `*.db.svc.wikimedia.cloud` replica hostname; locally it returns `127.0.0.1`
//! so SSH tunnels work.
//!
//! A few wikis keep part of their tables in an [*extension
//! database*](https://wikitech.wikimedia.org/wiki/Help:Wiki_Replicas#Extension_databases)
//! on a separate replica cluster, reached by prefixing that hostname — e.g.
//! `links.commonswiki.web.db.svc.wikimedia.cloud`. [`DbCluster`] models those
//! clusters and the tables each one holds, so a caller can ask either for the
//! host serving one cluster
//! ([`ToolforgeDB::db_host_and_schema_for_wiki_cluster`]) or for the host able
//! to serve a query over a given set of tables
//! ([`ToolforgeDB::db_host_and_schema_for_wiki_tables`]).

use crate::toolforge_app::ToolforgeApp;
use crate::toolforge_credentials::{Credentials, DbService};
use core::time::Duration;
use mysql_async::{Opts, OptsBuilder, PoolConstraints, PoolOpts};
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

/// Failure modes of [`ToolforgeDB`].
#[derive(Debug, Error)]
pub enum DatabaseError {
    /// The JSON pool config has no `url` key, so no connection can be built.
    #[error("create_pool: missing 'url' key in pool config")]
    MissingPoolUrl,

    /// No pool has been registered under the requested name.
    #[error("no pool '{0}' found")]
    UnknownPool(String),

    /// The config asks for more minimum than maximum connections.
    #[error("invalid pool config: min_connections ({min}) exceeds max_connections ({max})")]
    InvalidPoolConstraints { min: usize, max: usize },

    /// The requested tables are not all served by a single replica cluster, so
    /// no one connection can join them. Query them separately and combine the
    /// results in code.
    #[error("tables {tables:?} of '{wiki}' are not all on one replica cluster")]
    TablesSpanClusters { wiki: String, tables: Vec<String> },

    /// Neither the environment nor a `replica.my.cnf` provided credentials,
    /// so the process is presumably not running as a Toolforge tool.
    #[error("no credentials for {0}: no TOOL_* variables and no replica.my.cnf")]
    MissingCredentials(DbService),

    /// The credentials file could not be read.
    #[error("cannot read credentials file '{path}'")]
    CredentialsFile {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// The credentials file has no user and password in its `[client]` section.
    #[error("no [client] user and password in credentials file '{path}'")]
    MalformedCredentials { path: String },

    /// The configured `url` is not a valid MySQL connection URL, or a wiki
    /// name cannot be part of one (reported as an invalid `wiki` parameter).
    #[error(transparent)]
    Url(#[from] mysql_async::UrlError),

    /// Acquiring a connection from the pool failed.
    #[error(transparent)]
    MySql(#[from] mysql_async::Error),
}

/// Domain shared by every Wiki Replica service name.
const REPLICA_DOMAIN: &str = ".db.svc.wikimedia.cloud";

/// The shared MariaDB server holding tool-created databases.
const TOOLSDB_HOST: &str = "tools.db.svc.wikimedia.cloud";

/// Read-only replica of [`TOOLSDB_HOST`].
const TOOLSDB_READONLY_HOST: &str = "tools-readonly.db.svc.wikimedia.cloud";

/// Connections a Toolforge tool may hold at once, by default.
const DEFAULT_MAX_CONNECTIONS: usize = 10;

/// How long an unused connection may be kept, by default. One second, not
/// zero: see [`PoolSettings`] for why that is the useful value.
const DEFAULT_KEEP_SEC: u64 = 1;

/// The MariaDB port, used by every Toolforge database service.
const DEFAULT_PORT: u16 = 3306;

/// The Commons links tables, moved to the `links` extension database (`x4`)
/// in September 2026. Only these are gone from the core cluster; `page` and
/// `redirect` exist on both (see [`DbCluster::hosts_table`]).
const LINKS_TABLES: &[&str] = &[
    "categorylinks",
    "collation",
    "existencelinks",
    "externallinks",
    "globalimagelinks",
    "imagelinks",
    "iwlinks",
    "langlinks",
    "linktarget",
    "pagelinks",
    "templatelinks",
];

/// Tables the links cluster carries a copy of while the core cluster keeps one
/// too, so a query may join them on either side of the split.
const LINKS_SHARED_TABLES: &[&str] = &["page", "redirect"];

/// Connection-pool tuning.
///
/// The defaults keep within the Toolforge [connection handling
/// policy](https://wikitech.wikimedia.org/wiki/Help:Wiki_Replicas#Connection_handling_policy):
/// nothing is held open for the long term, and no more than the ten
/// connections a tool is allowed.
///
/// `keep_sec` defaults to 1 rather than 0, which is the same compromise the
/// `toolforge` crate ships. The difference matters more than the number
/// suggests, because `mysql_async` reads a zero `inactive_connection_ttl` as
/// "bound the pool by `min_connections`": with a minimum of 0 that closes every
/// connection the moment it is returned, so nothing idles — but nothing is
/// reused either, and every query pays for a fresh connection and handshake.
/// One second buys that reuse, and the pool still reaps what has gone idle
/// (within `mysql_async`'s 30-second check interval).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSettings {
    min_connections: usize,
    max_connections: usize,
    keep_sec: u64,
    max_statement_time: Option<Duration>,
}

impl Default for PoolSettings {
    fn default() -> Self {
        Self {
            min_connections: 0,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            keep_sec: DEFAULT_KEEP_SEC,
            max_statement_time: None,
        }
    }
}

impl PoolSettings {
    /// Reads the settings from a JSON config object, falling back to the
    /// default for every key that is absent or not a usable number. Recognised
    /// keys are `min_connections`, `max_connections`, `keep_sec` and
    /// `max_statement_time` (in seconds, decimals allowed; 0 means no limit).
    pub fn from_json(config: &Value) -> Self {
        Self::default().overridden_by_json(config)
    }

    /// These settings, with every key `config` sets taken from it instead; see
    /// [`Self::from_json`] for the keys.
    fn overridden_by_json(self, config: &Value) -> Self {
        Self {
            min_connections: config["min_connections"]
                .as_u64()
                .map_or(self.min_connections, |value| value as usize),
            max_connections: config["max_connections"]
                .as_u64()
                .map_or(self.max_connections, |value| value as usize),
            keep_sec: config["keep_sec"].as_u64().unwrap_or(self.keep_sec),
            max_statement_time: config["max_statement_time"]
                .as_f64()
                .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
                .map_or(self.max_statement_time, |limit| limit_or_none(Some(limit))),
        }
    }

    /// These settings, with the pool parameters a connection URL sets taken
    /// from it instead: `pool_min`, `pool_max` and `inactive_connection_ttl`,
    /// whose values `mysql_async` has already parsed into `url_pool_opts`.
    /// A parameter the URL does not mention keeps the current setting rather
    /// than `mysql_async`'s default, which would break the policy.
    fn overridden_by_url(self, url: &str, url_pool_opts: &PoolOpts) -> Self {
        let given = |param: &str| url_query_keys(url).any(|key| key == param);
        let constraints = url_pool_opts.constraints();
        Self {
            min_connections: match given("pool_min") {
                true => constraints.min(),
                false => self.min_connections,
            },
            max_connections: match given("pool_max") {
                true => constraints.max(),
                false => self.max_connections,
            },
            keep_sec: match given("inactive_connection_ttl") {
                true => url_pool_opts.inactive_connection_ttl().as_secs(),
                false => self.keep_sec,
            },
            ..self
        }
    }

    /// Sets the lower bound of the pool. Anything above 0 keeps connections
    /// open while unused, which shared Toolforge servers do not allow.
    pub fn with_min_connections(mut self, min_connections: usize) -> Self {
        self.min_connections = min_connections;
        self
    }

    /// Sets the upper bound of the pool, 10 by default because that is a
    /// tool's connection limit.
    pub fn with_max_connections(mut self, max_connections: usize) -> Self {
        self.max_connections = max_connections;
        self
    }

    /// Sets how long an unused connection may be kept, in seconds. 0 is not
    /// "close immediately but keep reusing": it disables reuse altogether, as
    /// described on [`PoolSettings`].
    pub fn with_keep_sec(mut self, keep_sec: u64) -> Self {
        self.keep_sec = keep_sec;
        self
    }

    /// Caps how long a single query may run, by issuing MariaDB's
    /// [`SET max_statement_time`](https://wikitech.wikimedia.org/wiki/Help:Toolforge/Database#Query_Limits)
    /// on every connection in the pool, each time it is opened or reset for
    /// reuse.
    ///
    /// Worth setting: the replicas kill a query at 5 minutes on the `web`
    /// service name and 3 hours on `analytics`, and a self-imposed limit fails
    /// a runaway query where the caller can see it. MariaDB-only — MySQL calls
    /// this `max_execution_time` and counts milliseconds.
    ///
    /// A zero limit is the same as `None`, as it is to MariaDB: no limit. A
    /// limit below MariaDB's one-microsecond resolution is rounded up to it
    /// rather than down to zero.
    pub fn with_max_statement_time(mut self, max_statement_time: Option<Duration>) -> Self {
        self.max_statement_time = limit_or_none(max_statement_time);
        self
    }

    pub fn min_connections(&self) -> usize {
        self.min_connections
    }

    pub fn max_connections(&self) -> usize {
        self.max_connections
    }

    pub fn keep_sec(&self) -> u64 {
        self.keep_sec
    }

    pub fn max_statement_time(&self) -> Option<Duration> {
        self.max_statement_time
    }

    /// `base` with these settings applied. Whatever else `base` holds, such as
    /// a URL's `reset_connection`, is kept.
    fn pool_opts(&self, base: PoolOpts) -> Result<PoolOpts, DatabaseError> {
        // `PoolConstraints::new` rejects min > max, and both values can come
        // straight from a caller's JSON, so this must be an error rather than
        // a panic.
        let constraints = PoolConstraints::new(self.min_connections, self.max_connections).ok_or(
            DatabaseError::InvalidPoolConstraints {
                min: self.min_connections,
                max: self.max_connections,
            },
        )?;
        Ok(base
            .with_constraints(constraints)
            .with_inactive_connection_ttl(Duration::from_secs(self.keep_sec)))
    }

    /// The statements to run on every connection, both when it is opened and
    /// each time the pool resets it for reuse. They have to be `setup` rather
    /// than `init` statements: the reset (`COM_RESET_CONNECTION`) clears
    /// session variables, and only `setup` statements are run again after it.
    fn setup_commands(&self) -> Vec<String> {
        self.max_statement_time
            .map(|limit| format!("SET max_statement_time = {}", statement_time_secs(limit)))
            .into_iter()
            .collect()
    }
}

/// `None` for a zero limit, which MariaDB reads as "no limit" anyway.
fn limit_or_none(limit: Option<Duration>) -> Option<Duration> {
    limit.filter(|limit| !limit.is_zero())
}

/// A statement time limit in seconds, rounded up to MariaDB's resolution of a
/// microsecond: MariaDB truncates anything finer, and a limit truncated to
/// zero would be no limit at all.
fn statement_time_secs(limit: Duration) -> f64 {
    limit.as_nanos().div_ceil(1_000) as f64 / 1e6
}

/// The parameter names in a connection URL's query string. Only good for
/// telling which parameters are present: `mysql_async` parses the values.
fn url_query_keys(url: &str) -> impl Iterator<Item = &str> {
    let query = url.split_once('?').map_or("", |(_, query)| query);
    let query = query.split_once('#').map_or(query, |(query, _)| query);
    query
        .split('&')
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
}

/// A production database section, such as `s4`: the cluster a wiki's core
/// database is hosted on, shared with the other wikis of that section.
///
/// Connecting by section rather than by wiki is for tools that would otherwise
/// open a connection per wiki — the docs ask that nothing else do it, since
/// which section holds a wiki can change. The mapping is the `slice` column of
/// `meta_p.wiki` (`SELECT dbname, slice FROM meta_p.wiki`), which
/// [`Self::from_meta_slice`] parses; query it at startup rather than hardcoding
/// it.
///
/// Extension databases are *not* addressed this way even though they are
/// sections of their own (the Commons links tables are `x4`): use the
/// [`DbCluster`] host name for those, which is what the docs document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DbSection {
    number: u8,
}

impl DbSection {
    /// The section with this number, e.g. 4 for `s4`. `None` for 0.
    pub fn new(number: u8) -> Option<Self> {
        match number {
            0 => None,
            number => Some(Self { number }),
        }
    }

    /// Parses a section name such as `s4`. `None` for anything else,
    /// including the `x4` of an extension database.
    pub fn parse(name: &str) -> Option<Self> {
        let number = name.strip_prefix('s').or_else(|| name.strip_prefix('S'))?;
        Self::new(number.parse().ok()?)
    }

    /// Parses the `slice` column of `meta_p.wiki`, which still carries a
    /// legacy suffix (`s3.labsdb`).
    pub fn from_meta_slice(slice: &str) -> Option<Self> {
        Self::parse(slice.split('.').next()?)
    }

    pub fn number(&self) -> u8 {
        self.number
    }
}

impl std::fmt::Display for DbSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "s{}", self.number)
    }
}

/// Which Wiki Replica service name to connect to.
///
/// Both serve the same data; `analytics` allows much longer-running queries at
/// the cost of being slower, `web` is meant for queries whose results a user is
/// waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DbServerGroup {
    /// Short query timeout, for web services. The default.
    #[default]
    Web,
    /// Long query timeout, for batch jobs.
    Analytics,
}

impl DbServerGroup {
    /// The service name as it appears in a replica hostname.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Analytics => "analytics",
        }
    }
}

impl std::fmt::Display for DbServerGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A replica cluster holding some or all of a wiki's tables.
///
/// Most wikis keep everything in their core database, but a few have tables
/// split off into an *extension database* on its own cluster, addressed by
/// prefixing the replica hostname with the extension name. Adding a future
/// split means adding a variant plus its [`Self::host_prefix`],
/// [`Self::applies_to_wiki`] and `owns_exclusively` arms, and listing it in
/// [`Self::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DbCluster {
    /// The wiki's core database, holding every table not split off elsewhere.
    #[default]
    Core,
    /// The Commons links tables (`x4`), split off in September 2026.
    Links,
    /// The Wikidata term store, i.e. the `wbt_*` tables (`x3`), split off in
    /// 2025.
    TermStore,
}

impl DbCluster {
    /// Every cluster, core first. Table resolution prefers the earliest match,
    /// so a table available on more than one cluster is read from the core.
    pub const ALL: &'static [Self] = &[Self::Core, Self::Links, Self::TermStore];

    /// The extension databases, i.e. everything except [`Self::Core`].
    const EXTENSIONS: &'static [Self] = &[Self::Links, Self::TermStore];

    /// The hostname prefix identifying this cluster; empty for [`Self::Core`].
    pub fn host_prefix(self) -> &'static str {
        match self {
            Self::Core => "",
            Self::Links => "links.",
            Self::TermStore => "termstore.",
        }
    }

    /// Whether `wiki` has this cluster at all. Every wiki has a
    /// [`Self::Core`]; an extension database exists only for the wikis whose
    /// tables were actually split.
    pub fn applies_to_wiki(self, wiki: &str) -> bool {
        let wiki = ToolforgeDB::fix_wiki_db_name(wiki);
        match self {
            Self::Core => true,
            Self::Links => matches!(wiki.as_str(), "commonswiki" | "testcommonswiki"),
            Self::TermStore => wiki == "wikidatawiki",
        }
    }

    /// Whether `table` lives *only* on this cluster, and is therefore no longer
    /// readable on the core cluster of a wiki that has the split.
    fn owns_exclusively(self, table: &str) -> bool {
        match self {
            Self::Core => false,
            Self::Links => LINKS_TABLES.iter().any(|t| t.eq_ignore_ascii_case(table)),
            Self::TermStore => table.to_ascii_lowercase().starts_with("wbt_"),
        }
    }

    /// Whether a query against this cluster can read `table` for `wiki`.
    ///
    /// The core cluster holds everything an extension database of that wiki has
    /// not taken over; an extension database holds the tables it owns, plus any
    /// it keeps a copy of.
    pub fn hosts_table(self, wiki: &str, table: &str) -> bool {
        match self {
            Self::Core => !Self::EXTENSIONS
                .iter()
                .any(|ext| ext.applies_to_wiki(wiki) && ext.owns_exclusively(table)),
            Self::Links => {
                self.owns_exclusively(table)
                    || LINKS_SHARED_TABLES
                        .iter()
                        .any(|t| t.eq_ignore_ascii_case(table))
            }
            Self::TermStore => self.owns_exclusively(table),
        }
    }

    /// The cluster that can serve a query reading all of `tables` on `wiki`.
    ///
    /// Returns [`DatabaseError::TablesSpanClusters`] when no single cluster
    /// holds them all — a join that has to be done in code instead.
    ///
    /// ```
    /// use wikimisc::toolforge_db::DbCluster;
    ///
    /// // The Commons links tables moved to their own cluster, and `page` is on
    /// // both, so the two can still be joined there ...
    /// assert_eq!(
    ///     DbCluster::for_tables("commonswiki", &["page", "pagelinks"])?,
    ///     DbCluster::Links
    /// );
    /// // ... while every other wiki keeps serving them from its core cluster.
    /// assert_eq!(
    ///     DbCluster::for_tables("enwiki", &["page", "pagelinks"])?,
    ///     DbCluster::Core
    /// );
    /// // A join across the split is refused; do it in code instead.
    /// assert!(DbCluster::for_tables("commonswiki", &["actor", "pagelinks"]).is_err());
    /// # Ok::<(), wikimisc::toolforge_db::DatabaseError>(())
    /// ```
    pub fn for_tables(wiki: &str, tables: &[&str]) -> Result<Self, DatabaseError> {
        let wiki = ToolforgeDB::fix_wiki_db_name(wiki);
        Self::ALL
            .iter()
            .copied()
            .filter(|cluster| cluster.applies_to_wiki(&wiki))
            .find(|cluster| tables.iter().all(|table| cluster.hosts_table(&wiki, table)))
            .ok_or_else(|| DatabaseError::TablesSpanClusters {
                wiki,
                tables: tables.iter().map(|t| t.to_string()).collect(),
            })
    }
}

#[derive(Debug, PartialEq)]
pub struct HostSchema {
    host: String,
    schema: String,
}

impl HostSchema {
    pub fn new(host: &str, schema: &str) -> Self {
        Self {
            host: host.to_string(),
            schema: schema.to_string(),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }
}

pub struct ToolforgeDB {
    mysql_pools: HashMap<String, mysql_async::Pool>,
    is_on_toolforge: bool,
    server_group: DbServerGroup,
    pool_settings: PoolSettings,
    port: u16,
}

impl Default for ToolforgeDB {
    fn default() -> Self {
        Self {
            mysql_pools: HashMap::new(),
            is_on_toolforge: ToolforgeApp::is_on_toolforge(),
            server_group: DbServerGroup::default(),
            pool_settings: PoolSettings::default(),
            port: DEFAULT_PORT,
        }
    }
}

/// Written by hand because `mysql_async::Pool`'s `Debug` prints its connection
/// options, password included: only the pool names are shown.
impl std::fmt::Debug for ToolforgeDB {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut pools: Vec<&str> = self.mysql_pools.keys().map(String::as_str).collect();
        pools.sort_unstable();
        f.debug_struct("ToolforgeDB")
            .field("mysql_pools", &pools)
            .field("is_on_toolforge", &self.is_on_toolforge)
            .field("server_group", &self.server_group)
            .field("pool_settings", &self.pool_settings)
            .field("port", &self.port)
            .finish()
    }
}

impl ToolforgeDB {
    /// Registers a connection pool built from a JSON config object under
    /// `key`. Recognised keys are `url` (required), `min_connections`,
    /// `max_connections`, `keep_sec` (the idle-connection TTL in seconds) and
    /// `max_statement_time` (in seconds, decimals allowed, 0 for no limit).
    ///
    /// Each setting comes from the first of these that has it: the JSON key;
    /// the URL's `pool_min`, `pool_max` or `inactive_connection_ttl`
    /// parameter; and [`Self::pool_settings`], which are
    /// [`PoolSettings::default()`] (minimum 0, maximum 10, `keep_sec` 1, no
    /// statement limit) unless [`Self::set_pool_settings`] said otherwise.
    /// Other pool parameters in the URL, such as `reset_connection`, are kept.
    /// Mind that `mysql_async` checks the URL on its own first, against *its*
    /// default minimum of 10, so a URL giving a `pool_max` below 10 must give
    /// `pool_min` too.
    ///
    /// The defaults follow the Toolforge [connection handling
    /// policy](https://wikitech.wikimedia.org/wiki/Help:Wiki_Replicas#Connection_handling_policy):
    /// no connection is held open while unused, and 10 is the per-tool
    /// connection limit. Only override them for a database you own, such as a
    /// [Trove](https://wikitech.wikimedia.org/wiki/Help:Trove_database_user_guide)
    /// instance — a pool that keeps idle connections against a shared server
    /// will have them killed.
    pub fn add_mysql_pool(&mut self, key: &str, config: &Value) -> Result<(), DatabaseError> {
        let pool = mysql_async::Pool::new(self.opts_from_json(config)?);
        self.mysql_pools.insert(key.to_string(), pool);
        Ok(())
    }

    /// The connection options for an [`Self::add_mysql_pool`] config.
    fn opts_from_json(&self, config: &Value) -> Result<Opts, DatabaseError> {
        let url = config["url"]
            .as_str()
            .ok_or(DatabaseError::MissingPoolUrl)?;
        let opts = Opts::from_url(url)?;
        let url_pool_opts = opts.pool_opts().clone();
        let settings = self
            .pool_settings
            .clone()
            .overridden_by_url(url, &url_pool_opts)
            .overridden_by_json(config);
        Self::apply_pool_settings(OptsBuilder::from_opts(opts), url_pool_opts, &settings)
    }

    /// Applies [`PoolSettings`] to connection options, on top of the pool
    /// options in `base`.
    fn apply_pool_settings(
        opts: OptsBuilder,
        base: PoolOpts,
        settings: &PoolSettings,
    ) -> Result<Opts, DatabaseError> {
        let opts = opts.pool_opts(settings.pool_opts(base)?);
        let setup_commands = settings.setup_commands();
        // `setup` replaces any earlier setup statements. A connection URL
        // cannot give any, but only touch it when there is something to say.
        let opts = match setup_commands.is_empty() {
            true => opts,
            false => opts.setup(setup_commands),
        };
        Ok(opts.into())
    }

    /// The settings new pools are built with. [`PoolSettings::default()`]
    /// unless [`Self::set_pool_settings`] said otherwise.
    pub fn pool_settings(&self) -> &PoolSettings {
        &self.pool_settings
    }

    /// Sets the [`PoolSettings`] used by [`Self::add_replica_pool`] and the
    /// other credential-based constructors, and as the fallback for keys
    /// missing from an [`Self::add_mysql_pool`] config.
    pub fn set_pool_settings(&mut self, pool_settings: PoolSettings) {
        self.pool_settings = pool_settings;
    }

    /// Builder-style variant of [`Self::set_pool_settings`].
    pub fn with_pool_settings(mut self, pool_settings: PoolSettings) -> Self {
        self.set_pool_settings(pool_settings);
        self
    }

    /// The port pools connect to, 3306 unless [`Self::set_port`] said
    /// otherwise.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Sets the port pools connect to. Every Toolforge database service listens
    /// on 3306; override this when developing against an SSH tunnel on some
    /// other local port.
    pub fn set_port(&mut self, port: u16) {
        self.port = port;
    }

    /// Builder-style variant of [`Self::set_port`].
    pub fn with_port(mut self, port: u16) -> Self {
        self.set_port(port);
        self
    }

    /// Registers a pool for the replica cluster that can serve a query over
    /// `tables`, using this tool's Toolforge credentials.
    ///
    /// This is the one-line way to get a connection right: it picks the cluster
    /// ([`DbCluster::for_tables`], so a Commons links query lands on the links
    /// cluster), the host name, and the credentials.
    pub fn add_replica_pool(
        &mut self,
        key: &str,
        wiki: &str,
        tables: &[&str],
    ) -> Result<(), DatabaseError> {
        let cluster = DbCluster::for_tables(wiki, tables)?;
        self.add_replica_pool_for_cluster(key, wiki, cluster)
    }

    /// Registers a pool for one named replica cluster of `wiki`, for a caller
    /// that knows which one it wants.
    pub fn add_replica_pool_for_cluster(
        &mut self,
        key: &str,
        wiki: &str,
        cluster: DbCluster,
    ) -> Result<(), DatabaseError> {
        let host_schema = self.db_host_and_schema_for_wiki_cluster(wiki, cluster)?;
        let credentials = Credentials::for_service(DbService::Replicas)?;
        self.add_pool_for(key, &host_schema, &credentials)
    }

    /// Registers a pool for a whole [`DbSection`], for a crosswiki tool that
    /// would otherwise hold a connection per wiki.
    ///
    /// No default database is selected, so queries must name the schema:
    /// `SELECT page_title FROM enwiki_p.page`.
    pub fn add_section_pool(&mut self, key: &str, section: DbSection) -> Result<(), DatabaseError> {
        let host = self.db_host_for_section(section);
        let credentials = Credentials::for_service(DbService::Replicas)?;
        self.insert_pool(key, &host, None, &credentials)
    }

    /// Registers a pool for one of the tool's own ToolsDB databases, named
    /// `<credential user>__<name>`.
    pub fn add_tool_db_pool(&mut self, key: &str, database: &str) -> Result<(), DatabaseError> {
        let host = self.get_db_host_for_tool_db().to_string();
        self.add_tool_db_pool_on(key, database, &host)
    }

    /// As [`Self::add_tool_db_pool`], but against the read-only ToolsDB
    /// replica, which long-running queries should prefer.
    pub fn add_tool_db_pool_readonly(
        &mut self,
        key: &str,
        database: &str,
    ) -> Result<(), DatabaseError> {
        let host = self.get_db_host_for_tool_db_readonly().to_string();
        self.add_tool_db_pool_on(key, database, &host)
    }

    fn add_tool_db_pool_on(
        &mut self,
        key: &str,
        database: &str,
        host: &str,
    ) -> Result<(), DatabaseError> {
        let credentials = Credentials::for_service(DbService::ToolsDb)?;
        self.add_pool_for(key, &HostSchema::new(host, database), &credentials)
    }

    /// Registers a pool from an explicit host and schema plus credentials, for
    /// anything the constructors above do not cover.
    pub fn add_pool_for(
        &mut self,
        key: &str,
        host_schema: &HostSchema,
        credentials: &Credentials,
    ) -> Result<(), DatabaseError> {
        self.insert_pool(
            key,
            host_schema.host(),
            Some(host_schema.schema()),
            credentials,
        )
    }

    /// Builds the connection options field by field rather than as a URL, so
    /// that a password needs no escaping.
    fn insert_pool(
        &mut self,
        key: &str,
        host: &str,
        schema: Option<&str>,
        credentials: &Credentials,
    ) -> Result<(), DatabaseError> {
        let opts = OptsBuilder::default()
            .ip_or_hostname(host)
            .tcp_port(self.port)
            .user(Some(credentials.user()))
            .pass(Some(credentials.password()))
            .db_name(schema);
        let opts = Self::apply_pool_settings(opts, PoolOpts::default(), &self.pool_settings)?;
        self.mysql_pools
            .insert(key.to_string(), mysql_async::Pool::new(opts));
        Ok(())
    }

    /// Normalizes a wiki name to its replica database name, without the `_p`
    /// suffix: the name is lowercased, an already-suffixed name such as
    /// `enwiki_p` loses the suffix so it does not end up doubled, hyphens
    /// become underscores, and the several spellings of be-tarask collapse
    /// into one.
    ///
    /// The result is not checked here; the host-name builders reject anything
    /// that is not a plain database name.
    pub fn fix_wiki_db_name(wiki: &str) -> String {
        let normalized = wiki
            .to_ascii_lowercase()
            .trim_end_matches("_p")
            .replace('-', "_");
        match normalized.as_str() {
            "be_taraskwiki" => "be_x_oldwiki".to_string(),
            _ => normalized,
        }
    }

    /// [`Self::fix_wiki_db_name`], refusing anything but lowercase ASCII
    /// letters, digits and underscores. The name becomes part of a host name,
    /// so a `.` or other stray character could send the connection — and the
    /// tool's credentials — to another server. Fails with
    /// [`DatabaseError::Url`] reporting an invalid `wiki` parameter.
    fn checked_wiki_db_name(wiki: &str) -> Result<String, DatabaseError> {
        let name = Self::fix_wiki_db_name(wiki);
        let plain = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_';
        match !name.is_empty() && name.bytes().all(plain) {
            true => Ok(name),
            false => Err(DatabaseError::Url(
                mysql_async::UrlError::InvalidParamValue {
                    param: "wiki".to_string(),
                    value: wiki.to_string(),
                },
            )),
        }
    }

    pub fn get_pool(&self, key: &str) -> Option<&mysql_async::Pool> {
        self.mysql_pools.get(key)
    }

    pub async fn get_connection(&self, key: &str) -> Result<mysql_async::Conn, DatabaseError> {
        let conn = self
            .get_pool(key)
            .ok_or_else(|| DatabaseError::UnknownPool(key.to_string()))?
            .get_conn()
            .await?;
        Ok(conn)
    }

    /// The Wiki Replica service name used for new host names, `web` by default.
    pub fn server_group(&self) -> DbServerGroup {
        self.server_group
    }

    /// Selects the Wiki Replica service name to build host names from; see
    /// [`DbServerGroup`].
    pub fn set_server_group(&mut self, server_group: DbServerGroup) {
        self.server_group = server_group;
    }

    /// Builder-style variant of [`Self::set_server_group`].
    pub fn with_server_group(mut self, server_group: DbServerGroup) -> Self {
        self.set_server_group(server_group);
        self
    }

    /// Returns the server and database name for the wiki's core cluster.
    ///
    /// Tables that have been split into an extension database are *not*
    /// readable on this host; use [`Self::db_host_and_schema_for_wiki_tables`]
    /// or [`Self::db_host_and_schema_for_wiki_cluster`] for those.
    pub fn db_host_and_schema_for_wiki(&self, wiki: &str) -> Result<HostSchema, DatabaseError> {
        self.db_host_and_schema_for_wiki_cluster(wiki, DbCluster::Core)
    }

    /// Returns the server and database name for one cluster of the wiki.
    ///
    /// Asking for a cluster the wiki does not have falls back to
    /// [`DbCluster::Core`], where those tables still live — so a caller can
    /// always ask for [`DbCluster::Links`] when reading `pagelinks`, whatever
    /// the wiki.
    ///
    /// Fails with [`DatabaseError::Url`] for a wiki name that, normalized as
    /// by [`Self::fix_wiki_db_name`], holds anything but lowercase ASCII
    /// letters, digits and underscores.
    pub fn db_host_and_schema_for_wiki_cluster(
        &self,
        wiki: &str,
        cluster: DbCluster,
    ) -> Result<HostSchema, DatabaseError> {
        let wiki = Self::checked_wiki_db_name(wiki)?;
        let cluster = match cluster.applies_to_wiki(&wiki) {
            true => cluster,
            false => DbCluster::Core,
        };
        let host = self.replica_host(&wiki, cluster);
        let schema = format!("{wiki}_p");
        Ok(HostSchema::new(&host, &schema))
    }

    /// Returns the server and database name of the cluster that can serve a
    /// query reading all of `tables`.
    ///
    /// Fails with [`DatabaseError::TablesSpanClusters`] if the tables are split
    /// over several clusters, which no single connection can join.
    pub fn db_host_and_schema_for_wiki_tables(
        &self,
        wiki: &str,
        tables: &[&str],
    ) -> Result<HostSchema, DatabaseError> {
        let cluster = DbCluster::for_tables(wiki, tables)?;
        self.db_host_and_schema_for_wiki_cluster(wiki, cluster)
    }

    /// The replica host serving a whole [`DbSection`]. Combine with
    /// [`Self::add_pool_for`] to choose the schema yourself, or use
    /// [`Self::add_section_pool`].
    pub fn db_host_for_section(&self, section: DbSection) -> String {
        match self.is_on_toolforge {
            false => "127.0.0.1".to_string(),
            true => format!("{section}.{}{REPLICA_DOMAIN}", self.server_group),
        }
    }

    /// Builds the replica host name for a cluster. Off Toolforge everything is
    /// reached through an SSH tunnel, so there is only ever one host.
    fn replica_host(&self, wiki: &str, cluster: DbCluster) -> String {
        match self.is_on_toolforge {
            false => "127.0.0.1".to_string(),
            true => format!(
                "{prefix}{wiki}.{group}{REPLICA_DOMAIN}",
                prefix = cluster.host_prefix(),
                group = self.server_group,
            ),
        }
    }

    /// The host name of the [ToolsDB](https://wikitech.wikimedia.org/wiki/Help:Toolforge/ToolsDB)
    /// primary, where a tool's own databases live.
    pub fn get_db_host_for_tool_db(&self) -> &str {
        match self.is_on_toolforge {
            true => TOOLSDB_HOST,
            false => "127.0.0.1",
        }
    }

    /// The host name of the read-only ToolsDB replica, recommended over
    /// [`Self::get_db_host_for_tool_db`] for long-running queries so they do
    /// not load the primary. It can lag slightly behind.
    pub fn get_db_host_for_tool_db_readonly(&self) -> &str {
        match self.is_on_toolforge {
            true => TOOLSDB_READONLY_HOST,
            false => "127.0.0.1",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    #[ignore = "requires TFDB env var pointing at a reachable MySQL server"]
    // THIS TEST REQUIRES THE ENVIRONMENT VARIABLE `TFDB` TO BE SET TO A MYSQL URL
    // ALSO, THE MYSQL SERVER MUST BE ACCESSIBLE FROM THE MACHINE RUNNING THE TEST, EG:
    // ssh magnus@tools-login.wmflabs.org -L 3308:tools-db:3306 -N &
    // A local MariaDB works too, e.g.
    // TFDB='mysql://user@localhost/mysql?socket=/tmp/mysql.sock'
    async fn test_db() {
        let url = match std::env::var("TFDB") {
            Ok(val) => val,
            Err(_) => {
                eprintln!("TFDB not set");
                return;
            }
        };
        let mut db = ToolforgeDB::default();
        db.add_mysql_pool("test_good", &json!({"url":url})).unwrap();
        assert!(db
            .add_mysql_pool("test_bad", &json!({"url":"foobar"}))
            .is_err());
        assert!(db.get_connection("test_good").await.is_ok());

        // The statement limit must still be in force on a connection the pool
        // has reset and handed out again, not only on a fresh one. One
        // connection at most, so the second checkout has to reuse the first.
        use mysql_async::prelude::Queryable;
        db.add_mysql_pool(
            "limited",
            &json!({"url": url, "min_connections": 0, "max_connections": 1, "max_statement_time": 42}),
        )
        .unwrap();
        let mut ids = vec![];
        for _ in 0..2 {
            let mut conn = db.get_connection("limited").await.unwrap();
            let limit: Option<f64> = conn
                .query_first("SELECT @@max_statement_time")
                .await
                .unwrap();
            assert_eq!(limit, Some(42.0));
            ids.push(conn.id());
        }
        assert_eq!(ids[0], ids[1], "the connection was not reused");
    }

    #[test]
    fn test_fix_wiki_db_name() {
        assert_eq!(
            ToolforgeDB::fix_wiki_db_name("be-taraskwiki"),
            "be_x_oldwiki"
        );
        assert_eq!(
            ToolforgeDB::fix_wiki_db_name("be_taraskwiki"),
            "be_x_oldwiki"
        );
        assert_eq!(
            ToolforgeDB::fix_wiki_db_name("be-x-oldwiki"),
            "be_x_oldwiki"
        );
        assert_eq!(
            ToolforgeDB::fix_wiki_db_name("be_x_oldwiki"),
            "be_x_oldwiki"
        );
        assert_eq!(
            ToolforgeDB::fix_wiki_db_name("be_x_oldwiki"),
            "be_x_oldwiki"
        );
        assert_eq!(ToolforgeDB::fix_wiki_db_name("enwiki"), "enwiki");
    }

    #[test]
    fn test_fix_wiki_db_name_strips_suffix_and_case_first() {
        // The be-tarask alias must apply after `_p` is stripped ...
        for wiki in ["be-taraskwiki_p", "be_taraskwiki_p", "be-x-oldwiki_p"] {
            assert_eq!(
                ToolforgeDB::fix_wiki_db_name(wiki),
                "be_x_oldwiki",
                "{wiki}"
            );
        }
        // ... and after lowercasing, which also catches an uppercase suffix.
        assert_eq!(
            ToolforgeDB::fix_wiki_db_name("Be-TaraskWiki"),
            "be_x_oldwiki"
        );
        assert_eq!(ToolforgeDB::fix_wiki_db_name("ENWIKI_P"), "enwiki");
        assert_eq!(ToolforgeDB::fix_wiki_db_name("CommonsWiki"), "commonswiki");
        // A mixed-case Commons must still find its links split.
        assert!(DbCluster::Links.applies_to_wiki("CommonsWiki"));
        assert_eq!(
            on_toolforge()
                .db_host_and_schema_for_wiki_tables("CommonsWiki", &["pagelinks"])
                .unwrap(),
            HostSchema::new(
                "links.commonswiki.web.db.svc.wikimedia.cloud",
                "commonswiki_p"
            )
        );
    }

    #[test]
    fn test_wiki_names_that_are_no_database_name_are_rejected() {
        // The wiki name goes into a host name: anything that could redirect
        // the connection, and the credentials with it, must be refused.
        for db in [on_toolforge(), ToolforgeDB::default()] {
            for wiki in [
                "x.evil",
                "evil.com#",
                "en wiki",
                "enwiki/",
                "ünwiki",
                "",
                "_p",
            ] {
                let err = db
                    .db_host_and_schema_for_wiki_cluster(wiki, DbCluster::Core)
                    .unwrap_err();
                assert!(
                    matches!(
                        &err,
                        DatabaseError::Url(mysql_async::UrlError::InvalidParamValue { param, value })
                            if param == "wiki" && value == wiki
                    ),
                    "{wiki:?}: expected an invalid wiki parameter, got: {err:?}"
                );
                assert!(db.db_host_and_schema_for_wiki(wiki).is_err());
                assert!(db
                    .db_host_and_schema_for_wiki_tables(wiki, &["page"])
                    .is_err());
            }
        }
        // Rejected before any credentials are looked for, so this holds off
        // Toolforge too.
        let mut db = on_toolforge();
        assert!(matches!(
            db.add_replica_pool("x", "x.evil", &["page"]),
            Err(DatabaseError::Url(_))
        ));
        assert!(matches!(
            db.add_replica_pool_for_cluster("x", "x.evil", DbCluster::Links),
            Err(DatabaseError::Url(_))
        ));
        assert!(db.get_pool("x").is_none());
        // Ordinary names, digits included, still pass.
        assert!(db.db_host_and_schema_for_wiki("wikidatawiki").is_ok());
        assert!(db.db_host_and_schema_for_wiki("de2wiki").is_ok());
    }

    #[test]
    fn test_debug_does_not_leak_passwords() {
        // `mysql_async::Pool`'s own `Debug` prints the connection password.
        let mut db = on_toolforge();
        db.add_pool_for(
            "replica",
            &HostSchema::new("enwiki.web.db.svc.wikimedia.cloud", "enwiki_p"),
            &Credentials::new("u12345", "sekrit-one"),
        )
        .unwrap();
        db.add_mysql_pool(
            "json",
            &json!({"url": "mysql://u2:sekrit-two@127.0.0.1:3306/d"}),
        )
        .unwrap();
        let debug = format!("{db:?}");
        assert!(!debug.contains("sekrit"), "{debug}");
        assert!(debug.contains("\"json\""), "{debug}");
        assert!(debug.contains("\"replica\""), "{debug}");
        // The pool itself does leak, which is why it is kept out.
        let pool_debug = format!("{:?}", db.get_pool("replica").unwrap());
        assert!(pool_debug.contains("sekrit-one"), "{pool_debug}");
    }

    #[test]
    fn test_get_db_host_for_tool_db() {
        let mut db = ToolforgeDB {
            is_on_toolforge: false,
            ..Default::default()
        };
        assert_eq!(db.get_db_host_for_tool_db(), "127.0.0.1");
        assert_eq!(db.get_db_host_for_tool_db_readonly(), "127.0.0.1");
        db.is_on_toolforge = true;
        assert_eq!(db.get_db_host_for_tool_db(), "tools.db.svc.wikimedia.cloud");
        assert_eq!(
            db.get_db_host_for_tool_db_readonly(),
            "tools-readonly.db.svc.wikimedia.cloud"
        );
    }

    /// A `ToolforgeDB` that behaves as if it were running on Toolforge, so the
    /// host-name building can be tested off it.
    fn on_toolforge() -> ToolforgeDB {
        ToolforgeDB {
            is_on_toolforge: true,
            ..Default::default()
        }
    }

    #[test]
    fn test_server_group_in_host_name() {
        let mut db = on_toolforge();
        assert_eq!(db.server_group(), DbServerGroup::Web);
        assert_eq!(
            db.db_host_and_schema_for_wiki("enwiki").unwrap().host(),
            "enwiki.web.db.svc.wikimedia.cloud"
        );
        db.set_server_group(DbServerGroup::Analytics);
        assert_eq!(
            db.db_host_and_schema_for_wiki("enwiki").unwrap().host(),
            "enwiki.analytics.db.svc.wikimedia.cloud"
        );
        assert_eq!(
            on_toolforge()
                .with_server_group(DbServerGroup::Analytics)
                .db_host_and_schema_for_wiki_cluster("commonswiki", DbCluster::Links)
                .unwrap()
                .host(),
            "links.commonswiki.analytics.db.svc.wikimedia.cloud"
        );
    }

    #[test]
    fn test_db_host_and_schema_for_wiki() {
        let mut db = ToolforgeDB {
            is_on_toolforge: false,
            ..Default::default()
        };
        assert_eq!(
            db.db_host_and_schema_for_wiki("enwiki").unwrap(),
            HostSchema::new("127.0.0.1", "enwiki_p")
        );
        db.is_on_toolforge = true;
        assert_eq!(
            db.db_host_and_schema_for_wiki("enwiki").unwrap(),
            HostSchema::new("enwiki.web.db.svc.wikimedia.cloud", "enwiki_p")
        );
    }

    #[test]
    fn test_off_toolforge_ignores_cluster() {
        // Locally every cluster is reached through the same SSH tunnel, so the
        // cluster must not leak into the host name.
        let db = ToolforgeDB {
            is_on_toolforge: false,
            ..Default::default()
        };
        assert_eq!(
            db.db_host_and_schema_for_wiki_cluster("commonswiki", DbCluster::Links)
                .unwrap(),
            HostSchema::new("127.0.0.1", "commonswiki_p")
        );
    }

    #[test]
    fn test_links_cluster_host_only_for_commons() {
        let db = on_toolforge();
        // Commons and test Commons had their links tables split off ...
        for wiki in ["commonswiki", "testcommonswiki"] {
            assert_eq!(
                db.db_host_and_schema_for_wiki_cluster(wiki, DbCluster::Links)
                    .unwrap(),
                HostSchema::new(
                    &format!("links.{wiki}.web.db.svc.wikimedia.cloud"),
                    &format!("{wiki}_p")
                )
            );
        }
        // ... every other wiki still serves them from its core cluster, so
        // asking for the links cluster must not invent a `links.` host name.
        for wiki in ["enwiki", "wikidatawiki"] {
            assert_eq!(
                db.db_host_and_schema_for_wiki_cluster(wiki, DbCluster::Links)
                    .unwrap(),
                db.db_host_and_schema_for_wiki(wiki).unwrap()
            );
        }
    }

    #[test]
    fn test_db_host_and_schema_for_wiki_is_core() {
        // The long-standing entry point keeps pointing at the core cluster.
        let db = on_toolforge();
        assert_eq!(
            db.db_host_and_schema_for_wiki("commonswiki").unwrap(),
            HostSchema::new("commonswiki.web.db.svc.wikimedia.cloud", "commonswiki_p")
        );
    }

    #[test]
    fn test_cluster_for_tables_commons() {
        // Split-off tables resolve to the links cluster, ...
        assert_eq!(
            DbCluster::for_tables("commonswiki", &["pagelinks", "linktarget"]).unwrap(),
            DbCluster::Links
        );
        // ... `page` and `redirect` exist on both, so they may join them, ...
        assert_eq!(
            DbCluster::for_tables("commonswiki", &["page", "categorylinks"]).unwrap(),
            DbCluster::Links
        );
        // ... on their own they come from the core cluster, ...
        assert_eq!(
            DbCluster::for_tables("commonswiki", &["page", "redirect"]).unwrap(),
            DbCluster::Core
        );
        // ... and so does everything that did not move.
        assert_eq!(
            DbCluster::for_tables("commonswiki", &["image", "revision"]).unwrap(),
            DbCluster::Core
        );
    }

    #[test]
    fn test_cluster_for_tables_rejects_cross_cluster_join() {
        let err = DbCluster::for_tables("commonswiki", &["revision", "pagelinks"]).unwrap_err();
        assert!(
            matches!(
                &err,
                DatabaseError::TablesSpanClusters { wiki, tables }
                    if wiki == "commonswiki" && tables == &["revision", "pagelinks"]
            ),
            "expected TablesSpanClusters, got: {err:?}"
        );
    }

    #[test]
    fn test_cluster_for_tables_other_wikis() {
        // The split is Commons-only: elsewhere the links tables are still on
        // the core cluster.
        assert_eq!(
            DbCluster::for_tables("enwiki", &["revision", "pagelinks"]).unwrap(),
            DbCluster::Core
        );
        // Wikidata's term store is its own extension database.
        assert_eq!(
            DbCluster::for_tables("wikidatawiki", &["wbt_text", "wbt_item_terms"]).unwrap(),
            DbCluster::TermStore
        );
        assert!(DbCluster::for_tables("wikidatawiki", &["page", "wbt_text"]).is_err());
        // ... but `wbt_*` means nothing on a wiki without that split.
        assert_eq!(
            DbCluster::for_tables("enwiki", &["page", "wbt_text"]).unwrap(),
            DbCluster::Core
        );
    }

    #[test]
    fn test_db_host_and_schema_for_wiki_tables() {
        let db = on_toolforge();
        assert_eq!(
            db.db_host_and_schema_for_wiki_tables("commonswiki", &["page", "imagelinks"])
                .unwrap(),
            HostSchema::new(
                "links.commonswiki.web.db.svc.wikimedia.cloud",
                "commonswiki_p"
            )
        );
        assert_eq!(
            db.db_host_and_schema_for_wiki_tables("commonswiki", &["page", "actor"])
                .unwrap(),
            HostSchema::new("commonswiki.web.db.svc.wikimedia.cloud", "commonswiki_p")
        );
        assert!(db
            .db_host_and_schema_for_wiki_tables("commonswiki", &["actor", "imagelinks"])
            .is_err());
    }

    #[test]
    fn test_table_names_are_case_insensitive() {
        assert_eq!(
            DbCluster::for_tables("commonswiki", &["PageLinks"]).unwrap(),
            DbCluster::Links
        );
        assert_eq!(
            DbCluster::for_tables("wikidatawiki", &["WBT_Text"]).unwrap(),
            DbCluster::TermStore
        );
    }

    #[test]
    fn test_cluster_wiki_name_is_normalized() {
        // Hyphenated wiki names must be normalized before the per-wiki lookup.
        assert!(DbCluster::Links.applies_to_wiki("commonswiki"));
        assert!(!DbCluster::Links.applies_to_wiki("enwiki"));
        assert!(!DbCluster::Links.applies_to_wiki("be-taraskwiki"));
        assert!(DbCluster::Core.applies_to_wiki("be-taraskwiki"));
        assert_eq!(
            on_toolforge()
                .db_host_and_schema_for_wiki_cluster("be-taraskwiki", DbCluster::Links)
                .unwrap(),
            HostSchema::new("be_x_oldwiki.web.db.svc.wikimedia.cloud", "be_x_oldwiki_p")
        );
    }

    #[test]
    fn test_pool_settings_defaults_follow_the_policy() {
        let settings = PoolSettings::default();
        // Nothing held while unused, and no more than a tool's ten connections.
        assert_eq!(settings.min_connections(), 0);
        assert_eq!(settings.max_connections(), 10);
        // One second, so connections are reused but not hoarded.
        assert_eq!(settings.keep_sec(), 1);
        assert_eq!(settings.max_statement_time(), None);
        assert!(settings.setup_commands().is_empty());
    }

    #[test]
    fn test_pool_settings_from_json() {
        let settings = PoolSettings::from_json(&json!({
            "url": "mysql://u@127.0.0.1:3306/d",
            "min_connections": 1,
            "max_connections": 4,
            "keep_sec": 2,
            "max_statement_time": 300
        }));
        assert_eq!(settings.min_connections(), 1);
        assert_eq!(settings.max_connections(), 4);
        assert_eq!(settings.keep_sec(), 2);
        assert_eq!(
            settings.max_statement_time(),
            Some(Duration::from_secs(300))
        );

        // Absent keys fall back to the defaults, ...
        assert_eq!(PoolSettings::from_json(&json!({})), PoolSettings::default());
        // ... and so do unusable values, matching how the rest of the config
        // parsing tolerates junk.
        let junk = PoolSettings::from_json(
            &json!({"max_connections": "lots", "max_statement_time": "soon"}),
        );
        assert_eq!(junk, PoolSettings::default());
        assert_eq!(
            PoolSettings::from_json(&json!({"max_statement_time": -1})).max_statement_time(),
            None
        );
    }

    #[test]
    fn test_max_statement_time_becomes_a_setup_command() {
        // The limit has to be set per connection, so it rides along as a setup
        // statement rather than being applied once.
        let settings =
            PoolSettings::default().with_max_statement_time(Some(Duration::from_secs(300)));
        assert_eq!(settings.setup_commands(), ["SET max_statement_time = 300"]);
        // Sub-second limits are legal: MariaDB takes decimals.
        let settings =
            PoolSettings::default().with_max_statement_time(Some(Duration::from_millis(1500)));
        assert_eq!(settings.setup_commands(), ["SET max_statement_time = 1.5"]);
    }

    #[test]
    fn test_zero_max_statement_time_is_no_limit() {
        // MariaDB reads 0 as "no limit", so it must not be sent as if it were
        // one -- whether it comes from code or from JSON.
        let settings = PoolSettings::default().with_max_statement_time(Some(Duration::ZERO));
        assert_eq!(settings.max_statement_time(), None);
        assert!(settings.setup_commands().is_empty());
        for value in [json!(0), json!(0.0), json!(1e-12)] {
            let settings = PoolSettings::from_json(&json!({ "max_statement_time": value }));
            assert_eq!(settings.max_statement_time(), None, "{value}");
        }
    }

    #[test]
    fn test_tiny_max_statement_time_is_rounded_up() {
        // MariaDB truncates to microseconds, which would turn a tiny positive
        // limit into 0, i.e. none at all.
        for limit in [Duration::from_nanos(1), Duration::from_nanos(100)] {
            let settings = PoolSettings::default().with_max_statement_time(Some(limit));
            assert_eq!(
                settings.setup_commands(),
                ["SET max_statement_time = 0.000001"]
            );
        }
        let settings =
            PoolSettings::default().with_max_statement_time(Some(Duration::from_nanos(1_000_001)));
        assert_eq!(
            settings.setup_commands(),
            ["SET max_statement_time = 0.001001"]
        );
    }

    #[test]
    fn test_max_statement_time_survives_connection_reset() {
        // `init` statements run only when a connection is opened, and the
        // pool's reset on reuse wipes session variables; only `setup`
        // statements are run again after it.
        let settings =
            PoolSettings::default().with_max_statement_time(Some(Duration::from_secs(60)));
        let opts = ToolforgeDB::apply_pool_settings(
            OptsBuilder::default(),
            PoolOpts::default(),
            &settings,
        )
        .unwrap();
        assert_eq!(opts.setup(), ["SET max_statement_time = 60"]);
        assert!(opts.init().is_empty());
        // The same through the JSON path.
        let opts = ToolforgeDB::default()
            .opts_from_json(&json!({
                "url": "mysql://u@127.0.0.1:3306/d",
                "max_statement_time": 60
            }))
            .unwrap();
        assert_eq!(opts.setup(), ["SET max_statement_time = 60"]);
        assert!(opts.init().is_empty());
    }

    #[test]
    fn test_pool_settings_reject_min_above_max() {
        let err = PoolSettings::default()
            .with_min_connections(10)
            .with_max_connections(2)
            .pool_opts(PoolOpts::default())
            .unwrap_err();
        assert!(
            matches!(
                &err,
                DatabaseError::InvalidPoolConstraints { min: 10, max: 2 }
            ),
            "expected InvalidPoolConstraints, got: {err:?}"
        );
    }

    #[test]
    fn test_pool_settings_are_used_by_new_pools() {
        // `Pool::new` is lazy, so this exercises the whole build path offline.
        let mut db = on_toolforge().with_pool_settings(
            PoolSettings::default()
                .with_keep_sec(30)
                .with_max_statement_time(Some(Duration::from_secs(60))),
        );
        assert_eq!(db.pool_settings().keep_sec(), 30);
        db.add_pool_for(
            "replica",
            &HostSchema::new("enwiki.web.db.svc.wikimedia.cloud", "enwiki_p"),
            &Credentials::new("u12345", "pw"),
        )
        .unwrap();
        assert!(db.get_pool("replica").is_some());
    }

    #[test]
    fn test_add_pool_for_rejects_bad_settings() {
        let mut db =
            on_toolforge().with_pool_settings(PoolSettings::default().with_min_connections(99));
        assert!(db
            .add_pool_for(
                "replica",
                &HostSchema::new("host", "enwiki_p"),
                &Credentials::new("u12345", "pw"),
            )
            .is_err());
        assert!(db.get_pool("replica").is_none());
    }

    #[test]
    fn test_port_defaults_to_mariadb() {
        assert_eq!(ToolforgeDB::default().port(), 3306);
        // An SSH tunnel for local development usually listens elsewhere.
        assert_eq!(ToolforgeDB::default().with_port(3308).port(), 3308);
    }

    #[test]
    fn test_add_replica_pool_checks_the_tables_first() {
        // A cross-cluster table set must fail on the tables, not on whether
        // this machine happens to have Toolforge credentials.
        let mut db = on_toolforge();
        let err = db
            .add_replica_pool("x", "commonswiki", &["actor", "pagelinks"])
            .unwrap_err();
        assert!(
            matches!(&err, DatabaseError::TablesSpanClusters { .. }),
            "expected TablesSpanClusters, got: {err:?}"
        );
        assert!(db.get_pool("x").is_none());
    }

    #[test]
    fn test_db_section_parsing() {
        assert_eq!(DbSection::parse("s4").map(|s| s.number()), Some(4));
        assert_eq!(DbSection::parse("S4"), DbSection::new(4));
        assert_eq!(DbSection::from_meta_slice("s3.labsdb"), DbSection::new(3));
        assert_eq!(DbSection::from_meta_slice("s11"), DbSection::new(11));
        // There is no section 0, and an extension database is addressed by its
        // `DbCluster` name rather than by its `x` section.
        assert_eq!(DbSection::new(0), None);
        assert_eq!(DbSection::parse("s0"), None);
        assert_eq!(DbSection::parse("x4"), None);
        assert_eq!(DbSection::parse("enwiki"), None);
        assert_eq!(DbSection::parse(""), None);
        assert_eq!(DbSection::parse("s999"), None);
    }

    #[test]
    fn test_db_section_display() {
        assert_eq!(DbSection::new(4).unwrap().to_string(), "s4");
    }

    #[test]
    fn test_db_host_for_section() {
        let section = DbSection::new(4).unwrap();
        assert_eq!(
            on_toolforge().db_host_for_section(section),
            "s4.web.db.svc.wikimedia.cloud"
        );
        assert_eq!(
            on_toolforge()
                .with_server_group(DbServerGroup::Analytics)
                .db_host_for_section(section),
            "s4.analytics.db.svc.wikimedia.cloud"
        );
        let local = ToolforgeDB {
            is_on_toolforge: false,
            ..Default::default()
        };
        assert_eq!(local.db_host_for_section(section), "127.0.0.1");
    }

    #[test]
    fn test_host_prefix() {
        assert_eq!(DbCluster::Core.host_prefix(), "");
        assert_eq!(DbCluster::Links.host_prefix(), "links.");
        assert_eq!(DbCluster::TermStore.host_prefix(), "termstore.");
        // The core cluster is the default, so `DbCluster::default()` keeps the
        // pre-split behaviour.
        assert_eq!(DbCluster::default(), DbCluster::Core);
    }

    #[test]
    fn test_every_cluster_is_listed_in_all() {
        // `ALL` drives table resolution: a variant missing from it would be
        // unreachable. Core must come first so shared tables prefer it.
        assert_eq!(DbCluster::ALL.first(), Some(&DbCluster::Core));
        for cluster in DbCluster::EXTENSIONS {
            assert!(DbCluster::ALL.contains(cluster));
            assert!(!cluster.host_prefix().is_empty());
        }
        assert_eq!(DbCluster::ALL.len(), DbCluster::EXTENSIONS.len() + 1);
    }

    #[test]
    fn test_host_schema_accessors() {
        let hs = HostSchema::new("myhost.example.com", "mydb_p");
        assert_eq!(hs.host(), "myhost.example.com");
        assert_eq!(hs.schema(), "mydb_p");
    }

    #[test]
    fn test_host_schema_equality() {
        let hs1 = HostSchema::new("host", "schema");
        let hs2 = HostSchema::new("host", "schema");
        let hs3 = HostSchema::new("other", "schema");
        assert_eq!(hs1, hs2);
        assert_ne!(hs1, hs3);
    }

    #[test]
    fn test_get_pool_missing_key() {
        let db = ToolforgeDB::default();
        assert!(db.get_pool("nonexistent").is_none());
    }

    #[test]
    fn test_add_mysql_pool_missing_url_returns_err() {
        // A config object without a "url" key must produce an Err, not a panic.
        let mut db = ToolforgeDB::default();
        let result = db.add_mysql_pool("test", &json!({}));
        assert!(
            result.is_err(),
            "add_mysql_pool must return Err when 'url' is absent from the config"
        );
        // The pool must not have been registered.
        assert!(db.get_pool("test").is_none());
    }

    #[test]
    fn test_add_mysql_pool_invalid_url_returns_err() {
        // A config object with a syntactically invalid URL must also return Err.
        let mut db = ToolforgeDB::default();
        let result = db.add_mysql_pool("bad", &json!({"url": "not-a-valid-mysql-url"}));
        assert!(result.is_err());
    }

    #[test]
    fn test_fix_wiki_db_name_strips_p_suffix() {
        // Callers hold database names both with and without the suffix; either
        // must end up as exactly one `_p` in the schema name.
        assert_eq!(ToolforgeDB::fix_wiki_db_name("enwiki_p"), "enwiki");
        assert_eq!(
            ToolforgeDB::fix_wiki_db_name("commonswiki_p"),
            "commonswiki"
        );
        let db = on_toolforge();
        assert_eq!(
            db.db_host_and_schema_for_wiki("enwiki_p").unwrap(),
            db.db_host_and_schema_for_wiki("enwiki").unwrap()
        );
        // ... and the per-wiki cluster lookup must still recognise the wiki.
        assert_eq!(
            DbCluster::for_tables("commonswiki_p", &["pagelinks"]).unwrap(),
            DbCluster::Links
        );
    }

    #[test]
    fn test_fix_wiki_db_name_hyphen_to_underscore() {
        // Hyphens in ordinary wiki names (not the special be-tarask cases) must be
        // converted to underscores.
        assert_eq!(
            ToolforgeDB::fix_wiki_db_name("zh-min-nanwiki"),
            "zh_min_nanwiki"
        );
    }

    #[test]
    fn test_add_mysql_pool_valid_url_registers_pool() {
        // `Pool::new` is lazy -- it does not open a connection -- so the whole
        // config-parsing path can be exercised offline against an address that
        // nothing is listening on.
        let mut db = ToolforgeDB::default();
        let config = json!({
            "url": "mysql://user:pass@127.0.0.1:3306/somedb",
            "min_connections": 1,
            "max_connections": 4,
            "keep_sec": 30
        });

        db.add_mysql_pool("good", &config).unwrap();

        assert!(db.get_pool("good").is_some());
        assert!(db.get_pool("other").is_none());
    }

    #[test]
    fn test_add_mysql_pool_applies_config_defaults() {
        // Omitted min/max/keep values fall back to defaults rather than failing.
        let mut db = ToolforgeDB::default();
        db.add_mysql_pool("defaults", &json!({"url": "mysql://u@127.0.0.1:3306/d"}))
            .unwrap();
        assert!(db.get_pool("defaults").is_some());
    }

    /// The pool options `add_mysql_pool` would build for `config`.
    fn json_pool_opts(db: &ToolforgeDB, config: Value) -> PoolOpts {
        db.opts_from_json(&config).unwrap().pool_opts().clone()
    }

    #[test]
    fn test_add_mysql_pool_without_settings_follows_the_policy() {
        // Not `mysql_async`'s own defaults (min 10, max 100, TTL 0).
        let opts = json_pool_opts(
            &ToolforgeDB::default(),
            json!({"url": "mysql://u@127.0.0.1:3306/d"}),
        );
        assert_eq!(opts.constraints().min(), 0);
        assert_eq!(opts.constraints().max(), 10);
        assert_eq!(opts.inactive_connection_ttl(), Duration::from_secs(1));
        assert!(opts.reset_connection());
    }

    #[test]
    fn test_add_mysql_pool_falls_back_to_pool_settings() {
        // Keys missing from the JSON come from `set_pool_settings`, ...
        let db = ToolforgeDB::default().with_pool_settings(
            PoolSettings::default()
                .with_max_connections(4)
                .with_keep_sec(5)
                .with_max_statement_time(Some(Duration::from_secs(30))),
        );
        let opts = db
            .opts_from_json(&json!({"url": "mysql://u@127.0.0.1:3306/d", "keep_sec": 2}))
            .unwrap();
        assert_eq!(opts.pool_opts().constraints().max(), 4);
        assert_eq!(opts.setup(), ["SET max_statement_time = 30"]);
        // ... while the keys it does give win.
        assert_eq!(
            opts.pool_opts().inactive_connection_ttl(),
            Duration::from_secs(2)
        );
        // An explicit 0 lifts the statement limit the fallback would set.
        let opts = db
            .opts_from_json(&json!({"url": "mysql://u@127.0.0.1:3306/d", "max_statement_time": 0}))
            .unwrap();
        assert!(opts.setup().is_empty());
    }

    #[test]
    fn test_add_mysql_pool_keeps_url_pool_parameters() {
        let db = ToolforgeDB::default();
        let url = "mysql://u@127.0.0.1:3306/d?pool_min=0&pool_max=2\
                   &inactive_connection_ttl=7&reset_connection=false&ttl_check_interval=9";
        let opts = json_pool_opts(&db, json!({ "url": url }));
        assert_eq!(opts.constraints().min(), 0);
        assert_eq!(opts.constraints().max(), 2);
        assert_eq!(opts.inactive_connection_ttl(), Duration::from_secs(7));
        // Parameters `PoolSettings` has no say over are kept as well.
        assert!(!opts.reset_connection());
        assert_eq!(opts.ttl_check_interval(), Duration::from_secs(9));

        // A key given in the JSON overrides the URL ...
        let opts = json_pool_opts(
            &db,
            json!({ "url": url, "max_connections": 3, "keep_sec": 1 }),
        );
        assert_eq!(opts.constraints().max(), 3);
        assert_eq!(opts.inactive_connection_ttl(), Duration::from_secs(1));
        assert!(!opts.reset_connection());

        // ... and a parameter the URL leaves out takes the policy default, not
        // `mysql_async`'s maximum of 100.
        let opts = json_pool_opts(&db, json!({"url": "mysql://u@127.0.0.1:3306/d?pool_min=0"}));
        assert_eq!(opts.constraints().max(), 10);
        assert_eq!(opts.inactive_connection_ttl(), Duration::from_secs(1));
    }

    #[test]
    fn test_url_query_keys() {
        let keys = |url| url_query_keys(url).collect::<Vec<_>>();
        assert_eq!(keys("mysql://h/d?a=1&b&c=x=y#frag"), ["a", "b", "c"]);
        assert_eq!(keys("mysql://h/d"), [""]);
        assert!(!url_query_keys("mysql://h/d#pool_min=1").any(|key| key == "pool_min"));
    }

    #[tokio::test]
    async fn test_get_connection_unknown_pool_is_err() {
        // Asking for a pool that was never registered must fail with
        // DatabaseError::UnknownPool rather than panicking.
        let db = ToolforgeDB::default();
        let err = db.get_connection("nope").await.unwrap_err();
        assert!(
            matches!(&err, DatabaseError::UnknownPool(name) if name == "nope"),
            "expected UnknownPool, got: {err:?}"
        );
    }
    #[test]
    fn test_add_mysql_pool_min_above_max_returns_err() {
        // min_connections > max_connections is rejected by PoolConstraints; since
        // both come from caller-supplied JSON this must be an error, not a panic.
        let mut db = ToolforgeDB::default();
        let err = db
            .add_mysql_pool(
                "bad",
                &json!({
                    "url": "mysql://u@127.0.0.1:3306/d",
                    "min_connections": 10,
                    "max_connections": 2
                }),
            )
            .unwrap_err();

        assert!(
            matches!(
                &err,
                DatabaseError::InvalidPoolConstraints { min: 10, max: 2 }
            ),
            "expected InvalidPoolConstraints, got: {err:?}"
        );
        assert!(db.get_pool("bad").is_none());
    }
}
