use anyhow::{anyhow, Result};
use reqwest::ClientBuilder;
use std::{
    fs::File,
    io::{Seek, Write},
    time::Duration,
};
use tempfile::tempfile;
use wikibase::{mediawiki::Api, EntityTrait, ItemEntity, SnakType};

const WIKIDATA_USER_AGENT: &str = "wikimisc-wikidata/0.1.0";
const WIKIDATA_SPARQL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default)]
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

    pub async fn api(&self) -> Result<Api> {
        let api_url = "https://www.wikidata.org/w/api.php";
        let api = Api::new_from_builder(api_url, self.client_builder()).await?;
        Ok(api)
    }

    /// Returns a reqwest client with the current user agent and timeout.
    pub fn reqwest_client(&self) -> Result<reqwest::Client> {
        Ok(self.client_builder().build()?)
    }

    pub fn client_builder(&self) -> ClientBuilder {
        reqwest::Client::builder()
            .user_agent(&self.user_agent)
            .timeout(self.timeout.to_owned())
    }

    #[cfg(not(doctest))]
    /// Queries SPARQL and returns a filename with the result as CSV.
    /// USAGE:
    /// ```rust
    /// let mut reader = self.mnm.load_sparql_csv(&sparql).await?;
    /// for result in reader.records() {
    ///     let record = result.unwrap();
    /// }
    /// ```
    pub async fn load_sparql_csv(&self, sparql: &str) -> Result<csv::Reader<File>> {
        let url = format!("https://query.wikidata.org/sparql?query={sparql}");
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

    pub fn item2qs(&self, item: &ItemEntity) -> Result<Vec<String>> {
        if !item.id().is_empty() {
            return Err(anyhow!("Wikimisc::Wikidata::item2qs: Item ID is not empty"));
        }
        let mut ret = vec!["CREATE".into()];
        ret.extend(
            item.labels()
                .iter()
                .map(|ls| format!("LAST\tL{}\t\"{}\"", ls.language(), ls.value()))
                .collect::<Vec<String>>(),
        );
        ret.extend(
            item.descriptions()
                .iter()
                .map(|ls| format!("LAST\tD{}\t\"{}\"", ls.language(), ls.value()))
                .collect::<Vec<String>>(),
        );
        ret.extend(
            item.aliases()
                .iter()
                .map(|ls| format!("LAST\tA{}\t\"{}\"", ls.language(), ls.value()))
                .collect::<Vec<String>>(),
        );
        ret.extend(
            item.sitelinks()
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|sl| format!("LAST\tS{}\t\"{}\"", sl.site(), sl.title()))
                .collect::<Vec<String>>(),
        );
        for statement in item.claims() {
            let main_value = match Self::snak2qs(statement.main_snak()) {
                Some(value) => value,
                None => continue,
            };
            let qualifiers = statement
                .qualifiers()
                .iter()
                .filter_map(|snak| {
                    let v = Self::snak2qs(snak)?;
                    Some(vec![snak.property().to_string(), v])
                })
                .flatten()
                .collect::<Vec<String>>();
            let references = statement
                .references()
                .iter()
                .flat_map(|r| {
                    let mut parts = r
                        .snaks()
                        .iter()
                        .filter_map(|snak| {
                            let v = Self::snak2qs(snak)?;
                            let p = snak.property().replace('P', "S");
                            Some(vec![p, v])
                        })
                        .flatten()
                        .collect::<Vec<String>>();
                    parts[0] = format!("!{}", parts[0]); // Start new reference section
                    parts
                })
                .collect::<Vec<String>>();
            let mut parts = vec![main_value];
            parts.extend(qualifiers);
            parts.extend(references);
            ret.push(format!(
                "LAST\t{}\t{}",
                statement.property(),
                parts.join("\t")
            ));
        }
        Ok(ret)
    }

    fn snak2qs(snak: &wikibase::Snak) -> Option<String> {
        if *snak.snak_type() != SnakType::Value {
            return None;
        }
        let dv = match snak.data_value() {
            Some(dv) => dv,
            None => return None,
        };
        let value = match dv.value() {
            wikibase::Value::Coordinate(c) => format!("@{}/{}", c.latitude(), c.longitude()),
            wikibase::Value::MonoLingual(m) => format!("{}:\"{}\"", m.language(), m.text()),
            wikibase::Value::Entity(entity) => format!("\"{}\"", entity.id()),
            wikibase::Value::Quantity(quantity) => format!("{}", quantity.amount()), // TODO unit?
            wikibase::Value::StringValue(s) => format!("\"{s}\""),
            wikibase::Value::Time(time) => format!("{}/{}", time.time(), time.precision()),
            // wikibase::Value::EntitySchema(_entity_schema) => continue, // TODO
            _ => return None, // TODO
        };
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_load_sparql_csv() {
        let wd = Wikidata::new();
        let sparql = "SELECT ?item ?itemLabel WHERE { ?item wdt:P31 wd:Q34038. SERVICE wikibase:label { bd:serviceParam wikibase:language 'en'. }} LIMIT 5";
        let mut reader = wd.load_sparql_csv(sparql).await.unwrap();
        let mut count = 0;
        for _result in reader.records() {
            // let record = result.unwrap();
            // println!("{:?}", record);
            count += 1;
        }
        assert_eq!(count, 5);
    }
}
