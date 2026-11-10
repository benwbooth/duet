//! Built-in sources: Project Gutenberg (via Gutendex), Standard Ebooks,
//! LibriVox and Internet Archive public-domain texts, plus Anna's Archive
//! ebooks and Audiobook Bay audiobooks.

use duet_common::{AnnasArchiveSettings, AssetKind, AudiobookBaySettings, DownloadTarget, FreeSources, LibGenSettings, SearchResult};
use kuchikiki::traits::TendrilSink;
use serde::Deserialize;

use crate::{annas_archive::AnnasArchive, audiobookbay::AudiobookBay, libgen::LibGen};

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

/// Tries each mirror (your configured primary, then the Open SLUM known
/// set) until one answers with results. The first working mirror wins, and
/// the whole loop is bounded (40s) so a slow mirror never blocks search.
async fn failover<'a>(
    name: &str,
    mirrors: Vec<String>,
    mut attempt: impl FnMut(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Vec<SearchResult>>> + 'a + Send>>,
) -> Option<anyhow::Result<Vec<SearchResult>>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
    let mut last_err: Option<anyhow::Error> = None;
    // A bot-check answer is the most actionable failure: it tells the UI to
    // pop the browser. Never let a duller mirror error mask it.
    let mut need_check: Option<anyhow::Error> = None;
    for mirror in &mirrors {
        if std::time::Instant::now() > deadline {
            break;
        }
        match attempt(mirror.clone()).await {
            Ok(v) if !v.is_empty() => return Some(Ok(v)),
            Ok(_) => last_err = Some(anyhow::anyhow!("{name} answered but had nothing")),
            Err(e) if e.to_string().contains("BOT_CHECK") => need_check = Some(e),
            Err(e) => last_err = Some(e),
        }
    }
    Some(Err(need_check.or(last_err).unwrap_or_else(|| anyhow::anyhow!("{name}: no mirrors"))))
}

/// Effective Anna's Archive session cookies: harvested from the remote
/// Firefox profile first (fresh after each manual solve), else the pasted
/// `browser_cookies` fallback, else empty.
async fn aa_cookies(aa: &AnnasArchiveSettings) -> String {
    if let Some(h) = crate::annas_archive::harvest_browser_cookies(&aa.browser_profile_dir).await {
        if !h.trim().is_empty() {
            return h;
        }
    }
    aa.browser_cookies.trim().to_string()
}

/// Drops OR-match noise from free-source results: every significant query
/// word must appear (whole word, plural-tolerant) in the title or author.
/// Queries with no significant words match nothing (instead of everything).
/// Indexer releases are left alone (the indexer did its own matching).
fn relevant(results: Vec<SearchResult>, q: &str) -> Vec<SearchResult> {
    use duet_common::text::{matches_query, significant_words};
    if significant_words(q).is_empty() {
        return Vec::new();
    }
    results.into_iter().filter(|r| matches_query(&r.title, &r.author, q)).collect()
}

/// Searches one built-in source by its id ("gutenberg", "standard_ebooks", "internet_archive",
/// "librivox", "annas_archive", "libgen", "audiobookbay"), if it's switched on.
pub async fn search_one(
    http: &reqwest::Client,
    http_cookies: &reqwest::Client,
    annas: &AnnasArchive,
    enabled: &FreeSources,
    aa: &AnnasArchiveSettings,
    libgen: &LibGenSettings,
    abb: &AudiobookBaySettings,
    id: &str,
    q: &str,
) -> Option<anyhow::Result<Vec<SearchResult>>> {
    match id {
        "gutenberg" => source("Project Gutenberg", enabled.gutenberg, gutenberg(http, q)).await,
        "standard_ebooks" => source("Standard Ebooks", enabled.standard_ebooks, standard_ebooks(http, q)).await,
        "internet_archive" => source("Internet Archive", enabled.internet_archive, internet_archive(http, q)).await,
        "librivox" => source("LibriVox", enabled.librivox, librivox(http, q)).await,
        "annas_archive" => {
            if !enabled.annas_archive {
                None
            } else {
                let known = crate::mirrors::annas_archive_list();
                let key = aa.secret_key.clone();
                let cookies = aa_cookies(aa).await;
                let wd = aa.webdriver_url.clone();
                let q = q.to_string();
                failover("Anna's Archive", crate::mirrors::ordered(&aa.mirror, &known), move |m| {
                    let key = key.clone();
                    let cookies = cookies.clone();
                    let wd = wd.clone();
                    let q = q.clone();
                    Box::pin(async move {
                        annas.search(http, m.as_str(), &wd, &key, &cookies, &q).await
                    })
                }).await
            }
        }
        "libgen" => {
            if !enabled.libgen {
                None
            } else {
                let known = crate::mirrors::libgen_list();
                failover("LibGen", crate::mirrors::ordered(&libgen.base_url, &known), |m| Box::pin(async move {
                    LibGen::search(http, m.as_str(), q).await
                })).await
            }
        }
        "audiobookbay" => {
            if !enabled.audiobookbay {
                None
            } else {
                failover("Audiobook Bay", crate::mirrors::ordered(&abb.base_url, crate::mirrors::AUDIOBOOK_BAY), |m| Box::pin(async move {
                    AudiobookBay::search(http_cookies, m.as_str(), q).await
                })).await
            }
        }
        _ => None,
    }
    .map(|r| r.map(|v| relevant(v, q)))
}

pub async fn search(
    http: &reqwest::Client,
    http_cookies: &reqwest::Client,
    annas: &AnnasArchive,
    enabled: &FreeSources,
    aa: &AnnasArchiveSettings,
    libgen: &LibGenSettings,
    abb: &AudiobookBaySettings,
    q: &str,
    kind: AssetKind,
) -> Vec<anyhow::Result<Vec<SearchResult>>> {
    match kind {
        AssetKind::Ebook => {
            let annas_known = crate::mirrors::annas_archive_list();
            let libgen_known = crate::mirrors::libgen_list();
            let annas_search = failover("Anna's Archive", crate::mirrors::ordered(&aa.mirror, &annas_known), move |m| Box::pin(async move {
                let key = aa.secret_key.clone();
                let cookies = aa_cookies(aa).await;
                let wd = aa.webdriver_url.clone();
                let q = q.to_string();
                annas.search(http, m.as_str(), &wd, &key, &cookies, &q).await
            }));
            let libgen_search = failover("LibGen", crate::mirrors::ordered(&libgen.base_url, &libgen_known), |m| Box::pin(async move {
                LibGen::search(http, m.as_str(), q).await
            }));
            let (s, g, ia, annas, lg) = tokio::join!(
                source("Standard Ebooks", enabled.standard_ebooks, standard_ebooks(http, q)),
                source("Project Gutenberg", enabled.gutenberg, gutenberg(http, q)),
                source("Internet Archive", enabled.internet_archive, internet_archive(http, q)),
                async { if enabled.annas_archive { annas_search.await } else { None } },
                async { if enabled.libgen { libgen_search.await } else { None } },
            );
            [s, g, ia, annas, lg].into_iter().flatten().map(|r| r.map(|v| relevant(v, q))).collect()
        }
        AssetKind::Audiobook => {
            let abb_search = failover("Audiobook Bay", crate::mirrors::ordered(&abb.base_url, crate::mirrors::AUDIOBOOK_BAY), |m| Box::pin(async move {
                AudiobookBay::search(http_cookies, m.as_str(), q).await
            }));
            let (lv, ab) = tokio::join!(
                source("LibriVox", enabled.librivox, librivox(http, q)),
                async { if enabled.audiobookbay { abb_search.await } else { None } },
            );
            [lv, ab].into_iter().flatten().map(|r| r.map(|v| relevant(v, q))).collect()
        }
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

/// Project Gutenberg's own OPDS search (faster and more reliable than third-party mirrors).
async fn gutenberg(http: &reqwest::Client, q: &str) -> anyhow::Result<Vec<SearchResult>> {
    use quick_xml::{Reader, events::Event};
    let xml = http
        .get("https://www.gutenberg.org/ebooks/search.opds/")
        .query(&[("query", q)])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let mut reader = Reader::from_str(&xml);
    let (mut out, mut text) = (Vec::new(), String::new());
    let (mut in_entry, mut id, mut title, mut author) = (false, String::new(), String::new(), String::new());
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                text.clear();
                if e.local_name().as_ref() == b"entry" {
                    in_entry = true;
                    (id, title, author) = (String::new(), String::new(), String::new());
                }
            }
            Event::Text(t) => text.push_str(&t.xml_content()?),
            Event::GeneralRef(r) => {
                if let Some(c) = r.resolve_char_ref()? {
                    text.push(c);
                } else if let Some(v) = quick_xml::escape::resolve_xml_entity(&r.decode()?) {
                    text.push_str(v);
                }
            }
            Event::End(e) if in_entry => {
                let v = std::mem::take(&mut text).trim().to_string();
                match e.local_name().as_ref() {
                    b"id" => id = v,
                    b"title" => title = v,
                    b"content" => author = v,
                    b"entry" => {
                        in_entry = false;
                        // Book entries look like https://www.gutenberg.org/ebooks/11.opds.
                        let num = id.rsplit('/').next().and_then(|n| n.strip_suffix(".opds")).filter(|n| n.chars().all(|c| c.is_ascii_digit()));
                        if let Some(num) = num.filter(|n| !n.is_empty()) {
                            let author = (!author.is_empty()).then(|| first_last(&author));
                            let filename = safe_filename(&format!("{} - {}.epub", author.as_deref().unwrap_or("Unknown"), title));
                            out.push(SearchResult {
                                source: "Project Gutenberg".into(),
                                title: title.clone(),
                                author,
                                kind: AssetKind::Ebook,
                                size: None,
                                seeders: None,
                                download: DownloadTarget::Direct {
                                    url: format!("https://www.gutenberg.org/ebooks/{num}.epub3.images"),
                                    filename,
                                },
                                info_url: Some(format!("https://www.gutenberg.org/ebooks/{num}")),
                                format: Some("epub".into()),
                                aacid: None,
                                torrent_url: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Event::End(_) => text.clear(),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
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
    for book in doc.select("li[typeof='schema:Book'], article[typeof='schema:Book']").into_iter().flatten() {
        let node = book.as_node();
        // Placeholders are upcoming books with no files yet — skip them,
        // or every download 404s.
        if let Some(el) = node.as_element() {
            let cls = el.attributes.borrow().get("class").unwrap_or("").to_string();
            if cls.split_whitespace().any(|c| c == "ebook-placeholder") {
                continue;
            }
        }
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
            aacid: None,
            torrent_url: None,
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
                aacid: None,
                torrent_url: None,
            }
        })
        .collect())
}

/// The words of a query that matter for matching titles.
fn significant_words(q: &str) -> Vec<String> {
    const SMALL: [&str; 10] = ["the", "a", "an", "of", "in", "and", "to", "on", "at", "for"];
    q.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| !w.is_empty() && !SMALL.contains(&w.as_str()))
        .collect()
}

async fn librivox(http: &reqwest::Client, q: &str) -> anyhow::Result<Vec<SearchResult>> {
    let words = significant_words(q);
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
        // LibriVox only matches titles by prefix, so search by the first real word and keep the
        // books whose titles have all the words ("alice wonderland" finds "Alice's Adventures in Wonderland").
        .query(&[("title", format!("^{}", words.first().map(String::as_str).unwrap_or(q)).as_str()), ("format", "json"), ("limit", "200")])
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
        .filter(|b| {
            let title = b.title.to_lowercase();
            words.iter().all(|w| title.contains(w.as_str()))
        })
        .take(25)
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
                aacid: None,
                torrent_url: None,
            })
        })
        .collect())
}
