//! A book's details looked up online, once: when it was first published, a short description,
//! its subjects and how many pages it runs to. From Open Library (the year a work first came
//! out, rather than the date of whichever edition is on disk) and Google Books (descriptions),
//! both free and keyless. What the files themselves say comes first; this fills the gaps.

use anyhow::Context;
use serde_json::Value;

use crate::State;

const UA: &str = "duet-reader/0.1 (personal library)";

/// Every book not looked up yet, one after another (politely: a second apart).
pub async fn backfill(state: &State) {
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT id FROM works WHERE looked_up_at IS NULL").fetch_all(&state.db).await.unwrap_or_default();
    for (id,) in rows {
        if let Err(e) = enrich(state, id).await {
            tracing::warn!("looking up book {id}: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Looks one book up and fills in what's missing.
pub async fn enrich(state: &State, id: i64) -> anyhow::Result<()> {
    let (title, author, description): (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT title, author, description FROM works WHERE id = ?").bind(id).fetch_one(&state.db).await?;
    let author = author.unwrap_or_default();
    // (The title without a subtitle or series note, which search engines trip over.)
    let short_title = title.split([':', '(', '[']).next().unwrap_or(&title).trim().to_string();

    let ol = open_library(state, &short_title, &author).await.unwrap_or_default();
    let gb = google_books(state, &short_title, &author).await.unwrap_or_default();

    // The year the work first came out.
    let year = ol.year.or(gb.year);
    // A description: the publisher's (Google Books) or Open Library's, unless the file had one.
    let desc = description
        .filter(|d| d.trim().len() > 40)
        .or(gb.description.clone())
        .or(ol.description.clone())
        .map(|d| clean(&d));
    let mut subjects: Vec<String> = Vec::new();
    for s in gb.subjects.iter().chain(ol.subjects.iter()) {
        // (One way of writing them: "JUVENILE FICTION" and "classics" alike as "Juvenile fiction".)
        let lower = s.trim().to_lowercase();
        let mut c = lower.chars();
        let s = match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => continue,
        };
        if s.len() < 40 && !subjects.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
            subjects.push(s);
        }
        if subjects.len() >= 6 {
            break;
        }
    }
    let pages = gb.pages.or(ol.pages);
    sqlx::query(
        "UPDATE works SET year = COALESCE(?, year), description = COALESCE(?, description), subjects = ?, pages = COALESCE(?, pages),
         looked_up_at = datetime('now') WHERE id = ?",
    )
    .bind(year)
    .bind(desc)
    .bind(if subjects.is_empty() { None } else { Some(subjects.join("\n")) })
    .bind(pages)
    .bind(id)
    .execute(&state.db)
    .await?;
    Ok(())
}

#[derive(Default)]
struct Found {
    year: Option<i32>,
    description: Option<String>,
    subjects: Vec<String>,
    pages: Option<i64>,
}

async fn get_json(state: &State, url: &str) -> anyhow::Result<Value> {
    Ok(state
        .http
        .get(url)
        .header("User-Agent", UA)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn q(s: &str) -> String {
    encode(s)
}

/// Percent-encoding for a query value.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn open_library(state: &State, title: &str, author: &str) -> anyhow::Result<Found> {
    let url = format!(
        "https://openlibrary.org/search.json?title={}&author={}&limit=1&fields=key,first_publish_year,subject,number_of_pages_median",
        q(title),
        q(author)
    );
    let v = get_json(state, &url).await?;
    let doc = v["docs"].get(0).context("not in Open Library")?;
    let mut f = Found {
        year: doc["first_publish_year"].as_i64().map(|y| y as i32),
        subjects: doc["subject"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).take(8).collect()).unwrap_or_default(),
        pages: doc["number_of_pages_median"].as_i64(),
        ..Default::default()
    };
    // The work's own description.
    if let Some(key) = doc["key"].as_str() {
        if let Ok(w) = get_json(state, &format!("https://openlibrary.org{key}.json")).await {
            f.description = match &w["description"] {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("value").and_then(|v| v.as_str()).map(str::to_string),
                _ => None,
            };
        }
    }
    Ok(f)
}

async fn google_books(state: &State, title: &str, author: &str) -> anyhow::Result<Found> {
    let mut query = format!("intitle:{}", title);
    if !author.is_empty() {
        query.push_str(&format!(" inauthor:{author}"));
    }
    let url = format!("https://www.googleapis.com/books/v1/volumes?q={}&maxResults=3&printType=books", q(&query));
    let v = get_json(state, &url).await?;
    let items = v["items"].as_array().context("not in Google Books")?;
    // The first with a description.
    let info = items.iter().map(|i| &i["volumeInfo"]).find(|i| i["description"].is_string()).or(items.first().map(|i| &i["volumeInfo"])).context("no volume")?;
    Ok(Found {
        year: info["publishedDate"].as_str().and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()),
        description: info["description"].as_str().map(str::to_string),
        subjects: info["categories"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        pages: info["pageCount"].as_i64(),
    })
}

/// A description as plain prose: tags and HTML entities out, and not too long.
fn clean(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // (Open Library's often end with a source line or a list of editions.)
    let out = out.split("----------").next().unwrap_or(&out).trim().to_string();
    if out.chars().count() > 1200 {
        let cut: String = out.chars().take(1200).collect();
        match cut.rfind(". ") {
            Some(i) => cut[..=i].to_string(),
            None => cut,
        }
    } else {
        out
    }
}
