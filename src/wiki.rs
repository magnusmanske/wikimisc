//! Wikimedia wikis, as named by the `language`/`project` parameters of the
//! old tools. Only Wikimedia hosts can come out of this, so user input can
//! never point a request elsewhere.

use std::fmt;

/// Project families that have one wiki per language, at `<language>.<project>.org`.
const LANGUAGE_PROJECTS: &[&str] = &[
    "wikipedia",
    "wiktionary",
    "wikisource",
    "wikibooks",
    "wikinews",
    "wikiquote",
    "wikiversity",
    "wikivoyage",
];

/// "Languages" that name a single wiki, whatever the project: name, host, database.
const SPECIAL_WIKIS: &[(&str, &str, &str)] = &[
    ("commons", "commons.wikimedia.org", "commonswiki"),
    ("meta", "meta.wikimedia.org", "metawiki"),
    ("species", "species.wikimedia.org", "specieswiki"),
    ("incubator", "incubator.wikimedia.org", "incubatorwiki"),
    ("wikidata", "www.wikidata.org", "wikidatawiki"),
    ("mediawiki", "www.mediawiki.org", "mediawikiwiki"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wiki {
    host: String,
    dbname: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown wiki: {language}.{project}")]
pub struct UnknownWiki {
    pub language: String,
    pub project: String,
}

impl Wiki {
    /// `("en", "wikipedia")`, `("commons", <anything>)`, `("de", "wikisource")`, …
    pub fn new(language: &str, project: &str) -> Result<Self, UnknownWiki> {
        let language = language.trim().to_lowercase();
        let project = project.trim().to_lowercase();
        let special = SPECIAL_WIKIS
            .iter()
            .find(|(name, ..)| *name == language || *name == project);
        match special {
            Some((name, ..)) => Ok(Self::special(name)),
            None if LANGUAGE_PROJECTS.contains(&project.as_str())
                && is_language_code(&language) =>
            {
                let family = if project == "wikipedia" {
                    "wiki"
                } else {
                    &project
                };
                Ok(Self {
                    host: format!("{language}.{project}.org"),
                    dbname: format!("{}{family}", language.replace('-', "_")),
                })
            }
            None => Err(UnknownWiki { language, project }),
        }
    }

    /// From the old tools' `en.wikipedia` form.
    pub fn from_domain_prefix(prefix: &str) -> Result<Self, UnknownWiki> {
        let (language, project) = prefix.trim().split_once('.').unwrap_or((prefix, ""));
        Self::new(language, project)
    }

    /// From a database name: `enwiki`, `dewikisource`, `commonswiki`, …
    pub fn from_dbname(dbname: &str) -> Result<Self, UnknownWiki> {
        let dbname = dbname.trim().trim_end_matches("_p");
        if let Some((name, ..)) = SPECIAL_WIKIS.iter().find(|(.., db)| *db == dbname) {
            return Ok(Self::special(name));
        }
        let unknown = || UnknownWiki {
            language: dbname.to_string(),
            project: String::new(),
        };
        let (language, project) = LANGUAGE_PROJECTS
            .iter()
            .filter(|p| **p != "wikipedia")
            .find_map(|p| dbname.strip_suffix(p).map(|l| (l, *p)))
            .or_else(|| dbname.strip_suffix("wiki").map(|l| (l, "wikipedia")))
            .ok_or_else(unknown)?;
        Self::new(&language.replace('_', "-"), project)
    }

    fn special(name: &str) -> Self {
        let (_, host, dbname) = SPECIAL_WIKIS
            .iter()
            .find(|(n, ..)| *n == name)
            .copied()
            .unwrap_or(SPECIAL_WIKIS[0]);
        Self {
            host: host.to_string(),
            dbname: dbname.to_string(),
        }
    }

    pub fn commons() -> Self {
        Self::special("commons")
    }

    pub fn wikidata() -> Self {
        Self::special("wikidata")
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// The database name on the replicas, e.g. `enwiki`, `dewikisource`.
    pub fn dbname(&self) -> &str {
        &self.dbname
    }

    pub fn api_url(&self) -> String {
        format!("https://{}/w/api.php", self.host)
    }

    pub fn index_url(&self) -> String {
        format!("https://{}/w/index.php", self.host)
    }

    pub fn page_url(&self, title: &str) -> String {
        format!("https://{}/wiki/{}", self.host, encode_title(title))
    }
}

impl fmt::Display for Wiki {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.host)
    }
}

/// `s` with its first letter in upper case, as wikis have page names.
pub fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// `en`, `simple`, `zh-min-nan`, `be-tarask`, …
pub fn is_language_code(code: &str) -> bool {
    (2..=20).contains(&code.len())
        && code.starts_with(|c: char| c.is_ascii_lowercase())
        && !code.ends_with('-')
        && !code.contains("--")
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A page title as it appears in a wiki URL: spaces as underscores, the rest percent-encoded.
pub fn encode_title(title: &str) -> String {
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'_')
        .remove(b'-')
        .remove(b'.')
        .remove(b':')
        .remove(b'/')
        .remove(b'(')
        .remove(b')')
        .remove(b',');
    percent_encoding::utf8_percent_encode(&title.trim().replace(' ', "_"), KEEP).to_string()
}

/// A value for a URL query or path segment: everything but `A-Za-z0-9-._~` percent-encoded.
pub fn encode_component(value: &str) -> String {
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(value, KEEP).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(language: &str, project: &str) -> Option<String> {
        Wiki::new(language, project)
            .ok()
            .map(|w| w.host().to_string())
    }

    #[test]
    fn hosts() {
        assert_eq!(host("en", "wikipedia").as_deref(), Some("en.wikipedia.org"));
        assert_eq!(
            host(" DE ", "wikisource").as_deref(),
            Some("de.wikisource.org")
        );
        assert_eq!(
            host("zh-min-nan", "wikipedia").as_deref(),
            Some("zh-min-nan.wikipedia.org")
        );
        assert_eq!(
            host("commons", "wikimedia").as_deref(),
            Some("commons.wikimedia.org")
        );
        assert_eq!(
            host("commons", "wikipedia").as_deref(),
            Some("commons.wikimedia.org")
        );
        assert_eq!(host("en", "wikidata").as_deref(), Some("www.wikidata.org"));
        assert_eq!(
            host("wikidata", "wikipedia").as_deref(),
            Some("www.wikidata.org")
        );
    }

    #[test]
    fn nothing_but_wikimedia_hosts() {
        for (language, project) in [
            ("evil.com#", "wikipedia"),
            ("en", "evil"),
            ("en", "wikipedia.org.evil.com"),
            ("a", "wikipedia"),
            ("en-", "wikipedia"),
            ("", "wikipedia"),
            ("en/x", "wikipedia"),
            ("de", "wikimedia"),
        ] {
            assert_eq!(host(language, project), None, "{language}.{project}");
        }
    }

    #[test]
    fn database_names() {
        let dbname = |l: &str, p: &str| Wiki::new(l, p).unwrap().dbname().to_string();
        assert_eq!(dbname("en", "wikipedia"), "enwiki");
        assert_eq!(dbname("zh-min-nan", "wikipedia"), "zh_min_nanwiki");
        assert_eq!(dbname("de", "wikisource"), "dewikisource");
        assert_eq!(dbname("commons", "wikimedia"), "commonswiki");
        assert_eq!(dbname("en", "wikidata"), "wikidatawiki");
        assert_eq!(
            Wiki::from_domain_prefix("fr.wiktionary").unwrap().dbname(),
            "frwiktionary"
        );
        assert!(Wiki::from_domain_prefix("enwiki").is_err());
        let host = |db: &str| Wiki::from_dbname(db).map(|w| w.host().to_string());
        assert_eq!(host("metawiki").as_deref(), Ok("meta.wikimedia.org"));
        assert_eq!(host("dewiki").as_deref(), Ok("de.wikipedia.org"));
        assert_eq!(
            host("zh_min_nanwiki").as_deref(),
            Ok("zh-min-nan.wikipedia.org")
        );
        assert_eq!(host("frwikisource_p").as_deref(), Ok("fr.wikisource.org"));
        assert_eq!(host("wikidatawiki").as_deref(), Ok("www.wikidata.org"));
        assert!(Wiki::from_dbname("nonsense").is_err());
    }

    #[test]
    fn urls() {
        let wiki = Wiki::new("de", "wikipedia").unwrap();
        assert_eq!(wiki.api_url(), "https://de.wikipedia.org/w/api.php");
        assert_eq!(
            wiki.page_url("Benutzer:Magnus Manske/Spacer"),
            "https://de.wikipedia.org/wiki/Benutzer:Magnus_Manske/Spacer"
        );
        assert_eq!(encode_title("A&B?c d"), "A%26B%3Fc_d");
        assert_eq!(encode_component("1957-001A a&b"), "1957-001A%20a%26b");
    }
}
