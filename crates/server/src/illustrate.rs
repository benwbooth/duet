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
        // (Real pictures, not the little ornaments many books put at a chapter's head: an image
        // file of any size.)
        tokio::task::spawn_blocking(move || {
            let html = book::chapter(&src, asset, chapter).map(|c| c.html).unwrap_or_default();
            html.split("src=\"").skip(1).filter_map(|rest| rest.split('"').next()).filter_map(|url| url.split("/res/").nth(1)).any(|path| {
                book::resource(&src, path).map(|(bytes, _)| bytes.len() > 12_000).unwrap_or(false)
            })
        })
        .await
        .unwrap_or(false)
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
    let want = (text.len() / PER).clamp(1, 60);
    let system = format!(
        "You are the illustration director for an illustrated edition of {title} by {author}. Read this chapter \
         and choose about {want} moments to illustrate, spread evenly through it (roughly one every {PER} sentences). \
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

/// (Lining the recording up with the book no longer stops the pictures: it listens on the
/// processor while they're being drawn.)
pub fn listening() {}

/// Sentences per picture (about one every page and a half).
const PER: usize = 45;

/// Which of a chapter's scenes are shown: all, unless it was planned more densely than now
/// (then evenly fewer of them, so pictures already drawn are kept).
fn shown(scenes: &[Scene]) -> impl Fn(usize) -> bool + use<> {
    // (Kept evenly: two of every three, one of every two... whatever brings it to the density.)
    let span = scenes.last().map(|s| s.sentence + 1).unwrap_or(1);
    let keep = ((span as f64 / PER as f64) / scenes.len().max(1) as f64).min(1.0);
    move |i| ((i + 1) as f64 * keep).floor() > (i as f64 * keep).floor() || keep >= 1.0
}

/// Where each book's reader is (chapter, sentence): pictures are drawn nearest it first.
static READER: LazyLock<std::sync::Mutex<std::collections::HashMap<i64, (usize, usize)>>> = LazyLock::new(Default::default);

/// What a chapter's pictures are, and which are drawn (`ready`). Asking (with where the reader
/// is, `near`) also sets the director and the artist working round that place, in the
/// background: the nearest pictures first, a few at a time, so a move elsewhere is followed.
pub async fn chapter(state: &State, asset: i64, chapter: usize, near: Option<usize>) -> serde_json::Value {
    READER.lock().unwrap().insert(asset, (chapter, near.unwrap_or(0)));
    ahead(state, asset);
    let scenes: Vec<Scene> = read_json(&dir(state, asset).join(format!("plan-{chapter}.json"))).await.unwrap_or_default();
    let show = shown(&scenes);
    let list: Vec<_> = scenes
        .iter()
        .enumerate()
        .filter(|(i, _)| show(*i))
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

/// Pictures drawn at a time: few enough that the drawing turns to wherever the reader has got
/// to soon after they move (loading the model again each time costs a few seconds).
const BATCH: usize = 4;

fn ahead(state: &State, asset: i64) {
    if !DRAWING.lock().unwrap().insert(asset) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let chapters = match db::asset(&state.db, asset).await {
            Ok(Some(a)) => {
                let src = book::source(&a, &state.data_dir);
                tokio::task::spawn_blocking(move || book::layout(&src).map(|l| l.chapters)).await.ok().and_then(Result::ok).unwrap_or(0)
            }
            _ => 0,
        };
        let title = crate::activity::title(&state, asset).await;
        let key = format!("pictures-draw:{asset}");
        loop {
            let Some((c, near)) = READER.lock().unwrap().get(&asset).copied() else { break };
            // The scenes not drawn yet in this chapter and the next two, nearest the reader
            // first: ahead of them before behind, this chapter before the next.
            let mut todo: Vec<(usize, usize, usize, Scene)> = Vec::new();
            let mut failed = false;
            for (k, cc) in (c..(c + 3).min(chapters.max(c + 1))).enumerate() {
                let scenes = match plan(&state, asset, cc).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("illustrations, chapter {cc}: {e:#}");
                        failed = true;
                        break;
                    }
                };
                let show = shown(&scenes);
                for (i, sc) in scenes.into_iter().enumerate() {
                    if !show(i) || image_path(&state, asset, cc, i).exists() {
                        continue;
                    }
                    let d = if k == 0 {
                        if sc.sentence >= near { sc.sentence - near } else { (near - sc.sentence) * 4 }
                    } else {
                        1_000_000 * k + sc.sentence
                    };
                    todo.push((d, cc, i, sc));
                }
            }
            todo.sort_by_key(|t| t.0);
            todo.truncate(BATCH);
            if todo.is_empty() {
                break;
            }
            // The reader's audio comes first: while the full cast has the card (or is kept
            // loaded for a reader in full cast mode), the pictures wait. (Lining an audiobook up
            // with the book pauses them in the voice server, for as long as it takes.)
            if crate::cast::performing_recently() || crate::cast::cast_open_recently() {
                crate::activity::set(&key, "Pictures", &title, "Waiting: the full cast's voices come first", 0, 0);
                while crate::cast::performing_recently() || crate::cast::cast_open_recently() {
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                }
                continue;
            }
            // (How the chapter being drawn is coming along, for the jobs pane: counted again from
            // the pictures as they land, every few seconds while a batch is drawn.)
            let cd = todo[0].1;
            let plan_c: Vec<Scene> = read_json(&dir(&state, asset).join(format!("plan-{cd}.json"))).await.unwrap_or_default();
            let show_c = shown(&plan_c);
            let wanted: Vec<PathBuf> = (0..plan_c.len()).filter(|i| show_c(*i)).map(|i| image_path(&state, asset, cd, i)).collect();
            let what = if cd == c { format!("Drawing the pictures for the chapter you're reading ({})", cd + 1) } else { format!("Drawing the pictures for chapter {}, ahead of you", cd + 1) };
            let ticker = {
                let (key, title, what, wanted) = (key.clone(), title.clone(), what.clone(), wanted.clone());
                tokio::spawn(async move {
                    loop {
                        let drawn = wanted.iter().filter(|p| p.exists()).count() as u32;
                        crate::activity::set(&key, "Pictures", &title, what.clone(), drawn, wanted.len() as u32);
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    }
                })
            };
            let items: Vec<(usize, usize, Scene)> = todo.into_iter().map(|(_, cc, i, sc)| (cc, i, sc)).collect();
            let drew = draw(&state, asset, &items).await;
            ticker.abort();
            if let Err(e) = drew {
                tracing::warn!("drawing pictures: {e:#}");
                failed = true;
            }
            if failed {
                break;
            }
        }
        crate::activity::clear(&key);
        DRAWING.lock().unwrap().remove(&asset);
    });
}

/// Draws these pictures (chapter, index, scene) on the GPU, and the portraits of anyone in them
/// who hasn't one yet. Not while the full cast is performing: the performer has the card, and
/// stopping it would leave the listener waiting.
async fn draw(state: &State, asset: i64, items: &[(usize, usize, Scene)]) -> anyhow::Result<()> {
    let style = style(state, asset).await?;
    // Z-Image-Turbo reads the whole description with a language model, so each picture's prompt
    // is the scene, how everyone in it looks (the look-book, word for word: the same in every
    // picture) and the style.
    let mut jobs = Vec::new();
    for (chapter, i, s) in items {
        let looks: Vec<String> = s.characters.iter().filter_map(|c| style.characters.get(c).map(|l| format!("{c}: {l}"))).collect();
        let who = if looks.is_empty() { String::new() } else { format!(" The people in it look like this. {}", looks.join(" ")) };
        jobs.push(json!({
            "full_prompt": format!("{}{who} Art style: {}. No text or lettering.", s.scene, style.style),
            "prompt": s.scene,
            "seed": s.seed,
            "out": image_path(state, asset, *chapter, *i),
        }));
    }
    if jobs.is_empty() {
        return Ok(());
    }
    let tts = state.settings.read().await.ai.tts_url.clone();
    state
        .http
        .post(format!("{}/illustrate", tts.trim_end_matches('/')))
        .json(&json!({ "jobs": jobs, "model": "z-image" }))
        .timeout(std::time::Duration::from_secs(1800))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?
        .error_for_status()?;
    Ok(())
}
