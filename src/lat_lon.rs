//! Stores latitude/longitude coordinates.

use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

/// Failure modes of [`LatLon::from_str`].
#[derive(Debug, Error)]
pub enum LatLonError {
    /// The latitude is absent or not a number.
    #[error("cannot parse latitude from '{0}'")]
    Latitude(String),

    /// The longitude is absent or not a number.
    #[error("cannot parse longitude from '{0}'")]
    Longitude(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }
}

impl FromStr for LatLon {
    type Err = LatLonError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // A missing component and an unparseable one are the same failure as far
        // as a caller is concerned, so both map to the per-component variant.
        let latitude = || LatLonError::Latitude(s.to_string());
        let longitude = || LatLonError::Longitude(s.to_string());

        let mut parts = s.split(',');
        let lat = parts
            .next()
            .ok_or_else(latitude)?
            .parse::<f64>()
            .map_err(|_| latitude())?;
        let lon = parts
            .next()
            .ok_or_else(longitude)?
            .parse::<f64>()
            .map_err(|_| longitude())?;
        Ok(Self { lat, lon })
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
}
