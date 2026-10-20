//! Stores latitude/longitude coordinates, and computes with them: distances,
//! bounding boxes, WKT points.

use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

/// Failure modes of [`LatLon::from_str`] and [`LatLon::try_new`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LatLonError {
    /// The latitude is absent or not a number.
    #[error("cannot parse latitude from '{0}'")]
    Latitude(String),

    /// The longitude is absent or not a number.
    #[error("cannot parse longitude from '{0}'")]
    Longitude(String),

    /// The latitude is outside ±90°, the longitude outside ±180°, or one is
    /// not finite.
    #[error("coordinates out of range: {lat}, {lon}")]
    OutOfRange { lat: f64, lon: f64 },
}

/// Mean radius of the Earth, as the haversine distances here use it.
pub const EARTH_RADIUS_KM: f64 = 6371.0;

/// The globe IRI Wikidata puts before a WKT point on Earth, when it spells it
/// out at all.
const EARTH_IRI_SUFFIX: &str = "://www.wikidata.org/entity/Q2>";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    /// A coordinate pair, unchecked; see [`Self::try_new`].
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }

    /// A coordinate pair, if both are finite and within ±90° latitude and
    /// ±180° longitude.
    pub fn try_new(lat: f64, lon: f64) -> Result<Self, LatLonError> {
        let valid = lat.is_finite()
            && lon.is_finite()
            && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lon);
        if valid {
            Ok(Self { lat, lon })
        } else {
            Err(LatLonError::OutOfRange { lat, lon })
        }
    }

    /// Reads a WKT point as SPARQL endpoints return it: `Point(lon lat)` —
    /// longitude first —, `POINT(…)` or any other case, with Wikidata's
    /// optional `<http://www.wikidata.org/entity/Q2>` (Earth) in front. A
    /// point on another globe is not read: a `LatLon` would put it on Earth.
    pub fn from_wkt(wkt: &str) -> Option<Self> {
        let point = match wkt.strip_prefix('<') {
            Some(iri) => {
                let end = iri.find('>')?;
                let (iri, rest) = iri.split_at(end + 1);
                let earth = iri
                    .strip_prefix("http")
                    .map(|iri| iri.strip_prefix('s').unwrap_or(iri))
                    .is_some_and(|iri| iri.eq_ignore_ascii_case(EARTH_IRI_SUFFIX));
                if !earth {
                    return None;
                }
                rest.trim_start()
            }
            None => wkt,
        };
        let keyword = point.get(..5)?;
        if !keyword.eq_ignore_ascii_case("point") {
            return None;
        }
        let inner = point
            .get(5..)?
            .trim_start()
            .strip_prefix('(')?
            .strip_suffix(')')?;
        let mut numbers = inner.split_whitespace();
        let (lon, lat) = (numbers.next()?, numbers.next()?);
        if numbers.next().is_some() || !is_wkt_number(lon) || !is_wkt_number(lat) {
            return None;
        }
        Some(Self {
            lat: lat.parse().ok()?,
            lon: lon.parse().ok()?,
        })
    }

    /// The point as WKT, `Point(lon lat)`.
    pub fn to_wkt(&self) -> String {
        format!("Point({} {})", self.lon, self.lat)
    }

    /// The great-circle (haversine) distance to `other`, in kilometres.
    pub fn distance_km(&self, other: &LatLon) -> f64 {
        let (lat1, lat2) = (self.lat.to_radians(), other.lat.to_radians());
        let dlat = (other.lat - self.lat).to_radians();
        let dlon = (other.lon - self.lon).to_radians();
        let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        2.0 * EARTH_RADIUS_KM * h.sqrt().asin()
    }

    /// The great-circle (haversine) distance to `other`, in metres.
    pub fn distance_m(&self, other: &LatLon) -> f64 {
        self.distance_km(other) * 1000.0
    }
}

/// A number as the WKT points of SPARQL endpoints write them: optional `-`,
/// digits, optional fraction, optional exponent (`5.25125E1`).
fn is_wkt_number(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (mantissa, exponent) = match s.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (s, None),
    };
    let (int, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    !int.is_empty() && digits(int) && digits(fraction) && exponent_ok
}

impl FromStr for LatLon {
    type Err = LatLonError;

    /// Reads `lat,lon` or `lat/lon`, optionally prefixed with `@` (as in map
    /// URLs), with optional whitespace around the parts.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // A missing component and an unparseable one are the same failure as far
        // as a caller is concerned, so both map to the per-component variant.
        let latitude = || LatLonError::Latitude(s.to_string());
        let longitude = || LatLonError::Longitude(s.to_string());

        let trimmed = s.trim();
        let trimmed = trimmed.strip_prefix('@').unwrap_or(trimmed);
        let separator = trimmed.find([',', '/']).ok_or_else(longitude)?;
        let (lat, rest) = trimmed.split_at(separator);
        let rest = rest.get(1..).unwrap_or("");
        let lat = lat.trim().parse::<f64>().map_err(|_| latitude())?;
        let lon = rest
            .split([',', '/'])
            .next()
            .ok_or_else(longitude)?
            .trim()
            .parse::<f64>()
            .map_err(|_| longitude())?;
        Ok(Self { lat, lon })
    }
}

/// Parses a distance such as `500m`, `5km`, `5 km` or `1.5KM` into metres; a
/// bare number is metres. Negative and non-finite distances are rejected.
pub fn parse_distance_m(s: &str) -> Option<f64> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let (number, factor) = if let Some(number) = lower.strip_suffix("km") {
        (number, 1000.0)
    } else if let Some(number) = lower.strip_suffix('m') {
        (number, 1.0)
    } else {
        (lower.as_str(), 1.0)
    };
    let value = number.trim().parse::<f64>().ok()? * factor;
    (value.is_finite() && value >= 0.0).then_some(value)
}

/// A latitude/longitude rectangle, e.g. for a cheap pre-filter before an
/// exact distance check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingBox {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl BoundingBox {
    /// The smallest box around `center` that contains every point within
    /// `km` kilometres, as [`LatLon::distance_km`] measures them (great-circle
    /// distance on a sphere of [`EARTH_RADIUS_KM`]). Latitudes are clamped to
    /// ±90°. When the circle reaches a pole, or the box would cross the
    /// antimeridian, it spans all longitudes rather than computing a wrong
    /// bound.
    pub fn around(center: &LatLon, km: f64) -> Self {
        // The angular radius, and the largest longitude difference on the
        // circle: further east or west than `dlat`, and reached slightly
        // poleward of `center`'s latitude.
        let angle = (km.max(0.0) / EARTH_RADIUS_KM).min(std::f64::consts::PI);
        let dlat = angle.to_degrees();
        let ratio = angle.sin() / center.lat.to_radians().cos();
        let dlon = if angle < std::f64::consts::FRAC_PI_2 && ratio < 1.0 {
            ratio.asin().to_degrees()
        } else {
            360.0
        };
        let (min_lon, max_lon) = (center.lon - dlon, center.lon + dlon);
        let (min_lon, max_lon) = if min_lon < -180.0 || max_lon > 180.0 {
            (-180.0, 180.0)
        } else {
            (min_lon, max_lon)
        };
        Self {
            min_lat: (center.lat - dlat).max(-90.0),
            max_lat: (center.lat + dlat).min(90.0),
            min_lon,
            max_lon,
        }
    }

    /// Whether `point` lies in the box, borders included.
    pub fn contains(&self, point: &LatLon) -> bool {
        (self.min_lat..=self.max_lat).contains(&point.lat)
            && (self.min_lon..=self.max_lon).contains(&point.lon)
    }

    /// Whether the box spans every longitude (see [`Self::around`]).
    pub fn spans_all_longitudes(&self) -> bool {
        self.min_lon <= -180.0 && self.max_lon >= 180.0
    }

    /// A SPARQL `FILTER` keeping `lat_var`/`lon_var` (with or without `?`)
    /// inside the box; the longitude part is left out when the box spans
    /// every longitude.
    pub fn sparql_filter(&self, lat_var: &str, lon_var: &str) -> String {
        let lat = format!("?{}", lat_var.trim_start_matches('?'));
        let lon = format!("?{}", lon_var.trim_start_matches('?'));
        let latitude = format!("{lat} >= {} && {lat} <= {}", self.min_lat, self.max_lat);
        if self.spans_all_longitudes() {
            format!("FILTER({latitude})")
        } else {
            format!(
                "FILTER({latitude} && {lon} >= {} && {lon} <= {})",
                self.min_lon, self.max_lon
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str() {
        assert_eq!(
            LatLon::from_str("12.34,56.78").unwrap(),
            LatLon::new(12.34, 56.78)
        );
        assert!(LatLon::from_str("12.34,ABC").is_err());
        assert!(LatLon::from_str("12.34").is_err());
        assert!(LatLon::from_str("ABC,56.78").is_err());
    }

    #[test]
    fn test_new() {
        assert_eq!(
            LatLon::new(12.34, 56.78),
            LatLon {
                lat: 12.34,
                lon: 56.78
            }
        );
    }

    #[test]
    fn test_serialize() {
        let lat_lon = LatLon::new(12.34, 56.78);
        let serialized = serde_json::to_string(&lat_lon).unwrap();
        assert_eq!(serialized, r#"{"lat":12.34,"lon":56.78}"#);
    }

    #[test]
    fn test_deserialize() {
        let deserialized: LatLon = serde_json::from_str(r#"{"lat":12.34,"lon":56.78}"#).unwrap();
        assert_eq!(deserialized, LatLon::new(12.34, 56.78));
    }

    #[test]
    fn test_from_str_negative_coordinates() {
        // Southern hemisphere latitude and western hemisphere longitude.
        let ll = LatLon::from_str("-33.87,151.21").unwrap();
        assert!((ll.lat - (-33.87)).abs() < 1e-9);
        assert!((ll.lon - 151.21).abs() < 1e-9);

        let ll2 = LatLon::from_str("51.5,-0.12").unwrap();
        assert!((ll2.lat - 51.5).abs() < 1e-9);
        assert!((ll2.lon - (-0.12)).abs() < 1e-9);
    }

    #[test]
    fn test_from_str_origin() {
        let ll = LatLon::from_str("0,0").unwrap();
        assert_eq!(ll.lat, 0.0);
        assert_eq!(ll.lon, 0.0);
    }

    #[test]
    fn test_from_str_empty_string_is_err() {
        assert!(LatLon::from_str("").is_err());
    }

    #[test]
    fn test_from_str_only_comma_is_err() {
        // A bare comma gives empty lat and lon strings, both of which fail f64 parsing.
        assert!(LatLon::from_str(",").is_err());
    }

    #[test]
    fn test_serialize_negative_coordinates() {
        let ll = LatLon::new(-33.87, 151.21);
        let json = serde_json::to_string(&ll).unwrap();
        let roundtrip: LatLon = serde_json::from_str(&json).unwrap();
        assert!((roundtrip.lat - ll.lat).abs() < 1e-9);
        assert!((roundtrip.lon - ll.lon).abs() < 1e-9);
    }

    #[test]
    fn test_clone_and_equality() {
        let ll = LatLon::new(10.0, 20.0);
        let cloned = ll.clone();
        assert_eq!(ll, cloned);
    }

    #[test]
    fn test_from_str_more_forms() {
        for s in [
            "12.34,56.78",
            "12.34/56.78",
            "@12.34,56.78",
            "@12.34/56.78",
            " 12.34, 56.78 ",
            "12.34 / 56.78",
        ] {
            assert_eq!(
                LatLon::from_str(s).unwrap(),
                LatLon::new(12.34, 56.78),
                "{s}"
            );
        }
        assert!(LatLon::from_str("@").is_err());
        assert!(LatLon::from_str("12.34 56.78").is_err());
        assert!(matches!(
            LatLon::from_str("12.34"),
            Err(LatLonError::Longitude(_))
        ));
        assert!(matches!(
            LatLon::from_str("abc/1"),
            Err(LatLonError::Latitude(_))
        ));
    }

    #[test]
    fn test_try_new() {
        assert!(LatLon::try_new(90.0, -180.0).is_ok());
        assert!(LatLon::try_new(90.1, 0.0).is_err());
        assert!(LatLon::try_new(0.0, 180.5).is_err());
        assert!(matches!(
            LatLon::try_new(f64::NAN, 0.0),
            Err(LatLonError::OutOfRange { .. })
        ));
        assert!(LatLon::try_new(0.0, f64::INFINITY).is_err());
    }

    #[test]
    fn test_wkt() {
        let berlin = LatLon::new(52.516667, 13.383333);
        assert_eq!(
            LatLon::from_wkt("Point(13.383333 52.516667)"),
            Some(berlin.clone())
        );
        assert_eq!(
            LatLon::from_wkt("POINT(13.383333 52.516667)"),
            Some(berlin.clone())
        );
        assert_eq!(
            LatLon::from_wkt("point ( 13.383333   52.516667 )"),
            Some(berlin.clone())
        );
        assert_eq!(
            LatLon::from_wkt("<http://www.wikidata.org/entity/Q2> Point(13.383333 52.516667)"),
            Some(berlin.clone())
        );
        assert_eq!(
            LatLon::from_wkt("Point(5.25125E1 -1.5e-1)"),
            Some(LatLon::new(-0.15, 52.5125))
        );
        // The Moon is not on Earth.
        assert_eq!(
            LatLon::from_wkt("<http://www.wikidata.org/entity/Q405> Point(1 2)"),
            None
        );
        for bad in [
            "",
            "Point()",
            "Point(1)",
            "Point(1 2 3)",
            "Point(1,2)",
            "Point(a b)",
            "Pointless(1 2)",
            "Point(1 2",
            "Point(+1 2)",
            "Point(.5 2)",
        ] {
            assert_eq!(LatLon::from_wkt(bad), None, "{bad}");
        }
        assert_eq!(berlin.to_wkt(), "Point(13.383333 52.516667)");
        assert_eq!(LatLon::from_wkt(&berlin.to_wkt()), Some(berlin));
    }

    #[test]
    fn test_distance() {
        let berlin = LatLon::new(52.5167, 13.3833);
        let paris = LatLon::new(48.8566, 2.3522);
        let km = berlin.distance_km(&paris);
        assert!((km - 877.0).abs() < 5.0, "{km}");
        assert!((berlin.distance_m(&paris) - km * 1000.0).abs() < 1e-6);
        assert_eq!(berlin.distance_km(&berlin), 0.0);
        // A quarter of the way round the equator.
        let quarter = LatLon::new(0.0, 0.0).distance_km(&LatLon::new(0.0, 90.0));
        assert!((quarter - std::f64::consts::PI * EARTH_RADIUS_KM / 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_parse_distance_m() {
        assert_eq!(parse_distance_m("500m"), Some(500.0));
        assert_eq!(parse_distance_m("5km"), Some(5000.0));
        assert_eq!(parse_distance_m("5 km"), Some(5000.0));
        assert_eq!(parse_distance_m("1.5KM"), Some(1500.0));
        assert_eq!(parse_distance_m(" 250 "), Some(250.0));
        assert_eq!(parse_distance_m("-5km"), None);
        assert_eq!(parse_distance_m("km"), None);
        assert_eq!(parse_distance_m("5 miles"), None);
        assert_eq!(parse_distance_m("inf"), None);
    }

    #[test]
    fn test_bounding_box() {
        let berlin = LatLon::new(52.5167, 13.3833);
        let bbox = BoundingBox::around(&berlin, 10.0);
        assert!(bbox.contains(&berlin));
        assert!(bbox.contains(&LatLon::new(52.55, 13.4)));
        assert!(!bbox.contains(&LatLon::new(48.8566, 2.3522)));
        assert!(!bbox.spans_all_longitudes());
        let dlat = (10.0 / EARTH_RADIUS_KM).to_degrees();
        assert!((bbox.max_lat - bbox.min_lat - 2.0 * dlat).abs() < 1e-9);
        let filter = bbox.sparql_filter("lat", "?lon");
        assert!(filter.starts_with("FILTER(?lat >= "), "{filter}");
        assert!(filter.contains("&& ?lon >= "), "{filter}");

        // Across the antimeridian: all longitudes.
        let fiji = BoundingBox::around(&LatLon::new(-17.0, 179.9), 50.0);
        assert!(fiji.spans_all_longitudes());
        assert!(fiji.contains(&LatLon::new(-17.0, -179.9)));
        assert!(!fiji.sparql_filter("lat", "lon").contains("?lon"));

        // At the pole: all longitudes, latitude clamped.
        let pole = BoundingBox::around(&LatLon::new(89.99, 0.0), 100.0);
        assert!(pole.spans_all_longitudes());
        assert_eq!(pole.max_lat, 90.0);
    }

    /// The point `km` from `from` in direction `bearing` (degrees from north).
    fn destination(from: &LatLon, bearing: f64, km: f64) -> LatLon {
        let (lat, lon, bearing) = (
            from.lat.to_radians(),
            from.lon.to_radians(),
            bearing.to_radians(),
        );
        let angle = km / EARTH_RADIUS_KM;
        let lat2 = (lat.sin() * angle.cos() + lat.cos() * angle.sin() * bearing.cos()).asin();
        let lon2 = lon
            + (bearing.sin() * angle.sin() * lat.cos()).atan2(angle.cos() - lat.sin() * lat2.sin());
        LatLon::new(lat2.to_degrees(), lon2.to_degrees())
    }

    #[test]
    fn test_bounding_box_contains_the_whole_circle() {
        for (lat, km) in [
            (0.0, 5.0),
            (37.98, 5.0),
            (52.5, 100.0),
            (-70.0, 500.0),
            (80.0, 300.0),
        ] {
            let center = LatLon::new(lat, 23.73);
            let bbox = BoundingBox::around(&center, km);
            for bearing in (0..360).step_by(5) {
                // Just inside the radius, so float noise cannot push it out.
                let point = destination(&center, f64::from(bearing), km * 0.999_999);
                assert!(center.distance_km(&point) <= km);
                assert!(
                    bbox.contains(&point),
                    "{point:?} ({bearing}°, {km} km from {center:?}) outside {bbox:?}"
                );
            }
            // ... and the box is tight: points just beyond it are further away.
            let beyond = LatLon::new(bbox.max_lat + 0.001, center.lon);
            assert!(center.distance_km(&beyond) > km);
        }
    }
}
