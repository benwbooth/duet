//! Built-in sources of free, legally downloadable books: Project Gutenberg (via Gutendex),
//! Standard Ebooks, LibriVox and Internet Archive public-domain texts.

use duet_common::{AssetKind, DownloadTarget, FreeSources, SearchResult};
use kuchikiki::traits::TendrilSink;
use serde::Deserialize;

/// How long one source may take before the others are shown without it.
const SOURCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// Runs one source with a time limit and a plain-language error naming it.
async fn source(
    name: &str,
    on: bool,
    f: impl std::future::Future<Output = anyhow::Result<Vec<SearchResult>>>,
) -> Option<anyhow::Result<Vec<SearchResult>>> {
    if !on {
        return None;
    }
    Some(match tokio::time::timeout(SOURCE_TIMEOUT, f).await {
        Err(_) => Err(anyhow::anyhow!("{name} didn't answer in time")),
        Ok(Err(e)) => {
            let status = e.chain().find_map(|c| c.downcast_ref::<reqwest::Error>()).and_then(|r| r.status());
            Err(match status {
                Some(s) if s.is_server_error() => anyhow::anyhow!("{name} is unavailable right now (HTTP {})", s.as_u16()),
                _ => e.context(format!("{name} search failed")),
            })
        }
        Ok(ok) => ok,
    })
}

pub async fn search(http: &reqwest::Client, enabled: &FreeSources, q: &str, kind: AssetKind) -> Vec<anyhow::Result<Vec<SearchResult>>> {
    match kind {
        AssetKind::Ebook => {
            let (s, g, ia) = tokio::join!(
                source("Standard Ebooks", enabled.standard_ebooks, standard_ebooks(http, q)),
                source("Project Gutenberg", enabled.gutenberg, gutenberg(http, q)),
                source("Internet Archive", enabled.internet_archive, internet_archive(http, q)),
            );
            [s, g, ia].into_iter().flatten().collect()
        }
        AssetKind::Audiobook => source("LibriVox", enabled.librivox, librivox(http, q)).await.into_iter().collect(),
    }
}

/// "Austen, Jane" -> "Jane Austen"
fn first_last(name: &str) -> String {
    match name.split_once(", ") {
        Some((last, first)) => format!("{first} {last}"),
        None => name.to_string(),
    }
}

fn safe_filename(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || " -_.,'()".contains(c) { c } else { '_' }).collect::<String>().trim().to_string()
}

async fn gutenberg(http: &reqwest::Client, q: &str) -> anyhow::Result<Vec<SearchResult>> {
    #[derive(Deserialize)]
    struct Resp {
        results: Vec<Book>,
    }
    #[derive(Deserialize)]
    struct Book {
        id: i64,
        title: String,
        authors: Vec<Person>,
        formats: std::collections::HashMap<String, String>,
    }
    #[derive(Deserialize)]
    struct Person {
        name: String,
    }
    let resp: Resp = http
        .get("https://gutendex.com/books/")
        .query(&[("search", q)])
        .send()
        .await
        ?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp
        .results
        .into_iter()
        .filter_map(|b| {
            let author = b.authors.first().map(|a| first_last(&a.name));
            let url = b.formats.get("application/epub+zip")?.clone();
            let filename = safe_filename(&format!("{} - {}.epub", author.as_deref().unwrap_or("Unknown"), b.title));
            Some(SearchResult {
                source: "Project Gutenberg".into(),
                title: b.title,
                author,
                kind: AssetKind::Ebook,
                size: None,
                seeders: None,
                download: DownloadTarget::Direct { url, filename },
                info_url: Some(format!("https://www.gutenberg.org/ebooks/{}", b.id)),
                format: Some("epub".into()),
            })
        })
        .collect())
}

async fn standard_ebooks(http: &reqwest::Client, q: &str) -> anyhow::Result<Vec<SearchResult>> {
    let html = http
        .get("https://standardebooks.org/ebooks")
        .query(&[("query", q)])
        .send()
        .await
        ?
        .error_for_status()?
        .text()
        .await?;
    let doc = kuchikiki::parse_html().one(html).document_node;
    let mut out: Vec<SearchResult> = Vec::new();
    for book in doc.select("li[typeof='schema:Book']").into_iter().flatten() {
        let node = book.as_node();
        let Some(link) = node.select_first("a[href^='/ebooks/']").ok() else { continue };
        let href = link.attributes.borrow().get("href").unwrap_or("").to_string();
        let title = node.select_first("[property='schema:name']").map(|n| n.text_contents()).unwrap_or_else(|_| link.text_contents());
        let author = node.select_first("[typeof='schema:Person'] [property='schema:name']").ok().map(|n| n.text_contents());
        // /ebooks/jane-austen/pride-and-prejudice -> .../downloads/jane-austen_pride-and-prejudice.epub
        let slug = href.trim_start_matches("/ebooks/").trim_end_matches('/').replace('/', "_");
        if slug.is_empty() || out.iter().any(|r| r.info_url.as_deref() == Some(&href)) {
            continue;
        }
        let url = format!("https://standardebooks.org{href}/downloads/{slug}.epub?source=download");
        let filename = safe_filename(&format!("{} - {}.epub", author.as_deref().unwrap_or("Unknown"), title.trim()));
        out.push(SearchResult {
            source: "Standard Ebooks".into(),
            title: title.trim().to_string(),
            author: author.map(|a| a.trim().to_string()),
            kind: AssetKind::Ebook,
            size: None,
            seeders: None,
            download: DownloadTarget::Direct { url, filename },
            info_url: Some(format!("https://standardebooks.org{href}")),
            format: Some("epub".into()),
        });
    }
    Ok(out)
}

async fn internet_archive(http: &reqwest::Client, q: &str) -> anyhow::Result<Vec<SearchResult>> {
    #[derive(Deserialize)]
    struct Resp {
        response: Docs,
    }
    #[derive(Deserialize)]
    struct Docs {
        docs: Vec<Doc>,
    }
    #[derive(Deserialize)]
    struct Doc {
        identifier: String,
        title: Option<String>,
        creator: Option<serde_json::Value>,
    }
    // Public-domain marked texts and Gutenberg mirrors only.
    let query = format!(
        "title:({q}) AND mediatype:(texts) AND (collection:(gutenberg) OR licenseurl:(*publicdomain*))"
    );
    let resp: Resp = http
        .get("https://archive.org/advancedsearch.php")
        .query(&[("q", query.as_str()), ("fl[]", "identifier"), ("fl[]", "title"), ("fl[]", "creator"), ("rows", "25"), ("output", "json")])
        .send()
        .await
        ?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp
        .response
        .docs
        .into_iter()
        .map(|d| {
            let author = match d.creator {
                Some(serde_json::Value::String(s)) => Some(first_last(&s)),
                Some(serde_json::Value::Array(a)) => a.first().and_then(|v| v.as_str()).map(first_last),
                _ => None,
            };
            let title = d.title.unwrap_or_else(|| d.identifier.clone());
            SearchResult {
                source: "Internet Archive".into(),
                title: title.clone(),
                author: author.clone(),
                kind: AssetKind::Ebook,
                size: None,
                seeders: None,
                // The downloader picks the best file (EPUB, then PDF, then text) from the item's metadata.
                download: DownloadTarget::Direct {
                    url: format!("https://archive.org/metadata/{}", d.identifier),
                    filename: safe_filename(&format!("{} - {}", author.as_deref().unwrap_or("Unknown"), title)),
                },
                info_url: Some(format!("https://archive.org/details/{}", d.identifier)),
                format: None,
            }
        })
        .collect())
}

async fn librivox(http: &reqwest::Client, q: &str) -> anyhow::Result<Vec<SearchResult>> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        books: Vec<Book>,
    }
    #[derive(Deserialize)]
    struct Book {
        title: String,
        #[serde(default)]
        authors: Vec<Person>,
        url_zip_file: Option<String>,
        url_librivox: Option<String>,
    }
    #[derive(Deserialize)]
    struct Person {
        first_name: String,
        last_name: String,
    }
    let resp = http
        .get("https://librivox.org/api/feed/audiobooks/")
        .query(&[("title", format!("^{q}").as_str()), ("format", "json"), ("limit", "25")])
        .send()
        .await
        ?;
    // LibriVox answers 404 with an error object when nothing matches.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let resp: Resp = resp.error_for_status()?.json().await?;
    Ok(resp
        .books
        .into_iter()
        .filter_map(|b| {
            let url = b.url_zip_file.filter(|u| !u.is_empty())?;
            let author = b.authors.first().map(|a| format!("{} {}", a.first_name, a.last_name).trim().to_string());
            let filename = safe_filename(&format!("{} - {}.zip", author.as_deref().unwrap_or("Unknown"), b.title));
            Some(SearchResult {
                source: "LibriVox".into(),
                title: b.title,
                author,
                kind: AssetKind::Audiobook,
                size: None,
                seeders: None,
                download: DownloadTarget::Direct { url, filename },
                info_url: b.url_librivox,
                format: Some("mp3".into()),
            })
        })
        .collect())
}
