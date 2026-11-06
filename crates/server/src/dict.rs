//! Word definitions from a real dictionary: Wiktionary, through the Free Dictionary API
//! (dictionaryapi.dev). Inflected words ("treading", "tarts") fall back to their base forms.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use duet_common::{DictEntry, DictMeaning, DictSense};

use crate::State;

static CACHE: LazyLock<Mutex<HashMap<String, Vec<DictEntry>>>> = LazyLock::new(Default::default);

/// The word, then the base forms it might be an inflection of.
fn candidates(word: &str) -> Vec<String> {
    let w = word.to_lowercase();
    let mut out = vec![w.clone()];
    let mut push = |s: String| {
        if s.len() > 1 && !out.contains(&s) {
            out.push(s);
        }
    };
    for (suffix, replace) in [("ies", "y"), ("ied", "y"), ("ier", "y"), ("iest", "y"), ("ily", "y"), ("es", ""), ("s", ""), ("ed", ""), ("ed", "e"), ("ing", ""), ("ing", "e"), ("er", ""), ("est", ""), ("ly", ""), ("'s", ""), ("’s", "")] {
        if let Some(stem) = w.strip_suffix(suffix) {
            push(format!("{stem}{replace}"));
            // "stopped" → "stop", "running" → "run"
            let b = stem.as_bytes();
            if replace.is_empty() && b.len() > 2 && b[b.len() - 1] == b[b.len() - 2] {
                push(stem[..stem.len() - 1].to_string());
            }
        }
    }
    out
}

fn parse(v: &serde_json::Value) -> Vec<DictEntry> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|e| DictEntry {
            word: e["word"].as_str().unwrap_or("").to_string(),
            phonetic: e["phonetic"]
                .as_str()
                .map(str::to_string)
                .or_else(|| e["phonetics"].as_array()?.iter().find_map(|p| p["text"].as_str().map(str::to_string))),
            meanings: e["meanings"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| DictMeaning {
                    part_of_speech: m["partOfSpeech"].as_str().unwrap_or("").to_string(),
                    definitions: m["definitions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|d| DictSense {
                            definition: d["definition"].as_str().unwrap_or("").to_string(),
                            example: d["example"].as_str().map(str::to_string),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

/// Entries for a word (empty when the dictionary doesn't have it or any of its base forms).
pub async fn define(state: &State, word: &str) -> anyhow::Result<Vec<DictEntry>> {
    let word = word.trim().trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-');
    if word.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(hit) = CACHE.lock().unwrap().get(&word.to_lowercase()) {
        return Ok(hit.clone());
    }
    let mut found = Vec::new();
    for w in candidates(word) {
        let resp = state
            .http
            .get(format!("https://api.dictionaryapi.dev/api/v2/entries/en/{}", urlencoding::encode(&w)))
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await?;
        if resp.status().is_success() {
            found = parse(&resp.json().await?);
            if !found.is_empty() {
                break;
            }
        }
    }
    CACHE.lock().unwrap().insert(word.to_lowercase(), found.clone());
    Ok(found)
}

#[cfg(test)]
mod tests {
    #[test]
    fn base_forms() {
        let c = super::candidates("Treading");
        assert!(c.contains(&"tread".to_string()));
        assert!(super::candidates("stopped").contains(&"stop".to_string()));
        assert!(super::candidates("tarts").contains(&"tart".to_string()));
        assert!(super::candidates("hurried").contains(&"hurry".to_string()));
    }
}
