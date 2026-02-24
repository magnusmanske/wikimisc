use crate::mediawiki::reqwest::{Client, ClientBuilder};
use anyhow::{anyhow, Result};
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
    pub fn reqwest_client(&self) -> Result<Client> {
        Ok(self.client_builder().build()?)
    }

    pub fn client_builder(&self) -> ClientBuilder {
        Client::builder()
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
        let mut f = tempfile()?;
        let mut res = self
            .reqwest_client()?
            .get("https://query.wikidata.org/sparql")
            .query(&[("query", sparql)])
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

    pub fn item2qs(item: &ItemEntity) -> Result<Vec<String>> {
        if !item.id().is_empty() {
            return Err(anyhow!("Wikimisc::Wikidata::item2qs: Item ID is not empty"));
        }

        let mut ret = vec!["CREATE".to_string()];

        // Add labels
        ret.extend(
            item.labels()
                .iter()
                .map(|ls| format!("LAST\tL{}\t\"{}\"", ls.language(), ls.value())),
        );

        // Add descriptions
        ret.extend(
            item.descriptions()
                .iter()
                .map(|ls| format!("LAST\tD{}\t\"{}\"", ls.language(), ls.value())),
        );

        // Add aliases
        ret.extend(
            item.aliases()
                .iter()
                .map(|ls| format!("LAST\tA{}\t\"{}\"", ls.language(), ls.value())),
        );

        // Add sitelinks
        ret.extend(
            item.sitelinks()
                .as_ref()
                .map(|sl| sl.as_slice())
                .unwrap_or(&[])
                .iter()
                .map(|sl| format!("LAST\tS{}\t\"{}\"", sl.site(), sl.title())),
        );

        // Add statements
        for statement in item.claims() {
            let main_value = match Self::snak2qs(statement.main_snak()) {
                Some(value) => value,
                None => continue,
            };

            let qualifiers: Vec<String> = statement
                .qualifiers()
                .iter()
                .filter_map(|snak| {
                    let v = Self::snak2qs(snak)?;
                    Some(vec![snak.property().to_string(), v])
                })
                .flatten()
                .collect();

            let references: Vec<String> = statement
                .references()
                .iter()
                .flat_map(|r| {
                    let mut parts: Vec<String> = r
                        .snaks()
                        .iter()
                        .filter_map(|snak| {
                            let v = Self::snak2qs(snak)?;
                            let p = snak.property().replace('P', "S");
                            Some(vec![p, v])
                        })
                        .flatten()
                        .collect();
                    if !parts.is_empty() {
                        parts[0] = format!("!{}", parts[0]); // Start new reference section
                    }
                    parts
                })
                .collect();

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
        let dv = snak.data_value().as_ref()?;
        let value = match dv.value() {
            wikibase::Value::Coordinate(c) => format!("@{}/{}", c.latitude(), c.longitude()),
            wikibase::Value::MonoLingual(m) => format!("{}:\"{}\"", m.language(), m.text()),
            wikibase::Value::Entity(entity) => format!("\"{}\"", entity.id()),
            wikibase::Value::Quantity(quantity) => quantity.amount().to_string(),
            wikibase::Value::StringValue(s) => format!("\"{s}\""),
            wikibase::Value::Time(time) => format!("{}/{}", time.time(), time.precision()),
            _ => return None,
        };
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wikibase::*;

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

    #[test]
    fn test_item2qs_rejects_item_with_id() {
        let mut item = ItemEntity::new_empty();
        item.set_id("Q123".to_string());
        assert!(Wikidata::item2qs(&item).is_err());
    }

    #[test]
    fn test_item2qs_empty_item() {
        let item = ItemEntity::new_empty();
        let qs = Wikidata::item2qs(&item).unwrap();
        assert_eq!(qs, vec!["CREATE"]);
    }

    #[test]
    fn test_item2qs_labels_descriptions_aliases() {
        let mut item = ItemEntity::new_empty();
        item.labels_mut().push(LocaleString::new("en", "Test item"));
        item.labels_mut()
            .push(LocaleString::new("de", "Testelement"));
        item.descriptions_mut()
            .push(LocaleString::new("en", "a test"));
        item.aliases_mut().push(LocaleString::new("en", "alias1"));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"CREATE".to_string()));
        assert!(qs.contains(&"LAST\tLen\t\"Test item\"".to_string()));
        assert!(qs.contains(&"LAST\tLde\t\"Testelement\"".to_string()));
        assert!(qs.contains(&"LAST\tDen\t\"a test\"".to_string()));
        assert!(qs.contains(&"LAST\tAen\t\"alias1\"".to_string()));
    }

    #[test]
    fn test_item2qs_sitelinks() {
        let mut item = ItemEntity::new_empty();
        item.sitelinks_mut()
            .get_or_insert_with(Vec::new)
            .push(SiteLink::new("enwiki", "Test item", vec![]));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tSenwiki\t\"Test item\"".to_string()));
    }

    #[test]
    fn test_item2qs_string_statement_no_qualifiers_no_refs() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello world"),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tP1476\t\"hello world\"".to_string()));
    }

    #[test]
    fn test_item2qs_item_statement() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_item("P31", "Q5"),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.contains(&"LAST\tP31\t\"Q5\"".to_string()));
    }

    #[test]
    fn test_item2qs_statement_with_qualifier() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![Snak::new_item("P407", "Q1860")],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Should have property, value, qualifier property, qualifier value
        assert!(qs
            .iter()
            .any(|s| s.contains("P1476") && s.contains("P407") && s.contains("Q1860")));
    }

    #[test]
    fn test_item2qs_statement_with_reference() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![Reference::new(vec![Snak::new_url(
                "P854",
                "http://example.com",
            )])],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Reference property becomes S854, prefixed with ! to start new ref section
        assert!(qs
            .iter()
            .any(|s| s.contains("!S854") && s.contains("http://example.com")));
    }

    #[test]
    fn test_item2qs_statement_with_multiple_references() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new_string("P1476", "hello"),
            vec![],
            vec![
                Reference::new(vec![Snak::new_url("P854", "http://first.com")]),
                Reference::new(vec![Snak::new_url("P854", "http://second.com")]),
            ],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        let line = qs.iter().find(|s| s.starts_with("LAST\tP1476")).unwrap();
        // Both references start with !S854
        assert_eq!(line.matches("!S854").count(), 2);
    }

    #[test]
    fn test_item2qs_skips_novalue_snak() {
        let mut item = ItemEntity::new_empty();
        // A novalue snak — snak2qs should return None, so this claim is skipped
        item.add_claim(Statement::new_normal(
            Snak::new(SnakDataType::WikibaseItem, "P31", SnakType::NoValue, None),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        // Only "CREATE" — the novalue statement must be skipped
        assert!(!qs.iter().any(|s| s.contains("P31")));
    }

    #[test]
    fn test_item2qs_quantity() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::Quantity,
                "P1082",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Quantity,
                    Value::Quantity(QuantityValue::new(42.0, None, "+42", None)),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs.iter().any(|s| s.contains("P1082") && s.contains("42")));
    }

    #[test]
    fn test_item2qs_monolingual() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::MonolingualText,
                "P1705",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::MonoLingualText,
                    Value::MonoLingual(MonoLingualText::new("Original title", "en")),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P1705") && s.contains("en:\"Original title\"")));
    }

    #[test]
    fn test_item2qs_coordinate() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::GlobeCoordinate,
                "P625",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::GlobeCoordinate,
                    Value::Coordinate(Coordinate::new(
                        None,
                        "http://www.wikidata.org/entity/Q2".to_string(),
                        51.5,
                        -0.1,
                        None,
                    )),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P625") && s.contains("@51.5/-0.1")));
    }

    #[test]
    fn test_item2qs_time_value() {
        let mut item = ItemEntity::new_empty();
        item.add_claim(Statement::new_normal(
            Snak::new(
                SnakDataType::Time,
                "P569",
                SnakType::Value,
                Some(DataValue::new(
                    DataValueType::Time,
                    Value::Time(TimeValue::new(
                        0,
                        0,
                        "http://www.wikidata.org/entity/Q1985727",
                        11,
                        "+1990-05-17T00:00:00Z",
                        0,
                    )),
                )),
            ),
            vec![],
            vec![],
        ));

        let qs = Wikidata::item2qs(&item).unwrap();
        assert!(qs
            .iter()
            .any(|s| s.contains("P569") && s.contains("+1990-05-17T00:00:00Z/11")));
    }

    #[test]
    fn test_set_user_agent() {
        let mut wd = Wikidata::new();
        wd.set_user_agent("my-agent/1.0");
        // Verify it builds without error (can't easily inspect the built client's UA)
        assert!(wd.reqwest_client().is_ok());
    }

    #[test]
    fn test_set_timeout() {
        let mut wd = Wikidata::new();
        wd.set_timeout(std::time::Duration::from_secs(30));
        assert!(wd.reqwest_client().is_ok());
    }
}
