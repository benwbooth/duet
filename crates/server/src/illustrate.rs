//! Pictures for a book: an illustration director (the chat model) reads each chapter and picks
//! its scenes, one every few pages, describing each moment, and keeps a look-book of the
//! characters and one art style for the whole book, so the pictures belong together. The style
//! is the reader's to change. The pictures are drawn on the GPU (Stable Diffusion XL, through
//! the voice server, which shares the card out) ahead of the reader, and kept on disk.
//!
//! A picture belongs to a sentence, not a page: pages come and go as the text is laid out
//! again, and the picture goes before the paragraph where its scene happens.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{State, book, db};

/// One picture: the moment it shows, where in the chapter, and what it's drawn from.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scene {
    /// The sentence the scene happens at (the picture goes before its paragraph).
    pub sentence: usize,
    pub caption: String,
    /// What's in the picture (who, where, doing what, the light, the framing).
    pub scene: String,
    /// The characters in it, by name (their look comes from the look-book).
    #[serde(default)]
    pub characters: Vec<String>,
    pub seed: u64,
}

/// The book's art style and its characters' looks: the same for every picture.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Style {
    pub style: String,
    #[serde(default)]
    pub characters: BTreeMap<String, String>,
}

fn dir(state: &State, asset: i64) -> PathBuf {
    state.data_dir.join("cache").join("illustrations").join(asset.to_string())
}

/// A character's portrait: what they look like in every picture (drawn once, from the
/// look-book, and then shown to the artist with each scene they're in).
pub fn portrait_path(state: &State, asset: i64, name: &str) -> PathBuf {
    dir(state, asset).join("portraits").join(format!("{}.png", slug(name)))
}

pub fn slug(name: &str) -> String {
    let s: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.trim_matches('-').to_string()
}

pub fn image_path(state: &State, asset: i64, chapter: usize, i: usize) -> PathBuf {
    dir(state, asset).join(format!("{chapter}-{i}.png"))
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Option<T> {
    serde_json::from_slice(&tokio::fs::read(path).await.ok()?).ok()
}

async fn write_json<T: Serialize>(path: &PathBuf, v: &T) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    tokio::fs::write(path, serde_json::to_vec_pretty(v)?).await?;
    Ok(())
}

fn json_in(text: &str) -> anyhow::Result<serde_json::Value> {
    let (start, end) = (text.find('{').context("no JSON in the reply")?, text.rfind('}').context("no JSON in the reply")?);
    Ok(serde_json::from_str(&text[start..=end])?)
}

async fn title_author(state: &State, asset: i64) -> anyhow::Result<(String, String)> {
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let w = db::work_detail(&state.db, a.work_id).await?.context("no such book")?;
    Ok((w.title, w.author.unwrap_or_default()))
}

async fn sentences(state: &State, asset: i64, chapter: usize) -> anyhow::Result<Vec<String>> {
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let src = book::source(&a, &state.data_dir);
    tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, chapter)).await?
}

/// The book's style, settled by the director the first time (the reader can change it).
pub async fn style(state: &State, asset: i64) -> anyhow::Result<Style> {
    let path = dir(state, asset).join("style.json");
    if let Some(s) = read_json::<Style>(&path).await.filter(|s| !s.style.is_empty()) {
        return Ok(s);
    }
    let (title, author) = title_author(state, asset).await?;
    let opening: String = sentences(state, asset, first_text_chapter(state, asset).await).await.unwrap_or_default().join(" ").chars().take(3000).collect();
    let system = "You are the art director for an illustrated edition of a book. Choose one art style for every \
        picture in it, suited to the book, its period and its readers (a Victorian children's classic: e.g. \
        'delicate pen-and-ink linework with soft watercolour washes, muted warm palette, classic storybook \
        illustration'; a modern thriller: 'moody cinematic digital painting, deep shadows, muted teal and \
        amber'). Describe it in at most 30 words, for an image generator: medium, linework, palette, mood. No \
        artist names, no text in the pictures. Reply with JSON only: {\"style\": \"<style>\"}.";
    let reply = crate::llm::chat(state, system, &[("user".into(), format!("Book: {title} by {author}\n\nOpening:\n{opening}"))]).await?;
    let style = json_in(&reply)?["style"].as_str().unwrap_or("").trim().to_string();
    anyhow::ensure!(!style.is_empty(), "no style from the director");
    let s = Style { style, characters: read_json::<Style>(&path).await.map(|s| s.characters).unwrap_or_default() };
    write_json(&path, &s).await?;
    Ok(s)
}

/// The first chapter with a real stretch of text (not the title page or contents).
async fn first_text_chapter(state: &State, asset: i64) -> usize {
    for c in 0..12 {
        if sentences(state, asset, c).await.map(|s| s.len() > 20).unwrap_or(false) {
            return c;
        }
    }
    0
}

/// The reader changes the style: kept, and every picture drawn again in it.
pub async fn set_style(state: &State, asset: i64, text: &str) -> anyhow::Result<()> {
    let path = dir(state, asset).join("style.json");
    let mut s: Style = read_json(&path).await.unwrap_or_default();
    s.style = text.trim().to_string();
    write_json(&path, &s).await?;
    if let Ok(mut entries) = tokio::fs::read_dir(dir(state, asset)).await {
        while let Ok(Some(e)) = entries.next_entry().await {
            if e.file_name().to_string_lossy().ends_with(".png") {
                let _ = tokio::fs::remove_file(e.path()).await;
            }
        }
    }
    Ok(())
}

/// A chapter's scenes, chosen by the director the first time (and the look-book kept up to
/// date with anyone new in them).
async fn plan(state: &State, asset: i64, chapter: usize) -> anyhow::Result<Vec<Scene>> {
    let path = dir(state, asset).join(format!("plan-{chapter}.json"));
    if let Some(p) = read_json::<Vec<Scene>>(&path).await {
        return Ok(p);
    }
    let lock = LOCKS.lock().unwrap().entry(asset).or_default().clone();
    let _held = lock.lock().await;
    if let Some(p) = read_json::<Vec<Scene>>(&path).await {
        return Ok(p);
    }
    let text = sentences(state, asset, chapter).await?;
    // Title pages, contents and such, and chapters with pictures of their own (an illustrated
    // edition already has its artist): none.
    let has_own = {
        let a = db::asset(&state.db, asset).await?.context("no such book")?;
        let src = book::source(&a, &state.data_dir);
        tokio::task::spawn_blocking(move || book::chapter(&src, asset, chapter).map(|c| c.html.contains("<img"))).await?.unwrap_or(false)
    };
    if text.len() < 20 || has_own {
        write_json(&path, &Vec::<Scene>::new()).await?;
        return Ok(vec![]);
    }
    let (title, author) = title_author(state, asset).await?;
    let key = format!("pictures-plan:{asset}:{chapter}");
    crate::activity::set(&key, "Pictures", &title, format!("Choosing the scenes for chapter {}", chapter + 1), 0, 0);
    let planned = plan_with(state, asset, chapter, &text, &title, &author, &path).await;
    crate::activity::clear(&key);
    planned
}

async fn plan_with(state: &State, asset: i64, chapter: usize, text: &[String], title: &str, author: &str, path: &PathBuf) -> anyhow::Result<Vec<Scene>> {
    let style = style(state, asset).await?;
    let numbered: String = text.iter().enumerate().map(|(i, s)| format!("{i}: {s}\n")).collect();
    let looks: String = style.characters.iter().map(|(n, l)| format!("- {n}: {l}\n")).collect();
    // (About one a page: a page holds ten-odd sentences.)
    let want = (text.len() / 10).clamp(2, 120);
    let system = format!(
        "You are the illustration director for an illustrated edition of {title} by {author}. Read this chapter \
         and choose about {want} moments to illustrate, spread evenly through it (roughly one a page, so about every ten sentences). \
         Picture what the reader would want to see: whenever a character does something (an action, a gesture, \
         a discovery, a fright, a fight, a meeting), show that moment; whenever the story arrives somewhere \
         striking or atmospheric (a house, a street, a room, a landscape, a strange place), show the place itself, \
         with or without people; and the vivid, funny, eerie or important moments. For each give: the number of the sentence where it \
         happens; a caption for under the picture, one short sentence saying what it shows, as in an \
         illustrated edition ('Dan drops the bottle into the trash outside the Burger King.'); and the scene for the artist in at most 40 words, the most \
         important first: who is there, described by how they look (the artist doesn't know their names: write \
         the look-book's description in, e.g. 'a fair-haired girl of seven in a pale blue dress and white \
         pinafore'), what they are doing, their expressions, then the setting, light and framing. No art \
         style and no text in the picture. Keep every \
         character looking the same through the whole book: use the look-book, and for anyone pictured who \
         isn't in it, add a look of at most 20 words (age, build, hair, clothes, colours; for creatures, what \
         they are and wear), true to how the book describes them. Reply with JSON only: {{\"scenes\": \
         [{{\"sentence\": <n>, \"caption\": \"<caption>\", \"scene\": \"<scene>\", \"characters\": [\"<name, the most prominent first>\"]}}], \
         \"new_looks\": {{\"<name>\": \"<look>\"}}}}."
    );
    let user = format!("Look-book:\n{}\nChapter:\n{numbered}", if looks.is_empty() { "(empty)\n".to_string() } else { looks });
    let reply = json_in(&crate::llm::chat(state, &system, &[("user".into(), user)]).await?)?;
    // Anyone new in the look-book.
    if let Some(new) = reply["new_looks"].as_object().filter(|n| !n.is_empty()) {
        let spath = dir(state, asset).join("style.json");
        let mut s: Style = read_json(&spath).await.unwrap_or(style.clone());
        for (n, l) in new {
            if let Some(l) = l.as_str().filter(|l| !l.trim().is_empty()) {
                s.characters.entry(n.clone()).or_insert_with(|| l.trim().to_string());
            }
        }
        write_json(&spath, &s).await?;
    }
    let mut scenes: Vec<Scene> = reply["scenes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    let sentence = v["sentence"].as_u64()? as usize;
                    let scene = v["scene"].as_str()?.trim().to_string();
                    (sentence < text.len() && !scene.is_empty()).then(|| Scene {
                        sentence,
                        caption: v["caption"].as_str().unwrap_or("").trim().to_string(),
                        scene,
                        characters: v["characters"].as_array().map(|c| c.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                        // (Fixed per scene, so a picture drawn again comes out the same.)
                        seed: (asset as u64) * 1_000_003 + (chapter as u64) * 1009 + sentence as u64,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    scenes.sort_by_key(|s| s.sentence);
    scenes.dedup_by_key(|s| s.sentence);
    write_json(path, &scenes).await?;
    Ok(scenes)
}

static LOCKS: LazyLock<std::sync::Mutex<std::collections::HashMap<i64, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);
/// Books whose pictures are being drawn.
static DRAWING: LazyLock<std::sync::Mutex<HashSet<i64>>> = LazyLock::new(Default::default);

/// What a chapter's pictures are, and which are drawn (`ready`). Asking also sets the director
/// and the artist working on this chapter and the next two, in the background.
pub async fn chapter(state: &State, asset: i64, chapter: usize) -> serde_json::Value {
    ahead(state, asset, chapter);
    let scenes: Vec<Scene> = read_json(&dir(state, asset).join(format!("plan-{chapter}.json"))).await.unwrap_or_default();
    let list: Vec<_> = scenes
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "sentence": s.sentence,
                "caption": s.caption,
                "ready": image_path(state, asset, chapter, i).exists(),
                "url": format!("/api/assets/{asset}/illustrations/{chapter}/{i}.png"),
            })
        })
        .collect();
    json!(list)
}

fn ahead(state: &State, asset: i64, from: usize) {
    if !DRAWING.lock().unwrap().insert(asset) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let chapters = match db::asset(&state.db, asset).await {
            Ok(Some(a)) => {
                let src = book::source(&a, &state.data_dir);
                tokio::task::spawn_blocking(move || book::layout(&src).map(|l| l.chapters)).await.ok().and_then(Result::ok).unwrap_or(from + 3)
            }
            _ => from + 3,
        };
        for c in from..(from + 3).min(chapters) {
            let scenes = match plan(&state, asset, c).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("illustrations, chapter {c}: {e:#}");
                    break;
                }
            };
            if let Err(e) = draw(&state, asset, c, &scenes).await {
                tracing::warn!("drawing chapter {c}: {e:#}");
                break;
            }
        }
        DRAWING.lock().unwrap().remove(&asset);
    });
}

/// Draws a chapter's pictures not drawn yet, all in one go on the GPU. Not while the full cast
/// is performing: the performer has the card, and stopping it would leave the listener waiting.
async fn draw(state: &State, asset: i64, chapter: usize, scenes: &[Scene]) -> anyhow::Result<()> {
    let style = style(state, asset).await?;
    // Portraits of anyone pictured who hasn't one yet: drawn first, in the same go.
    let mut jobs = Vec::new();
    let mut portraits = HashSet::new();
    for (i, sc) in scenes.iter().enumerate() {
        if image_path(state, asset, chapter, i).exists() {
            continue;
        }
        for c in &sc.characters {
            let Some(look) = style.characters.get(c) else { continue };
            let path = portrait_path(state, asset, c);
            if !path.exists() && portraits.insert(slug(c)) {
                let p = format!("Portrait of {}, head and shoulders, facing the viewer, calm expression, plain soft background", look.trim_end_matches('.'));
                jobs.push(json!({
                    "prompt": p,
                    "prompt_2": format!("{}. {p}", style.style),
                    "seed": 4242 + slug(c).len() as u64,
                    "width": 896, "height": 1152,
                    "out": path,
                }));
            }
        }
    }
    for (i, s) in scenes.iter().enumerate().filter(|(i, _)| !image_path(state, asset, chapter, *i).exists()) {
        // The scene's main character's portrait shown to the artist, so they look the same.
        let reference = s.characters.iter().find(|c| style.characters.contains_key(*c)).map(|c| portrait_path(state, asset, c));
        jobs.push(json!({
            // (Two prompts: SDXL reads each through its own text encoder, 77 tokens apiece:
            // the moment and who's in it in one, the style in the other, so neither crowds
            // the other out.)
            "prompt": s.scene,
            "prompt_2": format!("{}. {}", style.style, s.scene),
            "seed": s.seed,
            "ref": reference,
            "ref_scale": 0.45,
            "out": image_path(state, asset, chapter, i),
        }));
    }
    if jobs.is_empty() {
        return Ok(());
    }
    if crate::cast::performing_recently() {
        let title = crate::activity::title(state, asset).await;
        let key = format!("pictures-wait:{asset}");
        crate::activity::set(&key, "Pictures", &title, "Waiting for the full cast to finish with the graphics card", 0, 0);
        while crate::cast::performing_recently() {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
        crate::activity::clear(&key);
    }
    // How many are drawn, for the jobs pane (counted from the pictures as they land).
    let outs: Vec<PathBuf> = jobs.iter().filter_map(|j| j["out"].as_str().map(PathBuf::from)).collect();
    let total = outs.len() as u32;
    let title = crate::activity::title(state, asset).await;
    let key = format!("pictures-draw:{asset}:{chapter}");
    let what = format!("Drawing chapter {}'s pictures", chapter + 1);
    crate::activity::set(&key, "Pictures", &title, what.clone(), 0, total);
    let ticker = {
        let (key, title, what) = (key.clone(), title.clone(), what.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let done = outs.iter().filter(|p| p.exists()).count() as u32;
                crate::activity::set(&key, "Pictures", &title, what.clone(), done, total);
            }
        })
    };
    let tts = state.settings.read().await.ai.tts_url.clone();
    let sent = state
        .http
        .post(format!("{}/illustrate", tts.trim_end_matches('/')))
        .json(&json!({ "jobs": jobs }))
        .timeout(std::time::Duration::from_secs(3600))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")
        .and_then(|r| r.error_for_status().map_err(Into::into));
    ticker.abort();
    crate::activity::clear(&key);
    sent.map(|_| ())
}
