//! Request parameters the way the old PHP tools read them (`$_REQUEST`):
//! from the query string and, for form posts, the body. Forms keep working
//! whether they use GET or POST, and links to results keep working too.

use serde::de::DeserializeOwned;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid parameters: {0}")]
pub struct ParamError(pub String);

/// Deserializes `T` from query and form parameters; on a repeated name the
/// last value wins, so the body beats the query string, as in PHP.
pub fn merge<T: DeserializeOwned>(query: &str, body: &[u8]) -> Result<T, ParamError> {
    let bad = |e: &dyn std::fmt::Display| ParamError(e.to_string());
    let mut pairs: Vec<(String, String)> =
        serde_urlencoded::from_str(query).map_err(|e| bad(&e))?;
    pairs.extend(serde_urlencoded::from_bytes::<Vec<(String, String)>>(body).map_err(|e| bad(&e))?);
    let last_wins: HashMap<String, String> = pairs.into_iter().collect();
    let encoded = serde_urlencoded::to_string(&last_wins).map_err(|e| bad(&e))?;
    serde_urlencoded::from_str(&encoded).map_err(|e| bad(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct P {
        #[serde(default)]
        text: String,
        depth: Option<u32>,
        doit: Option<String>,
    }

    #[test]
    fn query_and_body_merge_with_the_body_winning() {
        let p: P = merge("text=from+query&depth=3", b"text=from%20body&doit=1").unwrap();
        assert_eq!(
            p,
            P {
                text: "from body".into(),
                depth: Some(3),
                doit: Some("1".into())
            }
        );
    }

    #[test]
    fn missing_and_invalid() {
        let p: P = merge("", b"").unwrap();
        assert_eq!(
            p,
            P {
                text: String::new(),
                depth: None,
                doit: None
            }
        );
        assert!(merge::<P>("depth=deep", b"").is_err());
    }
}
