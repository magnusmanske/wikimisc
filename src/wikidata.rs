use anyhow::Result;
use std::{
    fs::File,
    io::{Seek, Write},
    time::Duration,
};
use tempfile::tempfile;

const WIKIDATA_USER_AGENT: &str = "wikimisc-wikidata/0.1.0";
const WIKIDATA_SPARQL_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Wikidata {
    user_agent: String,
    timeout: Duration,
}

impl Wikidata {
    pub fn new() -> Self {
        Wikidata {
            user_agent: WIKIDATA_USER_AGENT.to_string(),
            timeout: WIKIDATA_SPARQL_TIMEOUT.to_owned(),
        }
    }

    /// Returns a reqwest client with the current user agent and timeout.
    pub fn reqwest_client(&self) -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .user_agent(&self.user_agent)
            .timeout(self.timeout.to_owned())
            .build()?)
    }

    /// Queries SPARQL and returns a filename with the result as CSV.
    /// USAGE:
    /// ```rust
    /// let mut reader = self.mnm.load_sparql_csv(&sparql).await?;
    /// for result in reader.records() {
    ///     let record = result.unwrap();
    /// }
    /// ```
    pub async fn load_sparql_csv(&self, sparql: &str) -> Result<csv::Reader<File>> {
        let url = format!("https://query.wikidata.org/sparql?query={}", sparql);
        let mut f = tempfile()?;
        let mut res = self
            .reqwest_client()?
            .get(url)
            .header(
                reqwest::header::ACCEPT,
                reqwest::header::HeaderValue::from_str("text/csv")?,
            )
            .send()
            .await?;
        while let Some(chunk) = res.chunk().await? {
            f.write_all(chunk.as_ref())?;
        }
        f.seek(std::io::SeekFrom::Start(0))?;
        Ok(csv::ReaderBuilder::new()
            .flexible(true)
            .has_headers(true)
            .delimiter(b',')
            .from_reader(f))
    }

    pub fn set_user_agent(&mut self, user_agent: &str) {
        self.user_agent = user_agent.to_string();
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_load_sparql_csv() {
        let wd = Wikidata::new();
        let sparql = "SELECT ?item ?itemLabel WHERE { ?item wdt:P31 wd:Q34038. SERVICE wikibase:label { bd:serviceParam wikibase:language 'en'. }} LIMIT 5";
        let mut reader = wd.load_sparql_csv(&sparql).await.unwrap();
        let mut count = 0;
        for result in reader.records() {
            let record = result.unwrap();
            println!("{:?}", record);
            count += 1;
        }
        assert_eq!(count, 5);
    }
}