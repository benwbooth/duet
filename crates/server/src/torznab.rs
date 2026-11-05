//! Torznab search (the API Prowlarr and Jackett expose), against indexers the user configures.

use anyhow::Context;
use duet_common::{AssetKind, DownloadTarget, Indexer, SearchResult};
use quick_xml::{events::Event, Reader};

pub async fn search(http: &reqwest::Client, ix: &Indexer, query: &str, kind: AssetKind) -> anyhow::Result<Vec<SearchResult>> {
    let cats = match kind {
        AssetKind::Ebook => &ix.ebook_categories,
        AssetKind::Audiobook => &ix.audiobook_categories,
    };
    let mut params = vec![("t", "search"), ("q", query)];
    if !ix.api_key.is_empty() {
        params.push(("apikey", &ix.api_key));
    }
    if !cats.trim().is_empty() {
        params.push(("cat", cats.trim()));
    }
    let body = http
        .get(&ix.url)
        .query(&params)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("searching {}", ix.name))?
        .error_for_status()?
        .text()
        .await?;
    parse(&body, &ix.name, kind)
}

#[derive(Default)]
struct Item {
    title: String,
    link: String,
    size: Option<i64>,
    seeders: Option<i64>,
    magnet: Option<String>,
    enclosure: Option<String>,
    comments: Option<String>,
}

pub fn parse(xml: &str, source: &str, kind: AssetKind) -> anyhow::Result<Vec<SearchResult>> {
    let mut reader = Reader::from_str(xml);
        let mut out = Vec::new();
    let mut item: Option<Item> = None;
    let mut text = String::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                let attr = |k: &str| {
                    e.attributes().flatten().find(|a| a.key.local_name().as_ref() == k.as_bytes()).and_then(|a| a.unescape_value().ok()).map(|v| v.to_string())
                };
                match name.as_str() {
                    "item" => item = Some(Item::default()),
                    "enclosure" => {
                        if let Some(it) = item.as_mut() {
                            it.enclosure = attr("url");
                            if it.size.is_none() {
                                it.size = attr("length").and_then(|v| v.parse().ok());
                            }
                        }
                    }
                    "attr" => {
                        if let (Some(it), Some(n), Some(v)) = (item.as_mut(), attr("name"), attr("value")) {
                            match n.as_str() {
                                "seeders" => it.seeders = v.parse().ok(),
                                "size" => it.size = v.parse().ok(),
                                "magneturl" => it.magnet = Some(v),
                                _ => {}
                            }
                        }
                    }
                    _ => text.clear(),
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
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::End(e) => {
                let v = std::mem::take(&mut text).trim().to_string();
                if let Some(it) = item.as_mut() {
                    match e.local_name().as_ref() {
                        b"title" => it.title = v,
                        b"link" => it.link = v,
                        b"size" => it.size = v.parse().ok().or(it.size),
                        b"comments" => it.comments = Some(v),
                        _ => {}
                    }
                }
                if e.local_name().as_ref() == b"item"
                    && let Some(it) = item.take() {
                        let url = it.magnet.or(it.enclosure).unwrap_or(it.link);
                        if !url.is_empty() {
                            out.push(SearchResult {
                                source: source.to_string(),
                                title: it.title,
                                author: None,
                                kind,
                                size: it.size,
                                seeders: it.seeders,
                                download: DownloadTarget::Torrent { url },
                                info_url: it.comments,
                                format: None,
                            });
                        }
                    }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_items() {
        let xml = r#"<?xml version="1.0"?><rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
            <item><title>Some Book</title><link>http://x/dl/1</link><size>1234</size>
            <enclosure url="http://x/dl/1.torrent" length="1234" type="application/x-bittorrent"/>
            <torznab:attr name="seeders" value="7"/></item></channel></rss>"#;
        let r = parse(xml, "test", AssetKind::Ebook).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].seeders, Some(7));
        assert_eq!(r[0].download, DownloadTarget::Torrent { url: "http://x/dl/1.torrent".into() });
    }
}
