//! Stores latitude/longitude coordinates.

#[derive(Debug, Clone, PartialEq)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }

    /// Parses a string of the form "lat,lon" into a `LatLon`.
    pub fn from_str(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(',').collect();
        /* trunk-ignore(clippy/get_first) */
        let lat = parts.get(0)?.parse::<f64>().ok()?;
        let lon = parts.get(1)?.parse::<f64>().ok()?;
        Some(Self { lat, lon })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str() {
        assert_eq!(
            LatLon::from_str("12.34,56.78"),
            Some(LatLon::new(12.34, 56.78))
        );
        assert_eq!(LatLon::from_str("12.34,ABC"), None);
        assert_eq!(LatLon::from_str("12.34"), None);
        assert_eq!(LatLon::from_str("ABC,56.78,"), None);
    }
}
