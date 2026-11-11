//! Renumbers saved data when the sentence splitter changes (`text::SEGMENTATION_VERSION`).
//!
//! Full-cast lines, sync maps, pictures and reading places all point at sentences by number,
//! so a chapter that now splits differently ("DERRY DEPT. OF PUBLIC WORKS." used to end at
//! "DEPT.") would have every line after the change off by one. Once per version, at startup,
//! each book with saved data has its chapters split both ways and, where they differ:
//! - full-cast recordings and directions move to their sentence's new number; a recording
//!   of half a sentence (two old sentences now one, or one now two) is dropped and performed
//!   again, and a directed part that now takes in undirected sentences is directed again;
//! - sync spans and pictures move to the new numbers (merged sentences' spans joined);
//! - the reading place moves with its sentence;
//! - the workers' copy of the book's sentences is rebuilt.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use duet_common::{AssetKind, SyncSpan, text};

use crate::{State, book, db, jobs};

pub async fn run(state: &State) {
    let marker = state.data_dir.join("segmentation-version");
    let done = tokio::fs::read_to_string(&marker).await.ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(1);
    if done >= text::SEGMENTATION_VERSION {
        return;
    }
    match assets_with_data(state).await {
        Ok(ids) => {
            for id in ids {
                if let Err(e) = asset(state, id).await {
                    tracing::warn!("renumbering sentences of book {id}: {e:#}");
                }
            }
        }
        Err(e) => tracing::warn!("renumbering sentences: {e:#}"),
    }
    let _ = tokio::fs::write(&marker, text::SEGMENTATION_VERSION.to_string()).await;
}

/// Books with anything saved by sentence number.
async fn assets_with_data(state: &State) -> anyhow::Result<BTreeSet<i64>> {
    let mut ids = BTreeSet::new();
    for sub in ["full-cast", "illustrations", "books"] {
        if let Ok(mut rd) = tokio::fs::read_dir(state.data_dir.join("cache").join(sub)).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                if let Some(id) = e.file_name().to_str().and_then(|n| n.parse().ok()) {
                    ids.insert(id);
                }
            }
        }
    }
    let synced: Vec<(i64,)> = sqlx::query_as("SELECT DISTINCT ebook_asset FROM sync_maps").fetch_all(&state.db).await?;
    ids.extend(synced.into_iter().map(|r| r.0));
    let placed: Vec<(i64,)> = sqlx::query_as("SELECT work_id FROM positions").fetch_all(&state.db).await?;
    for (work,) in placed {
        let ebooks: Vec<i64> = db::work_assets(&state.db, work).await?.into_iter().filter(|a| a.kind == AssetKind::Ebook).map(|a| a.id).collect();
        ids.extend(ebooks);
    }
    Ok(ids)
}

/// Old sentence number → new, for one chapter; `None` when the chapter splits the same.
#[derive(Debug, PartialEq)]
struct Renumber {
    /// The new number of each old sentence (where it starts).
    new_of: Vec<usize>,
    /// Old sentences that are exactly one new sentence (their recordings still fit).
    whole: Vec<bool>,
    /// How many sentences the chapter has now.
    count: usize,
}

fn renumber(old: &[String], new: &[String]) -> Option<Renumber> {
    if old == new {
        return None;
    }
    let len = |s: &String| s.chars().filter(|c| !c.is_whitespace()).count();
    let starts = |v: &[String]| -> Vec<usize> {
        v.iter().scan(0, |acc, s| {
            let at = *acc;
            *acc += len(s);
            Some(at)
        })
        .collect()
    };
    let (old_at, new_at) = (starts(old), starts(new));
    let total = |v: &[String]| v.iter().map(len).sum::<usize>();
    if total(old) != total(new) {
        // Not the same text split differently: nothing can be carried over safely.
        return Some(Renumber { new_of: vec![usize::MAX; old.len()], whole: vec![false; old.len()], count: new.len() });
    }
    let new_of: Vec<usize> = old_at.iter().map(|&o| new_at.partition_point(|&n| n <= o).saturating_sub(1)).collect();
    let whole = old
        .iter()
        .zip(&new_of)
        .map(|(s, &j)| new.get(j) == Some(s) && new_of.iter().filter(|&&k| k == j).count() == 1)
        .collect();
    Some(Renumber { new_of, whole, count: new.len() })
}

async fn asset(state: &State, id: i64) -> anyhow::Result<()> {
    let Some(a) = db::asset(&state.db, id).await? else { return Ok(()) };
    let src = book::source(&a, &state.data_dir);
    if matches!(src, book::Source::Pdf(_) | book::Source::Comic(_) | book::Source::NeedsConversion) || crate::comic::is_pages(&a, state) {
        return Ok(());
    }
    let changes: BTreeMap<usize, Renumber> = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let chapters = book::layout(&src)?.chapters;
        let mut out = BTreeMap::new();
        for c in 0..chapters {
            let (Ok(old), Ok(new)) = (book::chapter_sentences_v1(&src, id, c), book::chapter_sentences(&src, id, c)) else { continue };
            if let Some(r) = renumber(&old, &new) {
                out.insert(c, r);
            }
        }
        Ok(out)
    })
    .await??;
    // (The workers' copy of the sentences: rebuilt when next wanted.)
    let _ = tokio::fs::remove_dir_all(state.data_dir.join("cache").join("books").join(id.to_string())).await;
    if changes.is_empty() {
        return Ok(());
    }
    tracing::info!("book {id}: renumbering sentences in {} chapter(s) for the new sentence splitter", changes.len());
    full_cast(&state.data_dir.join("cache").join("full-cast").join(id.to_string()), &changes).await.context("full cast")?;
    pictures(&state.data_dir.join("cache").join("illustrations").join(id.to_string()), &changes).await.context("pictures")?;
    sync(state, id, &changes).await.context("sync")?;
    place(state, &a, &changes).await.context("reading place")?;
    Ok(())
}

async fn read<T: serde::de::DeserializeOwned>(p: &Path) -> Option<T> {
    serde_json::from_slice(&tokio::fs::read(p).await.ok()?).ok()
}

async fn write<T: serde::Serialize>(p: &Path, v: &T) -> anyhow::Result<()> {
    tokio::fs::write(p, serde_json::to_vec(v)?).await?;
    Ok(())
}

async fn full_cast(dir: &Path, changes: &BTreeMap<usize, Renumber>) -> anyhow::Result<()> {
    if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
        return Ok(());
    }
    let chunk = crate::cast::CHUNK;
    for (&c, r) in changes {
        // Recordings: kept under their new number when the sentence is unchanged, else dropped.
        // (Moved out of the way first, so a lower new number never overwrites one not yet moved.)
        let mut moves = Vec::new();
        for (i, (&j, &whole)) in r.new_of.iter().zip(&r.whole).enumerate() {
            for ext in ["mp3", "words.json"] {
                let from = dir.join(format!("{c}-{i}.{ext}"));
                if !tokio::fs::try_exists(&from).await.unwrap_or(false) {
                    continue;
                }
                let tmp = dir.join(format!("{c}-{i}.{ext}.renumbering"));
                tokio::fs::rename(&from, &tmp).await?;
                moves.push((tmp, whole.then(|| dir.join(format!("{c}-{j}.{ext}")))));
            }
        }
        for (tmp, to) in moves {
            match to {
                Some(to) => tokio::fs::rename(&tmp, &to).await?,
                None => tokio::fs::remove_file(&tmp).await?,
            }
        }
        // Directions: a whole-chapter script (older versions) or one per part of CHUNK sentences.
        let parts_before = r.new_of.len().div_ceil(chunk).max(1);
        let mut directed = HashSet::new();
        let mut script: BTreeMap<usize, serde_json::Value> = BTreeMap::new();
        let (mut say, mut narration): (BTreeMap<usize, serde_json::Value>, BTreeMap<usize, serde_json::Value>) = Default::default();
        let whole_chapter = dir.join(format!("script-{c}.json"));
        if let Some(s) = read::<BTreeMap<usize, serde_json::Value>>(&whole_chapter).await {
            directed.extend(0..r.new_of.len());
            script.extend(s);
            tokio::fs::remove_file(&whole_chapter).await?;
        }
        for p in 0..parts_before {
            let sp = dir.join(format!("script-{c}-{p}.json"));
            if let Some(s) = read::<BTreeMap<usize, serde_json::Value>>(&sp).await {
                directed.extend(p * chunk..((p + 1) * chunk).min(r.new_of.len()));
                script.extend(s);
                say.extend(read::<BTreeMap<usize, serde_json::Value>>(&dir.join(format!("say-{c}-{p}.json"))).await.unwrap_or_default());
                narration.extend(read::<BTreeMap<usize, serde_json::Value>>(&dir.join(format!("narration-{c}-{p}.json"))).await.unwrap_or_default());
            }
            for f in ["script", "say", "narration"] {
                let _ = tokio::fs::remove_file(dir.join(format!("{f}-{c}-{p}.json"))).await;
            }
        }
        // Each new sentence gets the first direction of the old sentences it's made of; its
        // replacement words only if it's the same sentence (they're the whole sentence, reworded).
        let moved = |m: &BTreeMap<usize, serde_json::Value>, only_whole: bool| -> BTreeMap<usize, serde_json::Value> {
            let mut out = BTreeMap::new();
            for (&i, v) in m {
                let Some(&j) = r.new_of.get(i) else { continue };
                if j == usize::MAX || (only_whole && !r.whole[i]) {
                    continue;
                }
                out.entry(j).or_insert_with(|| v.clone());
            }
            out
        };
        let (script, say, narration) = (moved(&script, false), moved(&say, true), moved(&narration, false));
        let directed_new: HashSet<usize> = directed.iter().filter_map(|&i| r.new_of.get(i).copied()).filter(|&j| j != usize::MAX).collect();
        // A part is kept only if every sentence in it was directed before; else it's directed again.
        for p in 0..r.count.div_ceil(chunk).max(1) {
            let range = p * chunk..((p + 1) * chunk).min(r.count);
            if range.is_empty() || !range.clone().all(|j| directed_new.contains(&j)) {
                continue;
            }
            let pick = |m: &BTreeMap<usize, serde_json::Value>| m.range(range.clone()).map(|(k, v)| (*k, v.clone())).collect::<BTreeMap<_, _>>();
            write(&dir.join(format!("script-{c}-{p}.json")), &pick(&script)).await?;
            write(&dir.join(format!("say-{c}-{p}.json")), &pick(&say)).await?;
            write(&dir.join(format!("narration-{c}-{p}.json")), &pick(&narration)).await?;
        }
    }
    Ok(())
}

async fn pictures(dir: &Path, changes: &BTreeMap<usize, Renumber>) -> anyhow::Result<()> {
    for (&c, r) in changes {
        let path: PathBuf = dir.join(format!("plan-{c}.json"));
        let Some(mut plan) = read::<Vec<serde_json::Value>>(&path).await else { continue };
        for scene in &mut plan {
            if let Some(i) = scene["sentence"].as_u64() {
                let j = r.new_of.get(i as usize).copied().filter(|&j| j != usize::MAX).unwrap_or(r.count.saturating_sub(1).min(i as usize));
                scene["sentence"] = j.into();
            }
        }
        tokio::fs::write(&path, serde_json::to_vec_pretty(&plan)?).await?;
    }
    Ok(())
}

async fn sync(state: &State, ebook: i64, changes: &BTreeMap<usize, Renumber>) -> anyhow::Result<()> {
    let pairs: Vec<(i64, i64)> = sqlx::query_as("SELECT work_id, audiobook_asset FROM sync_maps WHERE ebook_asset = ?").bind(ebook).fetch_all(&state.db).await?;
    for (work, audio) in pairs {
        let Some(map) = jobs::sync_map(state, ebook, audio).await? else { continue };
        for (&c, r) in changes {
            let mut joined: BTreeMap<usize, SyncSpan> = BTreeMap::new();
            let mut spans: Vec<&SyncSpan> = map.spans.iter().filter(|s| s.chapter == c).collect();
            if spans.is_empty() {
                continue;
            }
            spans.sort_by_key(|s| s.sentence);
            for s in spans {
                let Some(&j) = r.new_of.get(s.sentence) else { continue };
                if j == usize::MAX {
                    continue;
                }
                match joined.get_mut(&j) {
                    Some(t) if t.track == s.track => {
                        t.start = t.start.min(s.start);
                        t.end = t.end.max(s.end);
                        t.words.extend(&s.words);
                    }
                    Some(_) => {}
                    None => {
                        joined.insert(j, SyncSpan { sentence: j, ..s.clone() });
                    }
                }
            }
            jobs::replace_chapter_sync(state, work, ebook, audio, c, joined.into_values().collect()).await?;
        }
    }
    Ok(())
}

async fn place(state: &State, ebook: &duet_common::Asset, changes: &BTreeMap<usize, Renumber>) -> anyhow::Result<()> {
    let ebooks = db::work_assets(&state.db, ebook.work_id).await?.into_iter().filter(|a| a.kind == AssetKind::Ebook).count();
    if ebooks != 1 {
        return Ok(());
    }
    let Some(mut p) = db::get_position(&state.db, ebook.work_id).await? else { return Ok(()) };
    let Some(r) = changes.get(&p.chapter) else { return Ok(()) };
    if let Some(&j) = r.new_of.get(p.sentence).filter(|&&j| j != usize::MAX) {
        p.sentence = j;
        // (Written directly, keeping when it was last read.)
        sqlx::query("UPDATE positions SET data = ? WHERE work_id = ?").bind(serde_json::to_string(&p)?).bind(ebook.work_id).execute(&state.db).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn renumbers_merged_and_split_sentences() {
        let old = v(&["A b.", "DEPT.", "OF WORKS.", "C d.", "No. I e."]);
        let new = v(&["A b.", "DEPT. OF WORKS.", "C d.", "No.", "I e."]);
        let r = renumber(&old, &new).unwrap();
        assert_eq!(r.new_of, [0, 1, 1, 2, 3]);
        assert_eq!(r.whole, [true, false, false, true, false]);
        assert_eq!(r.count, 5);
        assert!(renumber(&old, &old).is_none());
    }
}
