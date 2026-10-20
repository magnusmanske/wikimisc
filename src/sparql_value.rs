/// Parses and stores values from a SPARQL JSON result.
use crate::entity_id::EntityId;
use crate::lat_lon::LatLon;
use regex::Regex;
use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde::Deserializer;
use serde_json::Value;
use std::sync::LazyLock;

// The patterns below are literals and cannot fail to compile in practice. They
// are still stored as `Option<Regex>` so that a pattern which somehow did fail
// degrades to "never matches" instead of panicking in a library: a URI then
// falls through to `SparqlValue::Uri`, and a typed literal keeps its raw value.
// `test_all_static_regexes_compile` makes sure CI catches a broken literal.
static RE_ENTITY: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"^https{0,1}://[^/]+/entity/([A-Z]\d+)$"#).ok());
static RE_FILE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"^https{0,1}://[^/]+/wiki/Special:FilePath/(.+?)$"#).ok());
// Midnight UTC is how both engines encode a date of day precision or coarser,
// so the time part is dropped. QLever omits the `Z` on years outside xsd's
// four-digit range (the Big Bang is `-13798000000-01-01T00:00:00`), so those
// match without it; a four-digit year without a time zone is a local time
// rather than a UTC date and is left alone.
static RE_DATE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"^(?:([+-]?\d+-\d{2}-\d{2})T00:00:00Z|([+-]?\d{5,}-\d{2}-\d{2})T00:00:00)$"#).ok()
});

const WKT_LITERAL: &str = "http://www.opengis.net/ont/geosparql#wktLiteral";
const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";

/// Captures from one of the statics above, treating an unavailable regex as a
/// plain non-match.
fn captures<'t>(re: &Option<Regex>, text: &'t str) -> Option<regex::Captures<'t>> {
    re.as_ref()?.captures(text)
}

#[derive(Debug, Clone, PartialEq)]
pub enum SparqlValue {
    Entity(String),
    File(String),
    Uri(String),
    Time(String),
    Location(LatLon),
    Literal(String),
}

impl SparqlValue {
    /// The entity, for an [`SparqlValue::Entity`] with a valid ID.
    pub fn entity_id(&self) -> Option<EntityId> {
        match self {
            SparqlValue::Entity(id) => id.parse().ok(),
            _ => None,
        }
    }

    /// Parses one binding of a SPARQL JSON result. The lexical forms of WDQS
    /// (Blazegraph) and QLever differ in places, and both are understood.
    pub fn new_from_json(j: &Value) -> Option<Self> {
        let value = j["value"].as_str()?;
        match j["type"].as_str() {
            Some("uri") => Self::from_uri(value),
            // `typed-literal` is the pre-recommendation spelling some
            // endpoints still emit for a literal with a datatype.
            Some("literal") | Some("typed-literal") => {
                Some(Self::from_literal(value, j["datatype"].as_str()))
            }
            Some("bnode") => Some(SparqlValue::Literal(value.to_string())),
            _ => None,
        }
    }

    fn from_uri(value: &str) -> Option<Self> {
        if let Some(caps) = captures(&RE_ENTITY, value) {
            return caps
                .get(1)
                .map(|entity| SparqlValue::Entity(entity.as_str().to_string()));
        }
        // A file name whose percent-encoding is not UTF-8 cannot be decoded,
        // so it stays a plain URI rather than failing the whole result.
        let file = captures(&RE_FILE, value)
            .and_then(|caps| urlencoding::decode(caps.get(1)?.as_str()).ok())
            .map(|file| SparqlValue::File(file.replace('_', " ")));
        Some(file.unwrap_or_else(|| SparqlValue::Uri(value.to_string())))
    }

    fn from_literal(value: &str, datatype: Option<&str>) -> Self {
        match datatype {
            // Geometries other than a point on Earth have no typed
            // representation, so they keep their WKT text.
            // `LatLon::from_wkt` reads Blazegraph's `Point(…)` and QLever's
            // `POINT(…)`, and rejects a point on another globe: a `LatLon` has
            // none, so it would read as a place on Earth.
            Some(WKT_LITERAL) => LatLon::from_wkt(value)
                .map(SparqlValue::Location)
                .unwrap_or_else(|| SparqlValue::Literal(value.to_string())),
            Some(XSD_DATE_TIME) => {
                let date = captures(&RE_DATE, value)
                    .and_then(|caps| caps.get(1).or_else(|| caps.get(2)))
                    .map_or(value, |date| date.as_str());
                SparqlValue::Time(date.to_string())
            }
            _ => SparqlValue::Literal(value.to_string()),
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
                s.serialize_field("value", &crate::sparql_util::commons_media_iri(file))?;
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
                s.serialize_field("datatype", XSD_DATE_TIME)?;
                s.serialize_field("value", time)?;
                s.end()
            }
            SparqlValue::Location(location) => {
                let mut s = serializer.serialize_struct("Location", 3)?;
                s.serialize_field("type", "literal")?;
                s.serialize_field("datatype", WKT_LITERAL)?;
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
            r#"{"type":"uri","value":"http://commons.wikimedia.org/wiki/Special:FilePath/File%3AExample.jpg"}"#
        );
        let file2: SparqlValue = serde_json::from_str(&json).unwrap();
        assert_eq!(file, file2);
        assert_eq!(file2, SparqlValue::File(file_str.to_string()));

        // A name with spaces becomes a valid IRI, as WDQS writes it.
        let file = SparqlValue::File("My File (1).jpg".to_string());
        let json = serde_json::to_string(&file).unwrap();
        assert!(
            json.contains("Special:FilePath/My%20File%20%281%29.jpg"),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<SparqlValue>(&json).unwrap(), file);
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

    #[test]
    fn test_serialize_deserialize() {
        let value = SparqlValue::Literal("GB-CAM".to_string());
        let json = serde_json::to_string(&value).unwrap();
        let value2: SparqlValue = serde_json::from_str(&json).unwrap();
        assert_eq!(value, value2);
    }

    #[test]
    fn test_serialize_deserialize_all() {
        let values = vec![
            SparqlValue::Literal("GB-CAM".to_string()),
            SparqlValue::Entity("Q1234".to_string()),
            SparqlValue::File("File:Example.jpg".to_string()),
            SparqlValue::Uri("http://example.com".to_string()),
            SparqlValue::Time("+2020-01-01T:01:23:45Z".to_string()),
            SparqlValue::Location(LatLon::new(1.0, -2.0)),
        ];
        for value in values {
            let json = serde_json::to_string(&value).unwrap();
            let value2: SparqlValue = serde_json::from_str(&json).unwrap();
            assert_eq!(value, value2);
        }
    }

    #[test]
    fn test_new_from_json_entity() {
        let json = r#"{"type":"uri","value":"http://www.wikidata.org/entity/Q21"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, Some(SparqlValue::Entity("Q21".to_string())));
    }

    #[test]
    fn test_new_from_json_entity_https() {
        let json = r#"{"type":"uri","value":"https://www.wikidata.org/entity/P123"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, Some(SparqlValue::Entity("P123".to_string())));
    }

    #[test]
    fn test_new_from_json_file() {
        let json = r#"{"type":"uri","value":"http://commons.wikimedia.org/wiki/Special:FilePath/Example.jpg"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, Some(SparqlValue::File("Example.jpg".to_string())));
    }

    #[test]
    fn test_new_from_json_file_with_spaces() {
        let json = r#"{"type":"uri","value":"http://commons.wikimedia.org/wiki/Special:FilePath/My_Example_File.jpg"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(
            value,
            Some(SparqlValue::File("My Example File.jpg".to_string()))
        );
    }

    #[test]
    fn test_new_from_json_file_url_encoded() {
        let json = r#"{"type":"uri","value":"http://commons.wikimedia.org/wiki/Special:FilePath/My%20File.jpg"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, Some(SparqlValue::File("My File.jpg".to_string())));
    }

    #[test]
    fn test_new_from_json_file_with_non_utf8_encoding_stays_uri() {
        // `%E9` is Latin-1 for "é"; it does not decode as UTF-8.
        let uri = "http://commons.wikimedia.org/wiki/Special:FilePath/Caf%E9.jpg";
        let json = serde_json::json!({"type": "uri", "value": uri});
        let value = SparqlValue::new_from_json(&json);
        assert_eq!(value, Some(SparqlValue::Uri(uri.to_string())));
    }

    #[test]
    fn test_new_from_json_generic_uri() {
        let json = r#"{"type":"uri","value":"http://example.com/resource"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(
            value,
            Some(SparqlValue::Uri("http://example.com/resource".to_string()))
        );
    }

    #[test]
    fn test_new_from_json_location() {
        let json = r#"{"type":"literal","datatype":"http://www.opengis.net/ont/geosparql#wktLiteral","value":"Point(-122.5 37.5)"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(
            value,
            Some(SparqlValue::Location(LatLon::new(37.5, -122.5)))
        );
    }

    #[test]
    fn test_new_from_json_datetime() {
        let json = r#"{"type":"literal","datatype":"http://www.w3.org/2001/XMLSchema#dateTime","value":"2020-01-15T00:00:00Z"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, Some(SparqlValue::Time("2020-01-15".to_string())));
    }

    #[test]
    fn test_new_from_json_datetime_full() {
        let json = r#"{"type":"literal","datatype":"http://www.w3.org/2001/XMLSchema#dateTime","value":"2020-01-15T12:30:45Z"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(
            value,
            Some(SparqlValue::Time("2020-01-15T12:30:45Z".to_string()))
        );
    }

    #[test]
    fn test_new_from_json_bnode() {
        let json = r#"{"type":"bnode","value":"b0"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, Some(SparqlValue::Literal("b0".to_string())));
    }

    #[test]
    fn test_new_from_json_invalid() {
        let json = r#"{"type":"unknown","value":"test"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, None);
    }

    #[test]
    fn test_new_from_json_missing_value() {
        let json = r#"{"type":"literal"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, None);
    }

    fn wkt(value: &str) -> Option<SparqlValue> {
        SparqlValue::new_from_json(&serde_json::json!({
            "type": "literal",
            "datatype": WKT_LITERAL,
            "value": value,
        }))
    }

    fn date_time(value: &str) -> Option<SparqlValue> {
        SparqlValue::new_from_json(&serde_json::json!({
            "type": "literal",
            "datatype": XSD_DATE_TIME,
            "value": value,
        }))
    }

    #[test]
    fn test_new_from_json_wkt_literal_that_is_not_a_point() {
        // Only a point is understood. Any other WKT geometry keeps its text
        // rather than becoming a bogus location — or failing the whole result,
        // which is what a `None` here does to `SparqlApiResult`.
        let polygon = "POLYGON((0 0, 1 0, 1 1, 0 0))";
        assert_eq!(wkt(polygon), Some(SparqlValue::Literal(polygon.into())));
    }

    #[test]
    fn test_new_from_json_location_qlever_spelling() {
        // QLever upper-cases the WKT keyword and prints fixed decimals.
        assert_eq!(
            wkt("POINT(13.383333 52.516667)"),
            Some(SparqlValue::Location(LatLon::new(52.516667, 13.383333)))
        );
    }

    #[test]
    fn test_new_from_json_location_lenient_lexical_forms() {
        assert_eq!(
            wkt("Point( -0.5   1e-3 )"),
            Some(SparqlValue::Location(LatLon::new(0.001, -0.5)))
        );
        assert_eq!(
            wkt("point(1.0E2 -45.)"),
            Some(SparqlValue::Location(LatLon::new(-45.0, 100.0)))
        );
    }

    #[test]
    fn test_new_from_json_location_explicit_earth_globe() {
        assert_eq!(
            wkt("<http://www.wikidata.org/entity/Q2> Point(1 2)"),
            Some(SparqlValue::Location(LatLon::new(2.0, 1.0)))
        );
    }

    #[test]
    fn test_new_from_json_location_on_another_globe_stays_literal() {
        // Both engines emit this for a coordinate on the Moon. A `LatLon` would
        // silently place it on Earth.
        let moon = "<http://www.wikidata.org/entity/Q405> Point(-60.44 -49.53)";
        assert_eq!(wkt(moon), Some(SparqlValue::Literal(moon.into())));
    }

    #[test]
    fn test_new_from_json_large_year_without_time_zone() {
        // QLever drops the `Z` for years outside xsd's four-digit range; WDQS
        // keeps it. Both must come out the same.
        let expected = Some(SparqlValue::Time("-13798000000-01-01".into()));
        assert_eq!(date_time("-13798000000-01-01T00:00:00"), expected);
        assert_eq!(date_time("-13798000000-01-01T00:00:00Z"), expected);
    }

    #[test]
    fn test_new_from_json_bce_date() {
        assert_eq!(
            date_time("-0099-07-01T00:00:00Z"),
            Some(SparqlValue::Time("-0099-07-01".into()))
        );
    }

    #[test]
    fn test_new_from_json_local_midnight_is_not_a_date() {
        // Without a time zone this is a local time, not a UTC date.
        assert_eq!(
            date_time("2020-01-15T00:00:00"),
            Some(SparqlValue::Time("2020-01-15T00:00:00".into()))
        );
    }

    #[test]
    fn test_new_from_json_typed_literal() {
        let json = serde_json::json!({
            "type": "typed-literal",
            "datatype": XSD_DATE_TIME,
            "value": "2020-01-15T00:00:00Z",
        });
        assert_eq!(
            SparqlValue::new_from_json(&json),
            Some(SparqlValue::Time("2020-01-15".into()))
        );
    }

    #[test]
    fn test_new_from_json_other_datatypes_are_literals() {
        // The lexical forms differ between engines (QLever writes `xsd:int`
        // and `3700577.0` where WDQS writes `xsd:integer` and `3700577`); the
        // value is passed through unchanged either way.
        for (datatype, value) in [
            ("http://www.w3.org/2001/XMLSchema#decimal", "3700577.0"),
            ("http://www.w3.org/2001/XMLSchema#int", "42"),
            ("http://www.w3.org/2001/XMLSchema#boolean", "true"),
            ("http://www.w3.org/2001/XMLSchema#date", "2020-01-02"),
        ] {
            let json = serde_json::json!({"type": "literal", "datatype": datatype, "value": value});
            assert_eq!(
                SparqlValue::new_from_json(&json),
                Some(SparqlValue::Literal(value.into())),
                "{datatype}"
            );
        }
    }

    #[test]
    fn test_new_from_json_language_literal() {
        let json = r#"{"type":"literal","value":"Berlin","xml:lang":"de"}"#;
        let value = SparqlValue::new_from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(value, Some(SparqlValue::Literal("Berlin".into())));
    }

    #[test]
    fn test_all_static_regexes_compile() {
        // The statics degrade to "never matches" rather than panicking, so
        // assert here that they are in fact available.
        assert!(RE_ENTITY.is_some());
        assert!(RE_FILE.is_some());
        assert!(RE_DATE.is_some());
    }

    #[test]
    fn test_entity_id() {
        let value = SparqlValue::Entity("Q42".to_string());
        assert_eq!(value.entity_id(), "Q42".parse().ok());
        assert_eq!(
            SparqlValue::Entity("P31".to_string())
                .entity_id()
                .map(|id| id.kind()),
            Some(crate::entity_id::EntityKind::Property)
        );
        assert_eq!(SparqlValue::Entity("Q042".to_string()).entity_id(), None);
        assert_eq!(SparqlValue::Literal("Q42".to_string()).entity_id(), None);
    }
}
