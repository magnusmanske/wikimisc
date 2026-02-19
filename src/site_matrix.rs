//! Manages a site matrix for all sites in the WikiVerse.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use wikibase::mediawiki::api::Api;

#[derive(Debug, Clone, Default)]
pub struct SiteMatrix {
    site_matrix: Value,
}

impl SiteMatrix {
    /// Create a new SiteMatrix object
    pub async fn new(api: &Api) -> Result<Self> {
        let params = Self::str_vec_to_hashmap(&[("action", "sitematrix")]);
        let site_matrix = api.get_query_api_json(&params).await?;
        Ok(Self { site_matrix })
    }

    /// Convert a vector of string tuples to a hashmap of String keys and values
    pub fn str_vec_to_hashmap(v: &[(&str, &str)]) -> HashMap<String, String> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Get the URL for a wiki from a site matrix entry
    fn get_url_for_wiki_from_site(&self, wiki: &str, site: &Value) -> Option<String> {
        self.get_value_from_site_matrix_entry(wiki, site, "dbname", "url")
    }

    /// Get the value for a key from a site matrix entry
    fn get_value_from_site_matrix_entry(
        &self,
        value: &str,
        site: &Value,
        key_match: &str,
        key_return: &str,
    ) -> Option<String> {
        // Skip closed or private sites
        if site["closed"].as_str().is_some() || site["private"].as_str().is_some() {
            return None;
        }

        site[key_match]
            .as_str()
            .filter(|&site_value| {
                // Normalize underscores and hyphens for comparison: the SiteMatrix API stores
                // some dbnames with underscores where the wiki name uses hyphens, e.g.
                // "zh_classicalwiki" (API) vs "zh-classicalwiki" (caller), so we treat them
                // as equivalent. This does not affect URL-based lookups since URLs use hyphens
                // consistently.
                site_value.replace('_', "-") == value.replace('_', "-")
            })
            .and_then(|_| site[key_return].as_str().map(String::from))
    }

    /// Normalize a wiki name by stripping a spurious trailing "wiki" suffix that is sometimes
    /// appended to project names that already contain "wiki" in them.
    ///
    /// For example, PetScan and users sometimes specify `enwiktionarywiki` instead of the
    /// correct MediaWiki dbname `enwiktionary`. This function detects those cases and strips
    /// the extra "wiki" suffix, converting e.g.:
    /// - `enwiktionarywiki`  → `enwiktionary`
    /// - `enwikibookswiki`   → `enwikibooks`
    /// - `enwikiquotewiki`   → `enwikiquote`
    /// - `enwikinewswiki`    → `enwikinews`
    /// - `enwikisourcewiki`  → `enwikisource`
    /// - `enwikiversitywiki` → `enwikiversity`
    /// - `enwikivoyagewiki`  → `enwikivoyage`
    ///
    /// Names that are already correct (e.g. `enwiki`, `enwiktionary`) are returned unchanged.
    fn normalize_wiki_name(wiki: &str) -> String {
        const SUFFIXES_WITH_EXTRA_WIKI: &[&str] = &[
            "wiktionarywiki",
            "wikibookswiki",
            "wikiquotewiki",
            "wikinewswiki",
            "wikisourcewiki",
            "wikiversitywiki",
            "wikivoyagewiki",
        ];
        for suffix in SUFFIXES_WITH_EXTRA_WIKI {
            if wiki.ends_with(suffix) {
                // Strip the trailing "wiki" (4 characters)
                return wiki[..wiki.len() - 4].to_string();
            }
        }
        wiki.to_string()
    }

    /// Get the server URL for a wiki
    pub fn get_server_url_for_wiki(&self, wiki: &str) -> Result<String> {
        // Normalize the wiki name first: strip any spurious trailing "wiki" suffix
        // that may have been appended to project names like "wiktionary", "wikibooks", etc.
        let wiki = &Self::normalize_wiki_name(wiki);

        // Handle special cases
        match wiki.replace('_', "-").as_str() {
            "be-taraskwiki" | "be-x-oldwiki" => {
                return Ok("https://be-tarask.wikipedia.org".to_string())
            }
            "metawiki" => return Ok("https://meta.wikimedia.org".to_string()),
            _ => {}
        }

        self.site_matrix["sitematrix"]
            .as_object()
            .ok_or_else(|| {
                anyhow!("SiteMatrix::get_server_url_for_wiki: sitematrix not an object")
            })?
            .iter()
            .find_map(|(id, data)| match id.as_str() {
                "count" => None,
                "specials" => data.as_array().and_then(|arr| {
                    arr.iter()
                        .find_map(|site| self.get_url_for_wiki_from_site(wiki, site))
                }),
                _other => data["site"].as_array().and_then(|sites| {
                    sites
                        .iter()
                        .find_map(|site| self.get_url_for_wiki_from_site(wiki, site))
                }),
            })
            .ok_or_else(|| {
                anyhow!("SiteMatrix::get_server_url_for_wiki: Cannot find server for wiki '{wiki}'")
            })
    }

    fn get_wiki_for_server_url_from_site(&self, url: &str, site: &Value) -> Option<String> {
        self.get_value_from_site_matrix_entry(url, site, "url", "dbname")
    }

    pub fn is_language_rtl(&self, language: &str) -> bool {
        self.site_matrix["sitematrix"]
            .as_object()
            .expect("SiteMatrix::is_language_rtl: sitematrix not an object")
            .iter()
            .any(|(_id, data)| {
                matches!(
                    (data["code"].as_str(), data["dir"].as_str()),
                    (Some(lang), Some("rtl")) if lang == language
                )
            })
    }

    pub fn get_wiki_for_server_url(&self, url: &str) -> Option<String> {
        self.site_matrix["sitematrix"]
            .as_object()
            .expect("SiteMatrix::get_wiki_for_server_url: sitematrix not an object")
            .iter()
            .find_map(|(id, data)| match id.as_str() {
                "count" => None,
                "specials" => data
                    .as_array()
                    .expect("SiteMatrix::get_wiki_for_server_url: 'specials' is not an array")
                    .iter()
                    .find_map(|site| self.get_wiki_for_server_url_from_site(url, site)),
                _other => data["site"].as_array().and_then(|sites| {
                    sites
                        .iter()
                        .find_map(|site| self.get_wiki_for_server_url_from_site(url, site))
                }),
            })
    }

    pub async fn get_api_for_wiki(&self, wiki: &str) -> Result<Api> {
        let url = self.get_server_url_for_wiki(wiki)? + "/w/api.php";
        Api::new(&url).await.map_err(|e| anyhow!(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // normalize_wiki_name tests (offline, no network required)
    // -----------------------------------------------------------------------

    #[test]
    fn test_normalize_wiki_name_wiktionary() {
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwiktionarywiki"),
            "enwiktionary"
        );
        assert_eq!(
            SiteMatrix::normalize_wiki_name("dewiktionarywiki"),
            "dewiktionary"
        );
        assert_eq!(
            SiteMatrix::normalize_wiki_name("frwiktionarywiki"),
            "frwiktionary"
        );
    }

    #[test]
    fn test_normalize_wiki_name_wikibooks() {
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwikibookswiki"),
            "enwikibooks"
        );
    }

    #[test]
    fn test_normalize_wiki_name_wikiquote() {
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwikiquotewiki"),
            "enwikiquote"
        );
    }

    #[test]
    fn test_normalize_wiki_name_wikinews() {
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwikinewswiki"),
            "enwikinews"
        );
    }

    #[test]
    fn test_normalize_wiki_name_wikisource() {
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwikisourcewiki"),
            "enwikisource"
        );
    }

    #[test]
    fn test_normalize_wiki_name_wikiversity() {
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwikiversitywiki"),
            "enwikiversity"
        );
    }

    #[test]
    fn test_normalize_wiki_name_wikivoyage() {
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwikivoyagewiki"),
            "enwikivoyage"
        );
    }

    #[test]
    fn test_normalize_wiki_name_unchanged() {
        // Names that are already correct must pass through untouched
        assert_eq!(SiteMatrix::normalize_wiki_name("enwiki"), "enwiki");
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwiktionary"),
            "enwiktionary"
        );
        assert_eq!(
            SiteMatrix::normalize_wiki_name("enwikibooks"),
            "enwikibooks"
        );
        assert_eq!(
            SiteMatrix::normalize_wiki_name("wikidatawiki"),
            "wikidatawiki"
        );
        assert_eq!(SiteMatrix::normalize_wiki_name("metawiki"), "metawiki");
        assert_eq!(
            SiteMatrix::normalize_wiki_name("commonswiki"),
            "commonswiki"
        );
    }

    #[test]
    fn test_get_server_url_for_wiki_wiktionary_with_extra_wiki_suffix() {
        // Simulate what PetScan passes: "enwiktionarywiki" – the site matrix only
        // contains the correct dbname "enwiktionary", so normalize_wiki_name must
        // strip the trailing "wiki" before the lookup.
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({
                "sitematrix": {
                    "count": 1,
                    "0": {
                        "code": "en",
                        "site": [
                            {
                                "url": "https://en.wikipedia.org",
                                "dbname": "enwiki",
                                "code": "wiki"
                            },
                            {
                                "url": "https://en.wiktionary.org",
                                "dbname": "enwiktionary",
                                "code": "wiktionary"
                            }
                        ]
                    },
                    "specials": []
                }
            }),
        };
        // The "wrong" name with extra "wiki" suffix must resolve correctly
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwiktionarywiki")
                .unwrap(),
            "https://en.wiktionary.org"
        );
        // The correct dbname must still work
        assert_eq!(
            site_matrix.get_server_url_for_wiki("enwiktionary").unwrap(),
            "https://en.wiktionary.org"
        );
        // Wikipedia must still work
        assert_eq!(
            site_matrix.get_server_url_for_wiki("enwiki").unwrap(),
            "https://en.wikipedia.org"
        );
    }

    #[test]
    fn test_get_server_url_for_wiki_other_projects_with_extra_wiki_suffix() {
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({
                "sitematrix": {
                    "count": 1,
                    "0": {
                        "code": "en",
                        "site": [
                            {"url": "https://en.wikibooks.org",   "dbname": "enwikibooks",   "code": "wikibooks"},
                            {"url": "https://en.wikiquote.org",   "dbname": "enwikiquote",   "code": "wikiquote"},
                            {"url": "https://en.wikinews.org",    "dbname": "enwikinews",    "code": "wikinews"},
                            {"url": "https://en.wikisource.org",  "dbname": "enwikisource",  "code": "wikisource"},
                            {"url": "https://en.wikiversity.org", "dbname": "enwikiversity", "code": "wikiversity"},
                            {"url": "https://en.wikivoyage.org",  "dbname": "enwikivoyage",  "code": "wikivoyage"}
                        ]
                    },
                    "specials": []
                }
            }),
        };
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikibookswiki")
                .unwrap(),
            "https://en.wikibooks.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikiquotewiki")
                .unwrap(),
            "https://en.wikiquote.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikinewswiki")
                .unwrap(),
            "https://en.wikinews.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikisourcewiki")
                .unwrap(),
            "https://en.wikisource.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikiversitywiki")
                .unwrap(),
            "https://en.wikiversity.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikivoyagewiki")
                .unwrap(),
            "https://en.wikivoyage.org"
        );
    }

    #[tokio::test]
    async fn test_site_matrix() {
        let api = Api::new("https://www.wikidata.org/w/api.php")
            .await
            .unwrap();
        let site_matrix = SiteMatrix::new(&api).await.unwrap();
        assert_eq!(
            site_matrix.get_server_url_for_wiki("wikidatawiki").unwrap(),
            "https://www.wikidata.org"
        );
        assert_eq!(
            site_matrix.get_server_url_for_wiki("enwiki").unwrap(),
            "https://en.wikipedia.org".to_string()
        );
        assert_eq!(
            site_matrix.get_server_url_for_wiki("enwikisource").unwrap(),
            "https://en.wikisource.org"
        );
        assert!(site_matrix.get_server_url_for_wiki("shcswirk8d7g").is_err());
        // Wiktionary: both the correct dbname and the "extra wiki" variant must work
        assert_eq!(
            site_matrix.get_server_url_for_wiki("enwiktionary").unwrap(),
            "https://en.wiktionary.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwiktionarywiki")
                .unwrap(),
            "https://en.wiktionary.org"
        );
        // Other projects with the extra "wiki" suffix
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikibookswiki")
                .unwrap(),
            "https://en.wikibooks.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikiquotewiki")
                .unwrap(),
            "https://en.wikiquote.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikinewswiki")
                .unwrap(),
            "https://en.wikinews.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikisourcewiki")
                .unwrap(),
            "https://en.wikisource.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikiversitywiki")
                .unwrap(),
            "https://en.wikiversity.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("enwikivoyagewiki")
                .unwrap(),
            "https://en.wikivoyage.org"
        );
    }

    #[tokio::test]
    async fn is_language_rtl() {
        let api = Api::new("https://www.wikidata.org/w/api.php")
            .await
            .unwrap();
        let site_matrix = SiteMatrix::new(&api).await.unwrap();
        assert!(!site_matrix.is_language_rtl("en"));
        assert!(site_matrix.is_language_rtl("ar"));
        assert!(!site_matrix.is_language_rtl("de"));
        assert!(site_matrix.is_language_rtl("he"));
    }

    #[test]
    fn test_str_vec_to_hashmap() {
        let v = vec![("a", "b"), ("c", "d")];
        let h = SiteMatrix::str_vec_to_hashmap(&v);
        assert_eq!(h.get("a"), Some(&"b".to_string()));
        assert_eq!(h.get("c"), Some(&"d".to_string()));
    }

    #[test]
    fn test_get_value_from_site_matrix_entry() {
        let site = serde_json::json!({
            "dbname": "wikidatawiki",
            "url": "https://www.wikidata.org",
            "closed": false,
            "private": false
        });
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({}),
        };
        let url =
            site_matrix.get_value_from_site_matrix_entry("wikidatawiki", &site, "dbname", "url");
        assert_eq!(url, Some("https://www.wikidata.org".to_string()));
    }

    #[test]
    fn test_get_url_for_wiki_from_site() {
        let site = serde_json::json!({
            "dbname": "wikidatawiki",
            "url": "https://www.wikidata.org",
            "closed": false,
            "private": false
        });
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({}),
        };
        let url = site_matrix.get_url_for_wiki_from_site("wikidatawiki", &site);
        assert_eq!(url, Some("https://www.wikidata.org".to_string()));
    }

    #[tokio::test]
    async fn test_site_matrix_zh_wikis() {
        let api = Api::new("https://www.wikidata.org/w/api.php")
            .await
            .unwrap();
        let site_matrix = SiteMatrix::new(&api).await.unwrap();
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("zh-classicalwiki")
                .unwrap(),
            "https://zh-classical.wikipedia.org"
        );
        assert_eq!(
            site_matrix.get_server_url_for_wiki("zh-yuewiki").unwrap(),
            "https://zh-yue.wikipedia.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("zh-min-nanwiki")
                .unwrap(),
            "https://zh-min-nan.wikipedia.org"
        );
    }

    #[test]
    fn test_get_server_url_for_wiki() {
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({
                "sitematrix": {
                    "count": 1,
                    "specials": [
                        {
                            "dbname": "wikidatawiki",
                            "url": "https://www.wikidata.org",
                            "closed": false,
                            "private": false
                        }
                    ]
                }
            }),
        };
        let url = site_matrix.get_server_url_for_wiki("wikidatawiki").unwrap();
        assert_eq!(url, "https://www.wikidata.org");
    }

    #[test]
    fn test_get_server_url_for_wiki_be_taraskwiki() {
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({
                "sitematrix": {
                    "count": 1,
                    "specials": [
                        {
                            "dbname": "be-taraskwiki",
                            "url": "https://be-tarask.wikipedia.org",
                            "closed": false,
                            "private": false
                        }
                    ]
                }
            }),
        };
        let url = site_matrix
            .get_server_url_for_wiki("be-taraskwiki")
            .unwrap();
        assert_eq!(url, "https://be-tarask.wikipedia.org");
    }

    #[test]
    fn test_get_server_url_for_wiki_metawiki() {
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({
                "sitematrix": {
                    "count": 1,
                    "specials": [
                        {
                            "dbname": "metawiki",
                            "url": "https://meta.wikimedia.org",
                            "closed": false,
                            "private": false
                        }
                    ]
                }
            }),
        };
        let url = site_matrix.get_server_url_for_wiki("metawiki").unwrap();
        assert_eq!(url, "https://meta.wikimedia.org");
    }

    #[test]
    fn test_get_server_url_for_wiki_underscore_dbname() {
        // The SiteMatrix API stores dbnames with underscores for wikis whose names use hyphens,
        // e.g. "zh_classicalwiki". The lookup must treat hyphens and underscores as equivalent.
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({
                "sitematrix": {
                    "count": 3,
                    "197": {
                        "code": "lzh",
                        "site": [
                            {
                                "url": "https://zh-classical.wikipedia.org",
                                "dbname": "zh_classicalwiki"
                            }
                        ]
                    },
                    "225": {
                        "code": "nan",
                        "site": [
                            {
                                "url": "https://zh-min-nan.wikipedia.org",
                                "dbname": "zh_min_nanwiki"
                            }
                        ]
                    },
                    "361": {
                        "code": "yue",
                        "site": [
                            {
                                "url": "https://zh-yue.wikipedia.org",
                                "dbname": "zh_yuewiki"
                            }
                        ]
                    },
                    "specials": []
                }
            }),
        };
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("zh-classicalwiki")
                .unwrap(),
            "https://zh-classical.wikipedia.org"
        );
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("zh-min-nanwiki")
                .unwrap(),
            "https://zh-min-nan.wikipedia.org"
        );
        assert_eq!(
            site_matrix.get_server_url_for_wiki("zh-yuewiki").unwrap(),
            "https://zh-yue.wikipedia.org"
        );
        // Also verify that lookup with underscores in the input works (the existing
        // replace('_', "-") at the start of get_server_url_for_wiki normalises those too).
        assert_eq!(
            site_matrix
                .get_server_url_for_wiki("zh_classicalwiki")
                .unwrap(),
            "https://zh-classical.wikipedia.org"
        );
    }

    #[test]
    fn test_get_server_url_for_wiki_not_found() {
        let site_matrix = SiteMatrix {
            site_matrix: serde_json::json!({
                "sitematrix": {
                    "count": 1,
                    "specials": [
                        {
                            "dbname": "wikidatawiki",
                            "url": "https://www.wikidata.org",
                            "closed": false,
                            "private": false
                        }
                    ]
                }
            }),
        };
        let url = site_matrix.get_server_url_for_wiki("notfoundwiki");
        assert!(url.is_err());
    }
}
