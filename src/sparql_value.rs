/// Parses and stores values from a SPARQL JSON result.
use crate::lat_lon::LatLon;
use regex::Regex;
use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde::Deserializer;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum SparqlValue {
    Entity(String),
    File(String),
    Uri(String),
    Time(String),
    Location(LatLon),
    Literal(String),
    None,
}

impl SparqlValue {
    pub fn new_from_json(j: &Value) -> Option<Self> {
        lazy_static! {
            static ref RE_ENTITY: Regex = Regex::new(r#"^https{0,1}://[^/]+/entity/([A-Z]\d+)$"#)
                .expect("RE_ENTITY does not parse");
            static ref RE_FILE: Regex =
                Regex::new(r#"^https{0,1}://[^/]+/wiki/Special:FilePath/(.+?)$"#)
                    .expect("RE_FILE does not parse");
            static ref RE_POINT: Regex =
                Regex::new(r#"^Point\((-{0,1}\d+[\.0-9]*) (-{0,1}\d+[\.0-9]*)\)$"#)
                    .expect("RE_POINT does not parse");
            static ref RE_DATE: Regex = Regex::new(r#"^([+-]{0,1}\d+-\d{2}-\d{2})T00:00:00Z$"#)
                .expect("RE_DATE does not parse");
        }
        let value = match j["value"].as_str() {
            Some(v) => v,
            None => return None,
        };
        match j["type"].as_str() {
            Some("uri") => match RE_ENTITY.captures(value) {
                Some(caps) => caps
                    .get(1)
                    .map(|caps1| SparqlValue::Entity(caps1.as_str().to_string())),
                None => match RE_FILE.captures(value) {
                    Some(caps) => match caps.get(1) {
                        Some(caps1) => {
                            let file = caps1.as_str().to_string();
                            let file = urlencoding::decode(&file).ok()?;
                            let file = file.replace('_', " ");
                            Some(SparqlValue::File(file))
                        }
                        None => None,
                    },
                    None => Some(SparqlValue::Uri(value.to_string())),
                },
            },
            Some("literal") => match j["datatype"].as_str() {
                Some("http://www.opengis.net/ont/geosparql#wktLiteral") => {
                    match RE_POINT.captures(value) {
                        Some(caps) => {
                            let lat: f64 = caps.get(2)?.as_str().parse().ok()?;
                            let lon: f64 = caps.get(1)?.as_str().parse().ok()?;
                            Some(SparqlValue::Location(LatLon::new(lat, lon)))
                        }
                        None => None,
                    }
                }
                Some("http://www.w3.org/2001/XMLSchema#dateTime") => {
                    let time = value.to_string();
                    let time = match RE_DATE.captures(value) {
                        Some(caps) => {
                            let date: String = caps.get(1)?.as_str().to_string();
                            date
                        }
                        None => time,
                    };
                    Some(SparqlValue::Time(time))
                }
                _ => Some(SparqlValue::Literal(value.to_string())),
            },
            Some("bnode") => j["value"]
                .as_str()
                .map(|value| SparqlValue::Literal(value.to_string())),
            _ => None,
        }
    }
}

impl Serialize for SparqlValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            SparqlValue::Entity(entity) => {
                let mut s = serializer.serialize_struct("Entity", 2)?;
                s.serialize_field("type", "uri")?;
                s.serialize_field("value", &format!("http://www.wikidata.org/entity/{entity}"))?;
                s.end()
            }
            SparqlValue::File(file) => {
                let mut s = serializer.serialize_struct("File", 2)?;
                s.serialize_field("type", "uri")?;
                s.serialize_field(
                    "value",
                    &format!("http://commons.wikimedia.org/wiki/Special:FilePath/{file}"),
                )?;
                s.end()
            }
            SparqlValue::Uri(uri) => {
                let mut s = serializer.serialize_struct("Uri", 2)?;
                s.serialize_field("type", "uri")?;
                s.serialize_field("value", uri)?;
                s.end()
            }
            SparqlValue::Time(time) => {
                let mut s = serializer.serialize_struct("Time", 3)?;
                s.serialize_field("type", "literal")?;
                s.serialize_field("datatype", "http://www.w3.org/2001/XMLSchema#dateTime")?;
                s.serialize_field("value", time)?;
                s.end()
            }
            SparqlValue::Location(location) => {
                let mut s = serializer.serialize_struct("Location", 3)?;
                s.serialize_field("type", "literal")?;
                s.serialize_field(
                    "datatype",
                    "http://www.opengis.net/ont/geosparql#wktLiteral",
                )?;
                s.serialize_field(
                    "value",
                    &format!("Point({lon} {lat})", lat = location.lat, lon = location.lon),
                )?;
                s.end()
            }
            SparqlValue::Literal(literal) => {
                let mut s = serializer.serialize_struct("Literal", 2)?;
                s.serialize_field("type", "literal")?;
                s.serialize_field("value", literal)?;
                s.end()
            }
            SparqlValue::None => {
                let s = serializer.serialize_struct("None", 0)?;
                s.end()
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for SparqlValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let json: serde_json::value::Value = serde_json::value::Value::deserialize(deserializer)?;
        Self::new_from_json(&json)
            .ok_or_else(|| serde::de::Error::custom("Could not parse SparqlValue from JSON"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_from_json() {
        let json = r#"{"type":"literal","value":"GB-CAM"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, Some(SparqlValue::Literal("GB-CAM".to_string())));
    }

    #[test]
    fn test_deserialize() {
        let json = r#"{"type":"literal","value":"GB-CAM"}"#;
        let value: SparqlValue = serde_json::from_str(json).unwrap();
        assert_eq!(value, SparqlValue::Literal("GB-CAM".to_string()));
    }

    #[test]
    fn test_serialize_time() {
        let dt = "+2020-01-01T:01:23:45Z";
        let time = SparqlValue::Time(dt.to_string());
        let json = serde_json::to_string(&time).unwrap();
        assert_eq!(
            json,
            r#"{"type":"literal","datatype":"http://www.w3.org/2001/XMLSchema#dateTime","value":""#
                .to_string()
                + dt
                + r#""}"#
        );
        let time2: SparqlValue = serde_json::from_str(&json).unwrap();
        assert_eq!(time, time2);
        assert_eq!(time2, SparqlValue::Time(dt.to_string()));
    }

    #[test]
    fn test_serialize_location() {
        let location = LatLon::new(1.0, -2.0);
        let loc = SparqlValue::Location(location.to_owned());
        let json = serde_json::to_string(&loc).unwrap();
        assert_eq!(
            json,
            r#"{"type":"literal","datatype":"http://www.opengis.net/ont/geosparql#wktLiteral","value":"Point(-2 1)"}"#
        );
        let loc2: SparqlValue = serde_json::from_str(&json).unwrap();
        assert_eq!(loc, loc2);
        assert_eq!(loc2, SparqlValue::Location(location));
    }

    #[test]
    fn test_serialize_entity() {
        let entity = "Q1234";
        let ent = SparqlValue::Entity(entity.to_string());
        let json = serde_json::to_string(&ent).unwrap();
        assert_eq!(
            json,
            r#"{"type":"uri","value":"http://www.wikidata.org/entity/Q1234"}"#
        );
        let ent2: SparqlValue = serde_json::from_str(&json).unwrap();
        assert_eq!(ent, ent2);
        assert_eq!(ent2, SparqlValue::Entity(entity.to_string()));
    }

    #[test]
    fn test_serialize_file() {
        let file_str = "File:Example.jpg";
        let file = SparqlValue::File(file_str.to_string());
        let json = serde_json::to_string(&file).unwrap();
        assert_eq!(
            json,
            r#"{"type":"uri","value":"http://commons.wikimedia.org/wiki/Special:FilePath/File:Example.jpg"}"#
        );
        let file2: SparqlValue = serde_json::from_str(&json).unwrap();
        assert_eq!(file, file2);
        assert_eq!(file2, SparqlValue::File(file_str.to_string()));
    }

    #[test]
    fn test_serialize_uri() {
        let uri_str = "http://example.com";
        let uri = SparqlValue::Uri(uri_str.to_string());
        let json = serde_json::to_string(&uri).unwrap();
        assert_eq!(json, r#"{"type":"uri","value":"http://example.com"}"#);
        let uri2: SparqlValue = serde_json::from_str(&json).unwrap();
        assert_eq!(uri, uri2);
        assert_eq!(uri2, SparqlValue::Uri(uri_str.to_string()));
    }

    #[test]
    fn test_serialize_literal() {
        let literal = "example";
        let lit = SparqlValue::Literal(literal.to_string());
        let json = serde_json::to_string(&lit).unwrap();
        assert_eq!(json, r#"{"type":"literal","value":"example"}"#);
        let lit2: SparqlValue = serde_json::from_str(&json).unwrap();
        assert_eq!(lit, lit2);
        assert_eq!(lit2, SparqlValue::Literal(literal.to_string()));
    }
}
