//! Builders for the `wikibase` snaks, references and statements tools add to
//! Wikidata, taking typed IDs so that no caller formats `"P31"` by hand.
//!
//! The `wikibase` crate has constructors of its own (`Snak::new_item`, …);
//! these fill its gaps:
//!
//! - times keep their calendar model ([`time_snak`]; `Snak::new_time` is
//!   always Gregorian) and are built from a [`Date`];
//! - coordinates get a precision ([`coordinate_snak`]); `Snak::new_coordinate`
//!   leaves it out, and `wbeditentity` rejects a coordinate without one;
//! - quantities can carry a unit ([`quantity_snak`]);
//! - monolingual text keeps language and text apart ([`monolingual_snak`]);
//! - the usual references: "stated in" with an identifier and "retrieved"
//!   ([`stated_in_reference`]), a reference URL ([`reference_url`]), and
//!   "imported from" a wiki revision ([`imported_from_revision`]).
//!
//! [`date_from_time_value`] goes the other way, from a `wikibase` time value
//! to a [`Date`].

use crate::date::{Calendar, Date, DateError};
use crate::entity_id::{ItemId, PropertyId};
use wikibase::{
    Coordinate, DataValue, DataValueType, EntityType, EntityValue, MonoLingualText, QuantityValue,
    Reference, Snak, SnakDataType, SnakType, Statement, TimeValue, Value,
};

/// stated in
pub const STATED_IN: PropertyId = PropertyId::new(248);
/// retrieved
pub const RETRIEVED: PropertyId = PropertyId::new(813);
/// reference URL
pub const REFERENCE_URL: PropertyId = PropertyId::new(854);
/// imported from Wikimedia project
pub const IMPORTED_FROM: PropertyId = PropertyId::new(143);
/// Wikimedia import URL
pub const WIKIMEDIA_IMPORT_URL: PropertyId = PropertyId::new(4656);

/// The globe of coordinates on Earth.
pub const GLOBE_EARTH: &str = "http://www.wikidata.org/entity/Q2";

/// The precision [`coordinate_snak`] uses when none is given: one arcsecond.
pub const DEFAULT_COORDINATE_PRECISION: f64 = 1.0 / 3600.0;

/// The unit of a quantity without one.
const UNITLESS: &str = "1";

fn value_snak(
    property: PropertyId,
    datatype: SnakDataType,
    kind: DataValueType,
    value: Value,
) -> Snak {
    Snak::new(
        datatype,
        property.to_string(),
        SnakType::Value,
        Some(DataValue::new(kind, value)),
    )
}

/// An item value: `P31 = Q5`.
pub fn item_snak(property: PropertyId, item: ItemId) -> Snak {
    value_snak(
        property,
        SnakDataType::WikibaseItem,
        DataValueType::EntityId,
        Value::Entity(EntityValue::new(EntityType::Item, item.to_string())),
    )
}

/// A property value: `P1659 = P570` (see also).
pub fn property_snak(property: PropertyId, value: PropertyId) -> Snak {
    value_snak(
        property,
        SnakDataType::WikibaseProperty,
        DataValueType::EntityId,
        Value::Entity(EntityValue::new(EntityType::Property, value.to_string())),
    )
}

/// A string value.
pub fn string_snak(property: PropertyId, value: impl Into<String>) -> Snak {
    value_snak(
        property,
        SnakDataType::String,
        DataValueType::StringType,
        Value::StringValue(value.into()),
    )
}

/// An external identifier.
pub fn external_id_snak(property: PropertyId, id: impl Into<String>) -> Snak {
    value_snak(
        property,
        SnakDataType::ExternalId,
        DataValueType::StringType,
        Value::StringValue(id.into()),
    )
}

/// A URL.
pub fn url_snak(property: PropertyId, url: impl Into<String>) -> Snak {
    value_snak(
        property,
        SnakDataType::Url,
        DataValueType::StringType,
        Value::StringValue(url.into()),
    )
}

/// A Commons file name (without `File:`).
pub fn commons_media_snak(property: PropertyId, file: impl Into<String>) -> Snak {
    value_snak(
        property,
        SnakDataType::CommonsMedia,
        DataValueType::StringType,
        Value::StringValue(file.into()),
    )
}

/// Text in a language: `P1476 = "Titel"@de`.
pub fn monolingual_snak(
    property: PropertyId,
    language: impl Into<String>,
    text: impl Into<String>,
) -> Snak {
    value_snak(
        property,
        SnakDataType::MonolingualText,
        DataValueType::MonoLingualText,
        Value::MonoLingual(MonoLingualText::new(text.into(), language.into())),
    )
}

/// A point in time, with the date's precision and calendar model.
pub fn time_snak(property: PropertyId, date: &Date) -> Snak {
    value_snak(
        property,
        SnakDataType::Time,
        DataValueType::Time,
        Value::Time(TimeValue::new(
            0,
            0,
            date.calendar().uri(),
            date.precision(),
            date.time(),
            0,
        )),
    )
}

/// A coordinate on Earth. Without a `precision`,
/// [`DEFAULT_COORDINATE_PRECISION`] is used: `wbeditentity` rejects a
/// coordinate that has none.
pub fn coordinate_snak(
    property: PropertyId,
    latitude: f64,
    longitude: f64,
    precision: Option<f64>,
) -> Snak {
    value_snak(
        property,
        SnakDataType::GlobeCoordinate,
        DataValueType::GlobeCoordinate,
        Value::Coordinate(Coordinate::new(
            None,
            GLOBE_EARTH.to_string(),
            latitude,
            longitude,
            Some(precision.unwrap_or(DEFAULT_COORDINATE_PRECISION)),
        )),
    )
}

/// A quantity, in the unit with the given item (`Some(Q11573)`, metre) or
/// without a unit.
pub fn quantity_snak(property: PropertyId, amount: f64, unit: Option<ItemId>) -> Snak {
    let unit = unit.map_or_else(|| UNITLESS.to_string(), ItemId::uri);
    value_snak(
        property,
        SnakDataType::Quantity,
        DataValueType::Quantity,
        Value::Quantity(QuantityValue::new(amount, None, unit, None)),
    )
}

/// "retrieved" (P813) today, day precision, Gregorian.
pub fn retrieved_today() -> Snak {
    time_snak(RETRIEVED, &Date::today())
}

/// "stated in" (P248) the given item.
pub fn stated_in(item: ItemId) -> Snak {
    item_snak(STATED_IN, item)
}

/// The usual reference for a value taken from a database: "stated in" the
/// database's item, the record's identifier (if given) and "retrieved" today
/// (if asked for), in that order.
pub fn stated_in_reference(
    stated_in: ItemId,
    id: Option<(PropertyId, &str)>,
    retrieved: bool,
) -> Reference {
    let mut snaks = vec![self::stated_in(stated_in)];
    if let Some((property, id)) = id {
        snaks.push(external_id_snak(property, id));
    }
    if retrieved {
        snaks.push(retrieved_today());
    }
    Reference::new(snaks)
}

/// A "reference URL" (P854) reference, with "retrieved" today if asked for.
pub fn reference_url(url: impl Into<String>, retrieved: bool) -> Reference {
    let mut snaks = vec![url_snak(REFERENCE_URL, url)];
    if retrieved {
        snaks.push(retrieved_today());
    }
    Reference::new(snaks)
}

/// A reference to the wiki page revision a value was harvested from:
/// "imported from Wikimedia project" (P143) the wiki's item, if known, and the
/// revision's permalink as "Wikimedia import URL" (P4656).
///
/// `server` is the wiki's host (`en.wikipedia.org`) or base URL
/// (`https://en.wikipedia.org`).
pub fn imported_from_revision(
    wiki: Option<ItemId>,
    server: &str,
    title: &str,
    revision: u64,
) -> Reference {
    let mut snaks: Vec<Snak> = wiki
        .map(|wiki| item_snak(IMPORTED_FROM, wiki))
        .into_iter()
        .collect();
    snaks.push(url_snak(
        WIKIMEDIA_IMPORT_URL,
        permalink(server, title, revision),
    ));
    Reference::new(snaks)
}

/// A statement of normal rank, without qualifiers.
pub fn statement(main_snak: Snak, references: Vec<Reference>) -> Statement {
    Statement::new_normal(main_snak, vec![], references)
}

/// The [`Date`] of a `wikibase` time value, in its calendar model (an
/// unknown model counts as Gregorian). See [`Date::from_wikibase`].
pub fn date_from_time_value(time: &TimeValue) -> Result<Date, DateError> {
    let calendar = Calendar::from_uri(time.calendarmodel()).unwrap_or_default();
    Date::from_wikibase(time.time(), *time.precision()).map(|date| date.with_calendar(calendar))
}

/// `https://{host}/w/index.php?title={title}&oldid={revision}`, the title in
/// database form and percent-encoded.
fn permalink(server: &str, title: &str, revision: u64) -> String {
    let server = server.trim_end_matches('/');
    let base = if server.contains("://") {
        server.to_string()
    } else {
        format!("https://{server}")
    };
    let title = encode_component(&title.replace(' ', "_"));
    format!("{base}/w/index.php?title={title}&oldid={revision}")
}

/// RFC 3986 percent-encoding: everything but `A–Z a–z 0–9 - _ . ~`.
fn encode_component(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len());
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn json_of(snak: &Snak) -> serde_json::Value {
        serde_json::to_value(snak).unwrap()
    }

    #[test]
    fn simple_snaks() {
        let p = PropertyId::new(31);
        let snak = item_snak(p, ItemId::new(5));
        assert_eq!(snak.property(), "P31");
        assert_eq!(*snak.datatype(), SnakDataType::WikibaseItem);
        assert_eq!(json_of(&snak)["datavalue"]["value"]["id"], "Q5");

        let snak = property_snak(PropertyId::new(1659), PropertyId::new(570));
        assert_eq!(json_of(&snak)["datavalue"]["value"]["id"], "P570");
        assert_eq!(json_of(&snak)["datatype"], "wikibase-property");

        assert_eq!(json_of(&string_snak(p, "x"))["datavalue"]["value"], "x");
        assert_eq!(
            json_of(&external_id_snak(p, "x"))["datatype"],
            "external-id"
        );
        assert_eq!(json_of(&url_snak(p, "https://x"))["datatype"], "url");
        assert_eq!(
            json_of(&commons_media_snak(p, "A.jpg"))["datatype"],
            "commonsMedia"
        );
    }

    #[test]
    fn monolingual_keeps_language_and_text_apart() {
        let snak = monolingual_snak(PropertyId::new(1476), "de", "Titel");
        let value = &json_of(&snak)["datavalue"]["value"];
        assert_eq!(value["language"], "de");
        assert_eq!(value["text"], "Titel");
    }

    #[test]
    fn time_snak_keeps_calendar_and_precision() {
        let date = Date::new(1582, Some(10), Some(4))
            .unwrap()
            .with_calendar(Calendar::Julian);
        let snak = time_snak(PropertyId::new(569), &date);
        let value = &json_of(&snak)["datavalue"]["value"];
        assert_eq!(value["time"], "+1582-10-04T00:00:00Z");
        assert_eq!(value["precision"], 11);
        assert_eq!(value["calendarmodel"], crate::date::JULIAN_URI);
    }

    #[test]
    fn coordinates_always_have_a_precision() {
        let snak = coordinate_snak(PropertyId::new(625), 52.5, 13.4, None);
        let value = &json_of(&snak)["datavalue"]["value"];
        assert_eq!(value["latitude"], 52.5);
        assert_eq!(value["longitude"], 13.4);
        assert_eq!(value["precision"], json!(DEFAULT_COORDINATE_PRECISION));
        assert_eq!(value["globe"], GLOBE_EARTH);
        let snak = coordinate_snak(PropertyId::new(625), 52.5, 13.4, Some(0.001));
        assert_eq!(json_of(&snak)["datavalue"]["value"]["precision"], 0.001);
    }

    #[test]
    fn quantities() {
        let snak = quantity_snak(PropertyId::new(2048), 1.85, Some(ItemId::new(11573)));
        let value = &json_of(&snak)["datavalue"]["value"];
        assert_eq!(value["unit"], "http://www.wikidata.org/entity/Q11573");
        let snak = quantity_snak(PropertyId::new(1082), 42.0, None);
        assert_eq!(json_of(&snak)["datavalue"]["value"]["unit"], "1");
    }

    #[test]
    fn stated_in_reference_matches_the_usual_shape() {
        let reference = stated_in_reference(
            ItemId::new(423048),
            Some((PropertyId::new(213), "0000000121251077")),
            true,
        );
        let properties: Vec<&str> = reference.snaks().iter().map(|s| s.property()).collect();
        assert_eq!(properties, ["P248", "P213", "P813"]);
        let retrieved = json_of(&reference.snaks()[2]);
        assert_eq!(
            retrieved["datavalue"]["value"]["time"],
            chrono::Utc::now().format("+%Y-%m-%dT00:00:00Z").to_string()
        );
        assert_eq!(retrieved["datavalue"]["value"]["precision"], 11);
        assert_eq!(
            retrieved["datavalue"]["value"]["calendarmodel"],
            crate::date::GREGORIAN_URI
        );

        let bare = stated_in_reference(ItemId::new(1), None, false);
        assert_eq!(bare.snaks().len(), 1);
    }

    #[test]
    fn reference_urls() {
        let reference = reference_url("https://example.org", true);
        let properties: Vec<&str> = reference.snaks().iter().map(|s| s.property()).collect();
        assert_eq!(properties, ["P854", "P813"]);
        assert_eq!(reference_url("https://example.org", false).snaks().len(), 1);
    }

    #[test]
    fn imported_from_revision_reference() {
        let reference = imported_from_revision(
            Some(ItemId::new(328)),
            "en.wikipedia.org",
            "The Shawshank Redemption",
            123,
        );
        let snaks = reference.snaks();
        assert_eq!(snaks[0].property(), "P143");
        assert_eq!(json_of(&snaks[0])["datavalue"]["value"]["id"], "Q328");
        assert_eq!(
            json_of(&snaks[1])["datavalue"]["value"],
            "https://en.wikipedia.org/w/index.php?title=The_Shawshank_Redemption&oldid=123"
        );
        let reference =
            imported_from_revision(None, "https://de.wikipedia.org/", "Köln (Stadt)", 7);
        assert_eq!(reference.snaks().len(), 1);
        assert_eq!(
            json_of(&reference.snaks()[0])["datavalue"]["value"],
            "https://de.wikipedia.org/w/index.php?title=K%C3%B6ln_%28Stadt%29&oldid=7"
        );
    }

    #[test]
    fn statements() {
        let statement = statement(
            item_snak(PropertyId::new(31), ItemId::new(5)),
            vec![reference_url("https://example.org", false)],
        );
        assert_eq!(statement.property(), "P31");
        assert!(statement.qualifiers().is_empty());
        assert_eq!(statement.references().len(), 1);
    }

    #[test]
    fn dates_from_time_values() {
        let snak = time_snak(
            PropertyId::new(569),
            &Date::new(1582, Some(10), Some(4))
                .unwrap()
                .with_calendar(Calendar::Julian),
        );
        let Some(Value::Time(time)) = snak.data_value().as_ref().map(|dv| dv.value().clone())
        else {
            panic!("not a time");
        };
        let date = date_from_time_value(&time).unwrap();
        assert_eq!(date.calendar(), Calendar::Julian);
        assert_eq!(
            (date.year(), date.month(), date.day()),
            (1582, Some(10), Some(4))
        );
    }
}
