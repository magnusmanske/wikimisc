//! Stores latitude/longitude coordinates.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

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
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split(',');
        let lat = parts
            .next()
            .ok_or_else(|| anyhow!("Cannot parse latitude from '{s}'"))?
            .parse::<f64>()?;
        let lon = parts
            .next()
            .ok_or_else(|| anyhow!("Cannot parse longitude from '{s}'"))?
            .parse::<f64>()?;
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
}
