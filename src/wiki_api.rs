//! The common MediaWiki Action API requests the tools make.
//!
//! Everything here works on a [`Wiki`](crate::wiki::Wiki), so only Wikimedia
//! hosts can be reached. Domain-specific query building stays in the tools.

use crate::wiki::Wiki;
use anyhow::{Context, Result};
use serde_json::Value;

/// A GET request to the action API; `format=json&formatversion=2` is added.
pub async fn api(http: &reqwest::Client, wiki: &Wiki, params: &[(&str, &str)]) -> Result<Value> {
    http.get(wiki.api_url())
        .query(params)
        .query(&[("format", "json"), ("formatversion", "2")])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("API request to {wiki} failed"))?
        .json()
        .await
        .with_context(|| format!("API response from {wiki} is not JSON"))
}

/// As [`api`], but POSTed: for parameters too long for a URL.
pub async fn api_post(
    http: &reqwest::Client,
    wiki: &Wiki,
    params: &[(&str, &str)],
) -> Result<Value> {
    http.post(wiki.api_url())
        .query(&[("format", "json"), ("formatversion", "2")])
        .form(params)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("API request to {wiki} failed"))?
        .json()
        .await
        .with_context(|| format!("API response from {wiki} is not JSON"))
}

/// As [`api`], but asks again, after a while, when the wiki says there are
/// too many requests (429): for tools that make many in a row.
pub async fn api_patiently(
    http: &reqwest::Client,
    wiki: &Wiki,
    params: &[(&str, &str)],
) -> Result<Value> {
    const RETRIES: u32 = 4;
    let mut wait = std::time::Duration::from_secs(1);
    for _ in 0..RETRIES {
        match api(http, wiki, params).await {
            Err(e) if is_too_many_requests(&e) => {
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
            result => return result,
        }
    }
    api(http, wiki, params).await
}

fn is_too_many_requests(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status)
            == Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
    })
}

/// The current wikitext of a page, following redirects; `None` if there is no such page.
pub async fn page_wikitext(
    http: &reqwest::Client,
    wiki: &Wiki,
    title: &str,
) -> Result<Option<String>> {
    let params = [
        ("action", "query"),
        ("prop", "revisions"),
        ("rvprop", "content"),
        ("rvslots", "main"),
        ("redirects", "1"),
        ("titles", title),
    ];
    let json = api(http, wiki, &params).await?;
    Ok(
        json["query"]["pages"][0]["revisions"][0]["slots"]["main"]["content"]
            .as_str()
            .map(str::to_string),
    )
}

/// A page rendered to HTML (`action=render`); `None` if there is no such page.
pub async fn page_html(http: &reqwest::Client, wiki: &Wiki, title: &str) -> Result<Option<String>> {
    let response = http
        .get(wiki.index_url())
        .query(&[("action", "render"), ("title", title)])
        .send()
        .await
        .with_context(|| format!("cannot render {title} on {wiki}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let html = response.error_for_status()?.text().await?;
    Ok(Some(html))
}

/// Wiki text with its templates expanded, as the wiki would transclude it.
pub async fn expand_templates(
    http: &reqwest::Client,
    wiki: &Wiki,
    wikitext: &str,
) -> Result<String> {
    let json = api(
        http,
        wiki,
        &[
            ("action", "expandtemplates"),
            ("prop", "wikitext"),
            ("text", wikitext),
        ],
    )
    .await?;
    Ok(json["expandtemplates"]["wikitext"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

/// Wiki text rendered by the wiki, as an HTML fragment with absolute links.
pub async fn parse_to_html(http: &reqwest::Client, wiki: &Wiki, wikitext: &str) -> Result<String> {
    let params = [
        ("action", "parse"),
        ("contentmodel", "wikitext"),
        ("prop", "text"),
        ("disablelimitreport", "1"),
        ("disableeditsection", "1"),
        ("wrapoutputclass", ""),
        ("text", wikitext),
    ];
    let json = api_post(http, wiki, &params).await?;
    let html = json["parse"]["text"]
        .as_str()
        .with_context(|| format!("no HTML from {wiki}"))?;
    let root = format!("=\"https://{}/", wiki.host());
    Ok(["href", "src"]
        .iter()
        .fold(html.to_string(), |html, attribute| {
            html.replace(
                &format!("{attribute}=\"//"),
                &format!("{attribute}=\"https://"),
            )
            .replace(&format!("{attribute}=\"/"), &format!("{attribute}{root}"))
        }))
}

/// A template parameter value: a bare `|` would end it, so it becomes
/// `{{!}}`; those in links and nested templates stay.
pub fn template_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0usize;
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' | '{' | ']' | '}' if chars.peek() == Some(&c) => {
                chars.next();
                depth = if matches!(c, '[' | '{') {
                    depth + 1
                } else {
                    depth.saturating_sub(1)
                };
                out.extend([c, c]);
            }
            '|' if depth == 0 => out.push_str("{{!}}"),
            c => out.push(c),
        }
    }
    out
}

/// Files in a category and its subcategories down to `depth` (0: the
/// category alone), as titles without namespace; at most `limit` of them.
pub async fn category_files(
    http: &reqwest::Client,
    wiki: &Wiki,
    category: &str,
    depth: u32,
    limit: usize,
) -> Result<Vec<String>> {
    let category = category.trim();
    let category = strip_namespace(category, &["category", "kategorie"]).unwrap_or(category);
    let mut files = vec![];
    let mut seen = std::collections::HashSet::from([category.replace('_', " ")]);
    let mut level = vec![category.replace('_', " ")];
    for current_depth in 0..=depth {
        let mut next_level = vec![];
        for category in &level {
            let mut cont: Option<String> = None;
            loop {
                let title = format!("Category:{category}");
                let mut params = vec![
                    ("action", "query"),
                    ("list", "categorymembers"),
                    ("cmtitle", title.as_str()),
                    ("cmtype", "file|subcat"),
                    ("cmlimit", "max"),
                ];
                if let Some(c) = &cont {
                    params.push(("cmcontinue", c.as_str()));
                }
                let json = api(http, wiki, &params).await?;
                for member in json["query"]["categorymembers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let (Some(ns), Some(title)) = (member["ns"].as_i64(), member["title"].as_str())
                    else {
                        continue;
                    };
                    let name = title
                        .split_once(':')
                        .map_or(title, |(_, name)| name)
                        .to_string();
                    match ns {
                        6 => files.push(name),
                        14 if current_depth < depth && seen.insert(name.clone()) => {
                            next_level.push(name)
                        }
                        _ => {}
                    }
                    if files.len() >= limit {
                        return Ok(files);
                    }
                }
                cont = json["continue"]["cmcontinue"].as_str().map(str::to_string);
                if cont.is_none() {
                    break;
                }
            }
        }
        level = next_level;
    }
    Ok(files)
}

/// The text of an HTML snippet, such as a search result's: tags removed, references decoded.
pub fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    crate::escape::unescape_html(&text).into_owned()
}

/// `title` without a leading `Namespace:` from `namespaces` (case-insensitive).
pub fn strip_namespace<'a>(title: &'a str, namespaces: &[&str]) -> Option<&'a str> {
    let (namespace, rest) = title.split_once(':')?;
    namespaces
        .iter()
        .any(|ns| ns.eq_ignore_ascii_case(namespace.trim()))
        .then(|| rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_of_html() {
        assert_eq!(
            html_to_text(r#"A <span class="searchmatch">warthog</span> &amp; &lt;b&gt;"#),
            "A warthog & <b>"
        );
    }

    #[test]
    fn template_values() {
        assert_eq!(template_value("A | B"), "A {{!}} B");
        assert_eq!(
            template_value("[[A|B]] {{lang|de|C}}"),
            "[[A|B]] {{lang|de|C}}"
        );
        assert_eq!(template_value("]] x|y"), "]] x{{!}}y");
    }

    #[test]
    fn namespaces() {
        assert_eq!(strip_namespace("Category:X", &["category"]), Some("X"));
        assert_eq!(
            strip_namespace("file: A.ogg", &["file", "image"]),
            Some("A.ogg")
        );
        assert_eq!(strip_namespace("X: Y", &["category"]), None);
        assert_eq!(strip_namespace("X", &["category"]), None);
    }
}
