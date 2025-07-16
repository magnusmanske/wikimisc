use crate::toolforge_app::ToolforgeApp;
use anyhow::{anyhow, Result};
use core::time::Duration;
use mysql_async::{Opts, OptsBuilder, PoolConstraints, PoolOpts};
use serde_json::Value;
use std::collections::HashMap;

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

#[derive(Debug)]
pub struct ToolforgeDB {
    mysql_pools: HashMap<String, mysql_async::Pool>,
    is_on_toolforge: bool,
}

impl Default for ToolforgeDB {
    fn default() -> Self {
        Self {
            mysql_pools: HashMap::new(),
            is_on_toolforge: ToolforgeApp::is_on_toolforge(),
        }
    }
}

impl ToolforgeDB {
    pub fn add_mysql_pool(&mut self, key: &str, config: &Value) -> Result<()> {
        self.mysql_pools
            .insert(key.to_string(), Self::create_pool(config)?);
        Ok(())
    }

    /// Helper function to create a DB pool from a JSON config object
    fn create_pool(config: &Value) -> Result<mysql_async::Pool> {
        let min_connections = config["min_connections"].as_u64().unwrap_or(0) as usize;
        let max_connections = config["max_connections"].as_u64().unwrap_or(10) as usize;
        let keep_sec = config["keep_sec"].as_u64().unwrap_or(0);
        let url = config["url"].as_str().expect("No url value");
        let pool_opts = PoolOpts::default()
            .with_constraints(
                PoolConstraints::new(min_connections, max_connections).expect("Constraints error"),
            )
            .with_inactive_connection_ttl(Duration::from_secs(keep_sec));
        let wd_url = url;
        let wd_opts = Opts::from_url(wd_url)?;
        let pool =
            mysql_async::Pool::new(OptsBuilder::from_opts(wd_opts).pool_opts(pool_opts.clone()));
        Ok(pool)
    }

    pub fn fix_wiki_db_name(wiki: &str) -> String {
        match wiki {
            "be-taraskwiki" | "be-x-oldwiki" | "be_taraskwiki" | "be_x_oldwiki" => "be_x_oldwiki",
            other => other,
        }
        .replace('-', "_")
    }

    pub fn get_pool(&self, key: &str) -> Option<&mysql_async::Pool> {
        self.mysql_pools.get(key)
    }

    pub async fn get_connection(&self, key: &str) -> Result<mysql_async::Conn> {
        let conn = self
            .get_pool(key)
            .ok_or_else(|| anyhow!("No pool '{key}' found"))?
            .get_conn()
            .await?;
        Ok(conn)
    }

    /// Returns the server and database name for the wiki, as a tuple
    pub fn db_host_and_schema_for_wiki(&self, wiki: &str) -> Result<HostSchema> {
        let wiki = Self::fix_wiki_db_name(wiki);
        let host = match self.is_on_toolforge {
            false => "127.0.0.1".to_string(),
            true => wiki.to_owned() + self.get_db_server_group(),
        };
        let schema = format!("{wiki}_p");
        Ok(HostSchema::new(&host, &schema))
    }

    fn get_db_server_group(&self) -> &str {
        match self.is_on_toolforge {
            true => ".web.db.svc.eqiad.wmflabs",
            false => "",
        }
    }

    /// Returns the server and database name for the tool db, as a tuple
    pub fn get_db_host_for_tool_db(&self) -> &str {
        match self.is_on_toolforge {
            true => "tools.labsdb",
            false => "127.0.0.1",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    // THIS TEST REQUIRES THE ENVIRONMENT VARIABLE `TFDB` TO BE SET TO A MYSQL URL
    // ALSO, THE MYSQL SERVER MUST BE ACCESSIBLE FROM THE MACHINE RUNNING THE TEST, EG:
    // ssh magnus@tools-login.wmflabs.org -L 3308:tools-db:3306 -N &
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
    fn test_get_db_host_for_tool_db() {
        let mut db = ToolforgeDB {
            is_on_toolforge: false,
            ..Default::default()
        };
        assert_eq!(db.get_db_host_for_tool_db(), "127.0.0.1");
        db.is_on_toolforge = true;
        assert_eq!(db.get_db_host_for_tool_db(), "tools.labsdb");
    }

    #[test]
    fn test_get_db_server_group() {
        let mut db = ToolforgeDB {
            is_on_toolforge: false,
            ..Default::default()
        };
        assert_eq!(db.get_db_server_group(), "");
        db.is_on_toolforge = true;
        assert_eq!(db.get_db_server_group(), ".web.db.svc.eqiad.wmflabs");
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
            HostSchema::new("enwiki.web.db.svc.eqiad.wmflabs", "enwiki_p")
        );
    }
}
