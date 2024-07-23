///! UNDER CONSTRUCTION
use anyhow::Result;
use core::time::Duration;
use mysql_async::{Opts, OptsBuilder, PoolConstraints, PoolOpts};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct ToolforgeDB {
    mysql_pools: HashMap<String, mysql_async::Pool>,
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

    // fn get_mysql_opts_for_wiki(
    //     &self,
    //     wiki: &str,
    //     user: &str,
    //     pass: &str,
    // ) -> Result<mysql_async::Opts, String> {
    //     let (host, schema) = self.db_host_and_schema_for_wiki(wiki)?;
    //     let port: u16 = match self.port_mapping.get(wiki) {
    //         Some(port) => *port,
    //         None => self.config["db_port"].as_u64().unwrap_or(3306) as u16,
    //     };
    //     let opts = mysql_async::OptsBuilder::default()
    //         .ip_or_hostname(host)
    //         .db_name(Some(schema))
    //         .user(Some(user))
    //         .pass(Some(pass))
    //         .tcp_port(port)
    //         .into();
    //     Ok(opts)
    // }

    pub fn fix_wiki_db_name(wiki: &str) -> String {
        match wiki {
            "be-taraskwiki" | "be-x-oldwiki" | "be_taraskwiki" | "be_x_oldwiki" => "be_x_oldwiki",
            other => other,
        }
        .to_string()
        .replace('-', "_")
    }

    /// Returns the server and database name for the wiki, as a tuple
    pub fn db_host_and_schema_for_wiki(&self, wiki: &str) -> Result<(String, String), String> {
        let wiki = Self::fix_wiki_db_name(wiki);
        let host = match Self::is_on_toolforge() {
            false => "127.0.0.1".to_string(),
            true => wiki.to_owned() + self.get_db_server_group(),
        };
        let schema = format!("{}_p", wiki);
        Ok((host, schema))
    }

    fn get_db_server_group(&self) -> &str {
        match Self::is_on_toolforge() {
            true => ".web.db.svc.eqiad.wmflabs",
            false => "",
        }
    }

    /// Returns the server and database name for the tool db, as a tuple
    pub fn get_db_host_for_tool_db(&self) -> &str {
        match Self::is_on_toolforge() {
            true => "tools.labsdb",
            false => "127.0.0.1",
        }
    }

    pub fn is_on_toolforge() -> bool {
        std::path::Path::new("/etc/wmcs-project").exists()
    }
}
