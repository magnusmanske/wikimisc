//! URLs of Wikimedia Commons files: file pages, `Special:FilePath`, and the
//! original and thumbnail URLs on `upload.wikimedia.org`.
//!
//! File names may be given with or without a `File:`/`Image:` prefix, with
//! spaces or underscores, and with a lowercase first letter; they are
//! normalised the way Commons does before a URL is built.

use crate::title::{self, Case};

/// The Commons server, for [`title::page_url`] and friends.
pub const COMMONS_SERVER: &str = "https://commons.wikimedia.org";

/// Where Commons keeps its files.
const UPLOAD_BASE: &str = "https://upload.wikimedia.org/wikipedia/commons";

/// The widths Wikimedia pre-renders thumbnails at. Other widths work, but are
/// rendered on demand and may be rate-limited, so prefer one of these (see
/// [`standard_thumb_width`]).
pub const STANDARD_THUMB_WIDTHS: [u32; 11] =
    [20, 40, 60, 120, 250, 330, 500, 960, 1280, 1920, 3840];

/// File names longer than this many bytes get thumbnails named
/// `thumbnail.<ext>` (Wikimedia's `abbrvThreshold`).
const THUMB_NAME_LIMIT: usize = 160;

/// The namespace names that mark a file.
const FILE_NAMESPACES: [&str; 2] = ["File", "Image"];

/// The file name without a `File:` or `Image:` prefix (any case); the name
/// itself if it has none.
pub fn strip_file_prefix(name: &str) -> &str {
    title::strip_namespace(name, &FILE_NAMESPACES).unwrap_or(name)
}

/// `https://commons.wikimedia.org/wiki/Special:FilePath/<name>[?width=N]`,
/// which redirects to the file, or to a thumbnail of the given width.
pub fn file_path_url(file: &str, width: Option<u32>) -> String {
    special_file_url(COMMONS_SERVER, "FilePath", file, width)
}

/// `{server}/wiki/Special:Redirect/file/<name>[?width=N]` on any wiki, which
/// redirects to the file (local or from Commons).
pub fn redirect_file_url(server: &str, file: &str, width: Option<u32>) -> String {
    special_file_url(server, "Redirect/file", file, width)
}

/// The URL of the file's description page on Commons.
pub fn file_page_url(file: &str) -> String {
    title::page_url(COMMONS_SERVER, &format!("File:{}", db_key(file)))
}

/// The URL of the original file on `upload.wikimedia.org`:
/// `https://upload.wikimedia.org/wikipedia/commons/a/a9/Example.jpg`.
pub fn upload_url(file: &str) -> String {
    let name = db_key(file);
    format!(
        "{UPLOAD_BASE}/{}/{}",
        hash_path(&name),
        title::percent_encode(&name)
    )
}

/// The URL of a thumbnail `width` pixels wide:
/// `https://upload.wikimedia.org/wikipedia/commons/thumb/a/a9/Example.jpg/120px-Example.jpg`.
///
/// Formats that are rendered to another format get the matching name: SVG as
/// PNG (`120px-Example.svg.png`), PDF and DjVu as the JPEG of their first page
/// (`page1-120px-…pdf.jpg`), TIFF as a lossy JPEG (`lossy-page1-120px-…tif.jpg`),
/// and video as a JPEG still (`120px--…webm.jpg`). Prefer a width from
/// [`STANDARD_THUMB_WIDTHS`].
pub fn thumb_url(file: &str, width: u32) -> String {
    let name = db_key(file);
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let thumb_name = if name.len() > THUMB_NAME_LIMIT {
        match extension.as_str() {
            "" => "thumbnail".to_string(),
            extension => format!("thumbnail.{extension}"),
        }
    } else {
        name.clone()
    };
    let (prefix, suffix) = match extension.as_str() {
        "svg" => ("", ".png"),
        "pdf" | "djvu" => ("page1-", ".jpg"),
        "tif" | "tiff" => ("lossy-page1-", ".jpg"),
        "webm" | "ogv" | "mpg" | "mpeg" => ("", ".jpg"),
        _ => ("", ""),
    };
    let separator = match extension.as_str() {
        "webm" | "ogv" | "mpg" | "mpeg" => "--",
        _ => "-",
    };
    format!(
        "{UPLOAD_BASE}/thumb/{}/{}/{prefix}{width}px{separator}{}{suffix}",
        hash_path(&name),
        title::percent_encode(&name),
        title::percent_encode(&thumb_name),
    )
}

/// The smallest of [`STANDARD_THUMB_WIDTHS`] that is at least `min`, or the
/// largest if none is.
pub fn standard_thumb_width(min: u32) -> u32 {
    let mut widths = STANDARD_THUMB_WIDTHS.iter().copied();
    widths
        .find(|&width| width >= min)
        .or_else(|| STANDARD_THUMB_WIDTHS.last().copied())
        .unwrap_or(min)
}

/// The file name (with spaces, without `File:`) from a Commons URL: a
/// `Special:FilePath` or `Special:Redirect/file` URL, a file page URL, or an
/// original or thumbnail URL on `upload.wikimedia.org`. `None` for anything
/// else.
pub fn file_name_from_url(url: &str) -> Option<String> {
    let encoded = special_file_name(url).or_else(|| upload_file_name(url));
    let name = match encoded {
        Some(encoded) => title::to_display(&title::percent_decode(encoded)),
        None => {
            let page = title::title_from_url(url)?;
            title::strip_namespace(&page, &FILE_NAMESPACES)?.to_string()
        }
    };
    let name = strip_file_prefix(&name).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The file page URL for a `Special:FilePath` (or `Special:Redirect/file`)
/// URL, on the same host and keeping the name's encoding:
/// `…/wiki/Special:FilePath/A%20B.jpg?width=300` gives `…/wiki/File:A%20B.jpg`.
pub fn file_page_url_from_file_path_url(url: &str) -> Option<String> {
    let name = special_file_name(url)?;
    let (base, _) = url.split_once("/wiki/Special:")?;
    Some(format!("{base}/wiki/File:{name}"))
}

/// The DB-key form of a file name as Commons stores it: no prefix, first
/// letter uppercase, underscores.
fn db_key(file: &str) -> String {
    title::to_db_key(&title::normalize(
        strip_file_prefix(file),
        Case::FirstLetter,
    ))
}

/// `a/ab`: the first one and two hex digits of the MD5 of the DB key.
fn hash_path(db_key: &str) -> String {
    let hash = format!("{:x}", md5::compute(db_key.as_bytes()));
    let first = hash.get(..1).unwrap_or_default();
    let first_two = hash.get(..2).unwrap_or_default();
    format!("{first}/{first_two}")
}

fn special_file_url(server: &str, special: &str, file: &str, width: Option<u32>) -> String {
    let mut url = title::page_url(server, &format!("Special:{special}/{}", db_key(file)));
    if let Some(width) = width {
        url.push_str(&format!("?width={width}"));
    }
    url
}

/// The still encoded name in a `Special:FilePath/…` or
/// `Special:Redirect/file/…` URL.
fn special_file_name(url: &str) -> Option<&str> {
    let (_, rest) = url
        .split_once("/wiki/Special:FilePath/")
        .or_else(|| url.split_once("/wiki/Special:Redirect/file/"))?;
    let name = rest.split(['?', '#']).next().unwrap_or(rest);
    (!name.is_empty()).then_some(name)
}

/// The still encoded name in an `upload.wikimedia.org` URL of an original
/// (`/wikipedia/commons/a/ab/Name`) or a thumbnail
/// (`/wikipedia/commons/thumb/a/ab/Name/120px-Name`).
fn upload_file_name(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.strip_prefix("upload.wikimedia.org/")?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let segments: Vec<&str> = path.split('/').collect();
    // Skip `<family>/<project>/` and an optional `thumb/`.
    let hashed = match segments.get(2..)? {
        ["thumb", rest @ ..] => rest,
        rest => rest,
    };
    match hashed {
        [first, first_two, name, ..]
            if first.len() == 1
                && first_two.len() == 2
                && first_two.starts_with(first)
                && first_two.bytes().all(|b| b.is_ascii_hexdigit())
                && !name.is_empty() =>
        {
            Some(name)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_urls_use_the_md5_hash_path() {
        assert_eq!(
            upload_url("Example.jpg"),
            "https://upload.wikimedia.org/wikipedia/commons/a/a9/Example.jpg"
        );
        assert_eq!(
            upload_url("File:Albert Einstein Head.jpg"),
            "https://upload.wikimedia.org/wikipedia/commons/d/d3/Albert_Einstein_Head.jpg"
        );
        // Normalised first: prefix, case of the first letter, spaces.
        assert_eq!(upload_url("image:example.jpg"), upload_url("Example.jpg"));
        assert_eq!(
            upload_url("Köln Hbf.jpg"),
            "https://upload.wikimedia.org/wikipedia/commons/9/9b/K%C3%B6ln_Hbf.jpg"
        );
    }

    #[test]
    fn thumbnail_urls() {
        assert_eq!(
            thumb_url("Example.jpg", 120),
            "https://upload.wikimedia.org/wikipedia/commons/thumb/a/a9/Example.jpg/120px-Example.jpg"
        );
        assert_eq!(
            thumb_url("Example.svg", 250),
            "https://upload.wikimedia.org/wikipedia/commons/thumb/8/84/Example.svg/250px-Example.svg.png"
        );
        let pdf = thumb_url("Doc.pdf", 120);
        assert!(pdf.ends_with("/Doc.pdf/page1-120px-Doc.pdf.jpg"), "{pdf}");
        let tif = thumb_url("Scan.tif", 120);
        assert!(
            tif.ends_with("/Scan.tif/lossy-page1-120px-Scan.tif.jpg"),
            "{tif}"
        );
        let video = thumb_url("Clip.webm", 320);
        assert!(
            video.ends_with("/Clip.webm/320px--Clip.webm.jpg"),
            "{video}"
        );
    }

    #[test]
    fn long_names_get_abbreviated_thumbnails() {
        let name = format!("{}.jpg", "A".repeat(200));
        let url = thumb_url(&name, 120);
        assert!(url.ends_with("/120px-thumbnail.jpg"), "{url}");
        assert!(url.contains(&format!("/{name}/")));
    }

    #[test]
    fn special_page_urls() {
        assert_eq!(
            file_path_url("File:A Person.jpg", Some(300)),
            "https://commons.wikimedia.org/wiki/Special:FilePath/A_Person.jpg?width=300"
        );
        assert_eq!(
            file_path_url("Example.jpg", None),
            "https://commons.wikimedia.org/wiki/Special:FilePath/Example.jpg"
        );
        assert_eq!(
            redirect_file_url("https://de.wikipedia.org", "Köln.jpg", Some(120)),
            "https://de.wikipedia.org/wiki/Special:Redirect/file/K%C3%B6ln.jpg?width=120"
        );
        assert_eq!(
            file_page_url("example (2).jpg"),
            "https://commons.wikimedia.org/wiki/File:Example_(2).jpg"
        );
    }

    #[test]
    fn standard_widths() {
        assert_eq!(standard_thumb_width(0), 20);
        assert_eq!(standard_thumb_width(120), 120);
        assert_eq!(standard_thumb_width(121), 250);
        assert_eq!(standard_thumb_width(10_000), 3840);
    }

    #[test]
    fn strip_prefix() {
        assert_eq!(strip_file_prefix("File:Foo.jpg"), "Foo.jpg");
        assert_eq!(strip_file_prefix("image: Foo.jpg"), "Foo.jpg");
        assert_eq!(strip_file_prefix("Foo.jpg"), "Foo.jpg");
        assert_eq!(strip_file_prefix("Category:Foo"), "Category:Foo");
    }

    #[test]
    fn file_names_from_urls() {
        let name = |url: &str| file_name_from_url(url);
        let expected = Some("Albert Einstein Head.jpg".to_string());
        for url in [
            "http://commons.wikimedia.org/wiki/Special:FilePath/Albert%20Einstein%20Head.jpg",
            "https://commons.wikimedia.org/wiki/Special:FilePath/Albert_Einstein_Head.jpg?width=300",
            "https://en.wikipedia.org/wiki/Special:Redirect/file/Albert_Einstein_Head.jpg",
            "https://commons.wikimedia.org/wiki/File:Albert_Einstein_Head.jpg",
            "https://commons.wikimedia.org/w/index.php?title=File:Albert_Einstein_Head.jpg&oldid=1",
            "https://upload.wikimedia.org/wikipedia/commons/d/d3/Albert_Einstein_Head.jpg",
            "https://upload.wikimedia.org/wikipedia/commons/thumb/d/d3/Albert_Einstein_Head.jpg/120px-Albert_Einstein_Head.jpg",
        ] {
            assert_eq!(name(url), expected, "{url}");
        }
        assert_eq!(
            name("https://upload.wikimedia.org/wikipedia/commons/9/9b/K%C3%B6ln_Hbf.jpg")
                .as_deref(),
            Some("Köln Hbf.jpg")
        );
        assert_eq!(name("https://commons.wikimedia.org/wiki/Main_Page"), None);
        assert_eq!(
            name("https://upload.wikimedia.org/wikipedia/commons/zz/Foo.jpg"),
            None
        );
        assert_eq!(name("https://example.org/Foo.jpg"), None);
        assert_eq!(
            name("https://commons.wikimedia.org/wiki/Special:FilePath/"),
            None
        );
    }

    #[test]
    fn urls_round_trip_to_names() {
        for file in ["Example.jpg", "Köln Hbf (2).jpg", "A&B.png"] {
            for url in [
                upload_url(file),
                thumb_url(file, 120),
                file_path_url(file, Some(120)),
                file_page_url(file),
            ] {
                assert_eq!(file_name_from_url(&url).as_deref(), Some(file), "{url}");
            }
        }
    }

    #[test]
    fn file_page_from_file_path() {
        assert_eq!(
            file_page_url_from_file_path_url(
                "https://commons.wikimedia.org/wiki/Special:FilePath/A%20Person.jpg?width=300"
            )
            .as_deref(),
            Some("https://commons.wikimedia.org/wiki/File:A%20Person.jpg")
        );
        assert_eq!(
            file_page_url_from_file_path_url("https://commons.wikimedia.org/wiki/File:X.jpg"),
            None
        );
    }
}
