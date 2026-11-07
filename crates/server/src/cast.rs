//! The full cast, performed live: a chapter is directed the first time it's needed (who says each
//! line of dialogue and how, and a voice description for anyone new), then each sentence is voiced
//! as the listener reaches it, a few sentences ahead. Everything is kept on disk, so a sentence is
//! only ever performed once.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::{State, book, db};

/// Sentences per direction request: a chapter is directed a part at a time, as the listener
/// reaches it, so the first line plays after one short request rather than the whole chapter's.
const CHUNK: usize = 30;

/// (speaker, acting direction) for each sentence with dialogue, by sentence number.
type Script = BTreeMap<usize, (String, String)>;

/// The words to say for sentences whose printed text a voice would misread (numerals,
/// abbreviations, symbols), by sentence number, as the director wrote them.
type Say = BTreeMap<usize, String>;

#[derive(Default, Serialize, Deserialize)]
struct Cast {
    narrator: String,
    characters: BTreeMap<String, String>,
    /// The accent the book is read in (decided once, from where it's set and who wrote it).
    #[serde(default)]
    accent: String,
    /// How the book is told, by default (a short acting tag, e.g. "bright, cheerful, full of
    /// wonder"): the narrator's delivery where the director gave none.
    #[serde(default)]
    style: String,
}

/// What the full cast is busy with for each book, for the player to show while it waits:
/// (what, how many done, out of how many, since when).
static STATUS: LazyLock<std::sync::Mutex<HashMap<i64, (String, u32, u32, std::time::Instant)>>> = LazyLock::new(Default::default);

fn status(asset: i64, what: impl Into<String>, done: u32, total: u32) {
    STATUS.lock().unwrap().insert(asset, (what.into(), done, total, std::time::Instant::now()));
}

fn idle(asset: i64) {
    STATUS.lock().unwrap().remove(&asset);
}

/// What the full cast is doing for this book right now, if anything. While the performer
/// itself is starting (minutes, the first time), that, with how far along it is.
pub async fn current(state: &State, asset: i64) -> serde_json::Value {
    let now = STATUS.lock().unwrap().get(&asset).cloned();
    let Some((what, done, total, since)) = now else { return json!(null) };
    if what.starts_with("Performing") {
        let tts = state.settings.read().await.ai.tts_url.clone();
        let url = format!("{}/status", tts.trim_end_matches('/'));
        if let Ok(r) = state.http.get(url).timeout(std::time::Duration::from_secs(2)).send().await {
            if let Ok(v) = r.json::<serde_json::Value>().await {
                if v["fish_starting"].as_bool() == Some(true) {
                    let (elapsed, expected) = (v["elapsed"].as_f64().unwrap_or(0.0), v["expected"].as_f64().unwrap_or(360.0));
                    return json!({
                        "what": "Warming up the voices (the first line after a while takes longest)",
                        "progress": (elapsed / expected.max(1.0)).min(0.97),
                        "left": (expected - elapsed).max(0.0).round(),
                        "secs": elapsed.round(),
                    });
                }
            }
        }
    }
    json!({ "what": what, "done": done, "total": total, "secs": since.elapsed().as_secs() })
}

/// One lock per book: directing and casting happen once, however many requests arrive together.
static LOCKS: LazyLock<std::sync::Mutex<HashMap<i64, Arc<Mutex<()>>>>> = LazyLock::new(Default::default);

fn lock_for(asset: i64) -> Arc<Mutex<()>> {
    LOCKS.lock().unwrap().entry(asset).or_default().clone()
}

fn dir(state: &State, asset: i64) -> PathBuf {
    state.data_dir.join("cache").join("full-cast").join(asset.to_string())
}

/// Where a quotation still open at the start of sentence `n` began (the sentence it opened
/// in): a speech of several sentences ("Oh dear! Oh dear! I shall be late!") has its quote
/// marks only at its two ends.
fn open_quote_before(sentences: &[String], n: usize) -> Option<usize> {
    let mut open = None;
    for (i, s) in sentences.iter().enumerate().take(n) {
        for c in s.chars() {
            match c {
                '“' => open = Some(i),
                '”' => open = None,
                '"' => open = if open.is_some() { None } else { Some(i) },
                _ => {}
            }
        }
    }
    open
}

/// Splits a sentence into (text, speaker): quoted speech goes to the speaker, the rest to the narrator.
/// `narration`: how the narrator reads their part of it.
fn pieces(sentence: &str, line: Option<&(String, String)>, narration: &str) -> Vec<(String, Option<String>, String)> {
    let Some((speaker, direction)) = line else { return vec![(sentence.to_string(), None, narration.to_string())] };
    let mut out = Vec::new();
    let mut rest = sentence;
    while let Some(open) = rest.find(['“', '"']) {
        let (before, from) = rest.split_at(open);
        let open_len = from.chars().next().map(char::len_utf8).unwrap_or(1);
        let inner = &from[open_len..];
        let close = inner.find(['”', '"']).unwrap_or(inner.len());
        out.push((before.trim().to_string(), None, narration.to_string()));
        out.push((inner[..close].trim().to_string(), Some(speaker.clone()), direction.clone()));
        rest = inner.get(close..).map(|r| r.trim_start_matches(['”', '"'])).unwrap_or("");
    }
    out.push((rest.trim().to_string(), None, narration.to_string()));
    out.retain(|(t, _, _)| t.chars().any(char::is_alphanumeric));
    out
}

/// A Roman numeral's value (I to C), if `s` is one.
fn roman(s: &str) -> Option<u32> {
    if s.is_empty() || !s.chars().all(|c| "IVXLC".contains(c)) {
        return None;
    }
    let val = |c| match c { 'I' => 1, 'V' => 5, 'X' => 10, 'L' => 50, _ => 100 };
    let cs: Vec<u32> = s.chars().map(val).collect();
    let mut n = 0i64;
    for (i, &v) in cs.iter().enumerate() {
        if cs.get(i + 1).is_some_and(|&next| next > v) { n -= v as i64 } else { n += v as i64 }
    }
    // Only well-formed ones (so "VIV" or "IL" aren't read as numbers).
    (n > 0 && n <= 100 && to_roman(n as u32) == s).then_some(n as u32)
}

fn to_roman(mut n: u32) -> String {
    let mut out = String::new();
    for (v, r) in [(100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")] {
        while n >= v {
            out.push_str(r);
            n -= v;
        }
    }
    out
}

fn number_words(n: u32) -> String {
    const ONES: [&str; 20] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen"];
    const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
    match n {
        0..=19 => ONES[n as usize].to_string(),
        20..=99 => {
            let t = TENS[(n / 10) as usize];
            if n % 10 == 0 { t.to_string() } else { format!("{t}-{}", ONES[(n % 10) as usize]) }
        }
        _ => "one hundred".to_string(),
    }
}

/// The words to say for a line where the page shows a number the voices would misread: a
/// chapter heading that's just a Roman numeral ("XII." is "Chapter twelve", not "x-i-i"), and
/// "Chapter IV" or "Book II" ("Chapter four").
fn speakable(text: &str) -> String {
    let bare = text.trim().trim_end_matches(['.', ':']);
    if let Some(n) = roman(bare) {
        return format!("Chapter {}.", number_words(n));
    }
    let words: Vec<&str> = text.split(' ').collect();
    let mut out = Vec::with_capacity(words.len());
    for (i, w) in words.iter().enumerate() {
        let core = w.trim_end_matches(['.', ',', ':', ';']);
        let after_heading = i > 0 && ["chapter", "book", "part", "volume", "act", "scene", "canto"].contains(&words[i - 1].to_lowercase().as_str());
        match roman(core).filter(|_| after_heading) {
            Some(n) => out.push(format!("{}{}", number_words(n), &w[core.len()..])),
            None => out.push((*w).to_string()),
        }
    }
    out.join(" ")
}

/// `text` with each whole-word `from` replaced by `to`.
fn replace_words(text: &str, from: &str, to: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(from) {
        let (before, after) = (rest[..i].chars().last(), rest[i + from.len()..].chars().next());
        out.push_str(&rest[..i]);
        out.push_str(if word(before) || word(after) { from } else { to });
        rest = &rest[i + from.len()..];
    }
    out.push_str(rest);
    out
}

fn json_in(text: &str) -> anyhow::Result<serde_json::Value> {
    let (start, end) = (text.find('{').context("no JSON in the reply")?, text.rfind('}').context("no JSON in the reply")?);
    Ok(serde_json::from_str(&text[start..=end])?)
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Option<T> {
    serde_json::from_slice(&tokio::fs::read(path).await.ok()?).ok()
}

async fn write_json<T: Serialize>(path: &PathBuf, v: &T) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    tokio::fs::write(path, serde_json::to_vec(v)?).await?;
    Ok(())
}

/// The chat model directs one chapter: speakers and directions, and descriptions of new characters
/// (and of the narrator, the first time).
async fn direct(state: &State, asset: i64, chapter: usize, part: usize, title: &str, author: &str, sentences: &[String], cast: &mut Cast, say: &mut Say, narration: &mut Say) -> anyhow::Result<Script> {
    let mut script = Script::new();
    let parts = sentences.len().div_ceil(CHUNK).max(1);
    for start in (part * CHUNK..sentences.len()).step_by(CHUNK).take(1) {
        let what = if parts > 1 { format!("Reading part {} of chapter {} to work out who says what", part + 1, chapter + 1) } else { format!("Reading chapter {} to work out who says what", chapter + 1) };
        status(asset, what, 0, 0);
        let part = &sentences[start..(start + CHUNK).min(sentences.len())];
        let numbered: String = part.iter().enumerate().map(|(i, s)| format!("{}: {s}\n", start + i)).collect();
        let known: String = cast.characters.iter().map(|(n, d)| format!("- {n}: {d}\n")).collect();
        let system = "You are the director of a full-cast audiobook. For each numbered sentence containing quoted \
            speech, name the character speaking and give an acting direction for how the line is delivered. \
            The voices are actors who play exactly what the direction says and fall flat without one, so \
            direct boldly: real, vivid emotion every time, never 'neutral' or 'calm' unless the moment truly is. \
            A direction is a short tag of one to four words that the actor can play at once: the emotion first, \
            then intensity or pace (e.g. 'excited, breathless', 'shouting angrily', 'whispering, scared', \
            'laughing', 'sobbing', 'indignant', 'sly, amused'). Use the narration for cues (\"cried\", \
            \"muttered\", \"shrieked\") and the scene's stakes: characters in a children's book are big, \
            colourful and full of life. For any character not in the known \
            list, write a one-sentence voice description for casting: gender, approximate age, accent, pitch, \
            timbre, pace and temperament. If asked, also describe the ideal narrator's voice the same way. \
            Accents follow the book: where it's set and where its author is from (an English novel gets English \
            voices, narrator included); a character the story gives another origin speaks with that one instead (a \
            Russian count with a Russian accent, a French maid with a French one). Always name the accent. Reply \
            with JSON only: {\"lines\": {\"<sentence number>\": {\"speaker\": \"<name>\", \"direction\": \"<direction>\"}}, \
            \"new_characters\": {\"<name>\": \"<voice description>\"}, \"narrator\": \"<voice description or empty>\", \"say\": {}, \"narration\": {}}. \
            Each character keeps one voice for the whole book, so use exactly the name from the known list when \
            they're in it, and give a new character a plain name only (\"Alice\", \"White Rabbit\"): never add how \
            they're speaking or what they're doing to the name; that goes in the direction. Lines spoken by \
            several characters together go to the one leading them. \
            The voices read exactly the letters they're given, so for any numbered sentence (dialogue or \
            narration) with words that would be misread aloud, give each such word or phrase exactly as printed \
            and the words to say instead, in \"say\": {\"<sentence number>\": {\"<as printed>\": \"<as spoken>\"}}. \
            Roman numerals become the number they mean, as a reader would say it (\"Chapter XII\": \"Chapter \
            twelve\", \"Henry VIII\": \"Henry the Eighth\", \"George III\": \"George the Third\"); digits, dates, \
            money, ordinals and symbols are spelled out the way the book's readers would say them (a year \"1865\": \
            \"eighteen sixty-five\", \"1066\": \"ten sixty-six\"; \"£5\": \"five pounds\"; \"&c.\": \"et cetera\"). \
            Never list the pronoun \"I\" or ordinary words, and leave sentences that read fine out of \"say\". \
            Direct the narrator too: the narrator is a performer, not someone reading aloud. For every numbered \
            sentence give a short tag of one to four words for how the narrator delivers its narration \
            (e.g. 'wonder, building', 'hushed, suspenseful', 'brisk, playful', 'amused'), in \
            \"narration\": {\"<sentence number>\": \"<direction>\"}. Pitch the energy to the book's audience (a \
            children's book is told with warmth, wonder, playfulness and lively variety, as a favourite \
            storyteller would; a thriller with tension; a comedy with relish), follow the mood of each moment \
            (hushed for suspense, quick for excitement, wry for a joke), and never flat or dry.";
        let ask_narrator = if cast.narrator.is_empty() { "\nAlso describe the narrator's voice." } else { "" };
        let user = format!(
            "Book: {title} by {author}\nKnown characters:\n{}{ask_narrator}\n\nSentences:\n{numbered}",
            if known.is_empty() { "(none yet)\n" } else { &known }
        );
        let reply = json_in(&crate::llm::chat(state, system, &[("user".into(), user)]).await?)?;
        let mut new_names = Vec::new();
        if let Some(new) = reply["new_characters"].as_object() {
            for (n, d) in new {
                if let Some(name) = canonical(n, &cast.characters) {
                    if !cast.characters.contains_key(&name) {
                        new_names.push(name.clone());
                    }
                    cast.characters.entry(name).or_insert_with(|| d.as_str().unwrap_or("").to_string());
                }
            }
        }
        // Anyone speaking who wasn't described is cast too.
        if let Some(lines) = reply["lines"].as_object() {
            for v in lines.values() {
                if let Some(name) = v["speaker"].as_str().or(v.as_str()).and_then(|s| canonical(s, &cast.characters)) {
                    if !cast.characters.contains_key(&name) && !new_names.contains(&name) {
                        new_names.push(name);
                    }
                }
            }
        }
        let narrator_new = cast.narrator.is_empty();
        if !new_names.is_empty() || narrator_new {
            cast_voices(state, title, author, &new_names, narrator_new, &numbered, cast).await;
        }
        if let Some(lines) = reply["lines"].as_object() {
            for (k, v) in lines {
                let (Ok(n), Some(speaker)) = (k.parse::<usize>(), v["speaker"].as_str().or(v.as_str())) else { continue };
                // (The narrator's lines need no entry: they're the narrator's anyway.)
                let Some(speaker) = canonical(speaker, &cast.characters) else { continue };
                if n < sentences.len() {
                    script.insert(n, (speaker, v["direction"].as_str().unwrap_or("").trim().to_string()));
                }
            }
        }
        if let Some(lines) = reply["narration"].as_object() {
            for (k, v) in lines {
                if let (Ok(n), Some(d)) = (k.parse::<usize>(), v.as_str().map(str::trim).filter(|d| !d.is_empty())) {
                    if n < sentences.len() {
                        narration.insert(n, d.to_string());
                    }
                }
            }
        }
        // Smaller models often leave the narration out of all that: asked for on its own.
        if !part.iter().enumerate().any(|(i, _)| narration.contains_key(&(start + i))) {
            let style = if cast.style.is_empty() { String::new() } else { format!(" The book's usual delivery: {}.", cast.style) };
            let ask = format!(
                "You direct the narrator of a full-cast audiobook of {title} by {author}. The narrator is a performer, \
                 not someone reading aloud, and plays exactly the tag you give. For every numbered sentence give a \
                 short acting tag of one to four words for how its narration is delivered, pitched to the book's \
                 readers (a children's book: bright, warm, playful, full of wonder) and to the moment (hushed for \
                 suspense, quick for excitement, wry for a joke); never flat, dry or neutral.{style} Reply with JSON \
                 only: {{\"narration\": {{\"<sentence number>\": \"<tag>\"}}}}."
            );
            if let Ok(r) = crate::llm::chat(state, &ask, &[("user".into(), format!("Sentences:\n{numbered}"))]).await.and_then(|t| json_in(&t)) {
                if let Some(lines) = r["narration"].as_object() {
                    for (k, v) in lines {
                        if let (Ok(n), Some(d)) = (k.parse::<usize>(), v.as_str().map(str::trim).filter(|d| !d.is_empty())) {
                            if n < sentences.len() {
                                narration.insert(n, d.to_string());
                            }
                        }
                    }
                }
            }
        }
        if let Some(lines) = reply["say"].as_object() {
            for (k, v) in lines {
                let (Ok(n), Some(swaps)) = (k.parse::<usize>(), v.as_object()) else { continue };
                let Some(mut t) = sentences.get(n).cloned() else { continue };
                for (printed, spoken) in swaps {
                    let Some(spoken) = spoken.as_str().map(str::trim).filter(|s| !s.is_empty()) else { continue };
                    // (Only whole words, and never the pronoun: "I" isn't one.)
                    if printed.trim() == "I" || printed.trim().is_empty() {
                        continue;
                    }
                    t = replace_words(&t, printed.trim(), spoken);
                }
                if &t != &sentences[n] {
                    say.insert(n, t);
                }
            }
        }
        if let Some(d) = reply["narrator"].as_str().filter(|d| !d.is_empty() && cast.narrator.is_empty()) {
            cast.narrator = d.to_string();
        }
    }
    Ok(script)
}

/// Casting, as its own careful step: each character's sex (as the book has them: the White
/// Rabbit is "he"), age, accent and voice, in fixed fields, made into a description the voice
/// designer follows (sex and accent first, plainly). The book's accent is settled once and every
/// voice gets it unless the story says otherwise. `context`: some of the text they appear in.
async fn cast_voices(state: &State, title: &str, author: &str, names: &[String], narrator: bool, context: &str, cast: &mut Cast) {
    let system = "You are casting a full-cast audiobook. First settle the book's accent: the one its readers \
        would expect from where it's set and where its author is from (e.g. 'British (Received Pronunciation)' \
        for an English novel, 'General American' for an American one). Every voice, narrator included, has that \
        accent unless the story gives a character another origin (a Russian count: Russian). For each character \
        give their sex exactly as the book presents them (use the pronouns the book uses; animals and creatures \
        too), their age, their accent, and a short description of the voice itself (pitch, timbre, pace, \
        temperament). When asked for the narrator, always cast one (never null): an adult \
        storyteller (never a child, even when the hero is one) chosen with judgment for the book, as a \
        good audiobook producer would: often, though not necessarily, matching the protagonist (a woman \
        for a story told around a girl or woman, a man for one around a man or boy); in a first-person \
        book, the protagonist as an adult. Give the narrator a voice with the energy the book's audience \
        wants: for a children's book an animated, warm, expressive storyteller full of wonder and fun, never \
        a dry or flat reader. Also settle how the book is told: \"book_style\", a short acting tag of three to six words for the \
        storytelling's usual delivery, suited to its readers (a children's book: e.g. 'bright, cheerful, \
        full of wonder'; a thriller: 'taut, low, urgent'). Reply with JSON only: {\"book_accent\": \"<accent>\", \"book_style\": \"<tag>\", \"characters\": {\"<name>\": \
        {\"sex\": \"male|female\", \"age\": \"<e.g. elderly, about 40, a young girl of 7>\", \"accent\": \"<accent>\", \
        \"nationality\": \"<who'd speak like that: e.g. an Englishman from London, an English girl, a Scottish woman>\", \
        \"accent_features\": \"<the accent's audible features, e.g. non-rhotic, crisp t's, long 'a' as in 'bath'>\", \
        \"voice\": \"<description>\"}}, \"narrator\": {same fields} or null}.";
    let who: String = names.iter().map(|n| format!("- {n}\n")).collect();
    let user = format!(
        "Book: {title} by {author}\n{}Characters to cast:\n{}{}\nSome of the text:\n{}",
        if cast.accent.is_empty() { String::new() } else { format!("The book's accent (already settled): {}\n", cast.accent) },
        if who.is_empty() { "(none)\n" } else { &who },
        if narrator { "Also cast the narrator (required: fill in \"narrator\").\n" } else { "" },
        context.chars().take(6000).collect::<String>(),
    );
    let reply = match crate::llm::chat(state, system, &[("user".into(), user)]).await.and_then(|t| json_in(&t)) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("casting: {e:#}");
            return;
        }
    };
    if cast.style.is_empty() {
        if let Some(a) = reply["book_style"].as_str().map(str::trim).filter(|a| !a.is_empty()) {
            cast.style = a.to_string();
        }
    }
    if cast.accent.is_empty() {
        if let Some(a) = reply["book_accent"].as_str().filter(|a| !a.is_empty()) {
            cast.accent = a.to_string();
        }
    }
    let describe = |v: &serde_json::Value, accent: &str| -> Option<String> {
        let sex = v["sex"].as_str().unwrap_or("").trim().to_lowercase();
        let sex = if sex.starts_with('f') { "female" } else if sex.starts_with('m') { "male" } else { "" };
        let age = v["age"].as_str().unwrap_or("").trim();
        // ("unknown" or "indeterminate" ages say nothing to the voice designer.)
        let age = if ["unknown", "indeterminate", "n/a", "unspecified"].iter().any(|u| age.to_lowercase().starts_with(u)) { "" } else { age };
        let acc = v["accent"].as_str().filter(|a| !a.trim().is_empty()).unwrap_or(accent).trim();
        // An accent, not commentary on how they talk ("RP with odd phrasing" → "RP").
        let acc = acc.split(" with ").next().unwrap_or(acc).trim();
        // The voice designer knows "British" better than "Received Pronunciation".
        let acc = if (acc.contains("Received Pronunciation") || acc == "RP" || acc.starts_with("RP ")) && !acc.contains("British") {
            "British (Received Pronunciation, like a BBC radio presenter)".to_string()
        } else {
            acc.to_string()
        };
        let acc = acc.as_str();
        let voice = v["voice"].as_str().unwrap_or("").trim().trim_end_matches('.');
        if sex.is_empty() && voice.is_empty() {
            return None;
        }
        // The accent said plainly and strongly, with who speaks like that and how it sounds: the
        // voice designer drifts to American unless it's spelled out (measured with an accent
        // classifier: "with a British accent" came out American; "an Englishman from London with
        // a strong, unmistakable British accent ... non-rhotic, crisp t's" came out English).
        let who = v["nationality"].as_str().unwrap_or("").trim().trim_end_matches('.');
        let features = v["accent_features"].as_str().unwrap_or("").trim().trim_end_matches('.');
        let mut d = String::new();
        if !sex.is_empty() {
            d.push_str(&format!("A {sex} voice"));
        } else {
            d.push_str("A voice");
        }
        if !age.is_empty() {
            d.push_str(&format!(", {age}"));
        }
        d.push('.');
        if !acc.is_empty() {
            let speaker = if who.is_empty() { "Speaks".to_string() } else { format!("{}{} speaking", who[..1].to_uppercase(), &who[1..]) };
            d.push_str(&format!(" {speaker} with a strong, unmistakable {acc} accent"));
            if !features.is_empty() {
                d.push_str(&format!(" ({features})"));
            }
            d.push('.');
        }
        d.push(' ');
        d.push_str(voice);
        d.push('.');
        Some(d)
    };
    let accent = cast.accent.clone();
    if let Some(chars) = reply["characters"].as_object() {
        for (n, v) in chars {
            let Some(name) = canonical(n, &cast.characters) else { continue };
            if let Some(d) = describe(v, &accent) {
                cast.characters.insert(name, d);
            }
        }
    }
    if narrator {
        if let Some(d) = describe(&reply["narrator"], &accent) {
            cast.narrator = d;
        }
    }
}

/// A narrator when casting didn't describe one: still in the book's accent.
fn narrator_default(accent: &str) -> String {
    if accent.is_empty() {
        "A warm, clear, expressive audiobook narrator.".into()
    } else {
        format!("A warm, clear, expressive audiobook narrator. Speaks with a strong, unmistakable {accent} accent.")
    }
}

/// Casts the whole book again (every character so far, and the narrator) with the careful
/// casting step, and drops their old voices and the lines performed with them.
pub async fn recast(state: &State, asset: i64) -> anyhow::Result<()> {
    let base = dir(state, asset);
    let cast_path = base.join("cast.json");
    let lock = lock_for(asset);
    let _held = lock.lock().await;
    let mut cast: Cast = read_json(&cast_path).await.unwrap_or_default();
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let w = db::work_detail(&state.db, a.work_id).await?.context("no such book")?;
    let src = book::source(&a, &state.data_dir);
    let text = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let mut out = String::new();
        for c in 0..40 {
            let Ok(s) = book::chapter_sentences(&src, asset, c) else { break };
            out.push_str(&s.join(" "));
            if out.len() > 6000 {
                break;
            }
        }
        Ok(out)
    })
    .await??;
    status(asset, "Casting the voices again", 0, 0);
    let names: Vec<String> = cast.characters.keys().cloned().collect();
    cast.accent.clear();
    cast.narrator.clear();
    cast_voices(state, &w.title, w.author.as_deref().unwrap_or(""), &names, true, &text, &mut cast).await;
    // (With many characters to cast the model sometimes leaves the narrator out: asked alone.)
    if cast.narrator.is_empty() {
        cast_voices(state, &w.title, w.author.as_deref().unwrap_or(""), &[], true, &text, &mut cast).await;
    }
    if cast.narrator.is_empty() {
        cast.narrator = narrator_default(&cast.accent);
    }
    write_json(&cast_path, &cast).await?;
    remember(state, asset, &cast).await;
    // The voices are made again from the new descriptions, and every line performed again.
    let tts = state.settings.read().await.ai.tts_url.clone();
    let mut voices = vec![json!({ "voice": voice_name(asset, None), "instructions": cast.narrator, "replace": true })];
    for (n, d) in &cast.characters {
        voices.push(json!({ "voice": voice_name(asset, Some(n)), "instructions": d, "replace": true }));
    }
    // (All in one request: the performer stops once for the lot, not once a voice.)
    status(asset, format!("Making the new voices ({})", voices.len()), 0, 0);
    state
        .http
        .post(format!("{}/voices", tts.trim_end_matches('/')))
        .json(&json!({ "voices": voices }))
        .timeout(std::time::Duration::from_secs(3600))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?
        .error_for_status()?;
    let mut entries = tokio::fs::read_dir(&base).await?;
    while let Some(e) = entries.next_entry().await? {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with(".mp3") || name.ends_with(".words.json") {
            let _ = tokio::fs::remove_file(e.path()).await;
        }
    }
    idle(asset);
    Ok(())
}

/// The book's full cast as the listener can see and change it: each member's description (what
/// their voice was made from), their voice's name (its clip is `/cast/clip/{voice}`), and how
/// the clip came out.
/// The full cast kept with the book too, so you can talk to them (Ask), each in their own voice.
async fn remember(state: &State, asset: i64, cast: &Cast) {
    let Ok(Some(a)) = db::asset(&state.db, asset).await else { return };
    let mut all = vec![("Narrator".to_string(), cast.narrator.clone(), voice_name(asset, None), true)];
    all.extend(cast.characters.iter().map(|(n, d)| (n.clone(), d.clone(), voice_name(asset, Some(n)), false)));
    for (name, description, voice, is_narrator) in all {
        if description.is_empty() {
            continue;
        }
        let m = duet_common::CastMember { id: 0, name, description, voice: Some(voice), is_narrator };
        if let Err(e) = db::upsert_cast(&state.db, a.work_id, &m).await {
            tracing::warn!("keeping the cast: {e:#}");
        }
    }
}

/// Books with a voice being made again from an edited description.
static REVOICING: LazyLock<std::sync::Mutex<std::collections::HashSet<i64>>> = LazyLock::new(Default::default);

/// Marks a voice as being made, before the work starts (so the listener's next look says so).
pub fn revoicing(asset: i64) {
    REVOICING.lock().unwrap().insert(asset);
}

pub async fn members(state: &State, asset: i64) -> serde_json::Value {
    let cast: Cast = read_json(&dir(state, asset).join("cast.json")).await.unwrap_or_default();
    remember(state, asset, &cast).await;
    let voices = state.data_dir.join("voices");
    let mut out = Vec::new();
    let mut add = |name: Option<&str>, description: &str| {
        let voice = voice_name(asset, name);
        let meta: serde_json::Value = std::fs::read(voices.join(format!("{voice}.json")))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(json!({}));
        out.push(json!({
            "name": name,
            "description": description,
            "voice": voice,
            "clip": voices.join(format!("{voice}.wav")).exists(),
            "engine": meta["engine"],
            "accent": meta["accent"],
        }));
    };
    if !cast.narrator.is_empty() {
        add(None, &cast.narrator);
    }
    for (n, d) in &cast.characters {
        add(Some(n), d);
    }
    json!({ "accent": cast.accent, "members": out, "busy": REVOICING.lock().unwrap().contains(&asset) })
}

/// A cast member's voice made again from a new description (`name` None: the narrator), and
/// the lines already performed thrown away so they're read in it.
pub async fn revoice(state: &State, asset: i64, name: Option<String>, description: String) -> anyhow::Result<()> {
    REVOICING.lock().unwrap().insert(asset);
    let r = revoice_one(state, asset, name, description).await;
    REVOICING.lock().unwrap().remove(&asset);
    r
}

async fn revoice_one(state: &State, asset: i64, name: Option<String>, description: String) -> anyhow::Result<()> {
    let base = dir(state, asset);
    let cast_path = base.join("cast.json");
    let lock = lock_for(asset);
    let _held = lock.lock().await;
    let mut cast: Cast = read_json(&cast_path).await.unwrap_or_default();
    let description = description.trim().to_string();
    match &name {
        None => cast.narrator = description.clone(),
        Some(n) => {
            anyhow::ensure!(cast.characters.contains_key(n), "no such character");
            cast.characters.insert(n.clone(), description.clone());
        }
    }
    write_json(&cast_path, &cast).await?;
    remember(state, asset, &cast).await;
    let who = name.clone().unwrap_or_else(|| "the narrator".into());
    status(asset, format!("Making {who}'s voice"), 0, 0);
    let tts = state.settings.read().await.ai.tts_url.clone();
    let made = state
        .http
        .post(format!("{}/voices", tts.trim_end_matches('/')))
        .json(&json!({ "voices": [{ "voice": voice_name(asset, name.as_deref()), "instructions": description, "replace": true }] }))
        .timeout(std::time::Duration::from_secs(1800))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")
        .and_then(|r| r.error_for_status().map_err(Into::into));
    if made.is_ok() {
        let mut entries = tokio::fs::read_dir(&base).await?;
        while let Some(e) = entries.next_entry().await? {
            let f = e.file_name().to_string_lossy().to_string();
            if f.ends_with(".mp3") || f.ends_with(".words.json") {
                let _ = tokio::fs::remove_file(e.path()).await;
            }
        }
    }
    idle(asset);
    made.map(|_| ())
}

/// The one name a character goes by for the whole book, so they keep one voice: the model
/// sometimes writes "Alice (reciting)", "the White Rabbit", "Narrator (reading address)" or
/// "March Hare and Hatter". Descriptions in brackets or after a dash go (they're directions),
/// "the" goes, a group is its first member, case is ignored against the characters already
/// cast, and the narrator in any guise is the narrator (None).
fn canonical(name: &str, known: &BTreeMap<String, String>) -> Option<String> {
    let mut n = name.trim();
    for cut in ["(", "[", " - ", " – ", " — ", ",", ";", ":"] {
        if let Some(i) = n.find(cut) {
            n = &n[..i];
        }
    }
    for sep in [" and ", " & ", "/"] {
        if let Some(i) = n.find(sep) {
            n = &n[..i];
        }
    }
    let mut n = n.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    for lead in ["the ", "The "] {
        if let Some(rest) = n.strip_prefix(lead) {
            n = rest.to_string();
        }
    }
    let key = n.to_lowercase();
    if key.is_empty() || key == "narrator" || key == "narration" || key == "unknown" {
        return None;
    }
    let simple = |k: &str| k.to_lowercase().trim_start_matches("the ").to_string();
    Some(known.keys().find(|k| simple(k) == key).cloned().unwrap_or(n))
}

fn voice_name(asset: i64, who: Option<&str>) -> String {
    let who = who.unwrap_or("narrator").to_lowercase();
    let slug: String = who.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    format!("cast{asset}-{}", slug.trim_matches('-'))
}

/// The script for the part of the chapter sentence `n` is in, and the cast, directing (and
/// casting) that part the first time.
async fn prepared(state: &State, asset: i64, chapter: usize, n: usize, sentences: &[String]) -> anyhow::Result<(Script, Cast)> {
    let base = dir(state, asset);
    let part = n / CHUNK;
    let cast_path = base.join("cast.json");
    // A part already directed doesn't wait for the director working on another.
    if let Some(script) = read_json::<Script>(&base.join(format!("script-{chapter}-{part}.json"))).await {
        return Ok((script, read_json(&cast_path).await.unwrap_or_default()));
    }
    let lock = lock_for(asset);
    let _held = lock.lock().await;
    let mut cast: Cast = read_json(&cast_path).await.unwrap_or_default();
    // (A whole chapter directed at once, as earlier versions did.)
    if let Some(script) = read_json::<Script>(&base.join(format!("script-{chapter}.json"))).await {
        return Ok((script, cast));
    }
    let script_path = base.join(format!("script-{chapter}-{part}.json"));
    if let Some(script) = read_json::<Script>(&script_path).await {
        return Ok((script, cast));
    }
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let w = db::work_detail(&state.db, a.work_id).await?.context("no such book")?;
    let (mut say, mut narration) = (Say::new(), Say::new());
    let script = match direct(state, asset, chapter, part, &w.title, w.author.as_deref().unwrap_or(""), sentences, &mut cast, &mut say, &mut narration).await {
        Ok(s) => {
            write_json(&base.join(format!("say-{chapter}-{part}.json")), &say).await.ok();
            write_json(&base.join(format!("narration-{chapter}-{part}.json")), &narration).await.ok();
            s
        }
        Err(e) => {
            // No chat model (or it failed): the narrator reads this chapter; try directing again later.
            tracing::warn!("directing chapter {chapter}: {e:#}");
            idle(asset);
            return Ok((Script::new(), cast));
        }
    };
    if cast.narrator.is_empty() {
        let context: String = sentences.iter().take(60).map(String::as_str).collect::<Vec<_>>().join(" ");
        cast_voices(state, &w.title, w.author.as_deref().unwrap_or(""), &[], true, &context, &mut cast).await;
    }
    if cast.narrator.is_empty() {
        cast.narrator = narrator_default(&cast.accent);
    }
    // Cast everyone in this part up front, so the voice server isn't swapping models mid-part.
    // (One at a time, so the player can say whose voice is being made.)
    // Every character met so far, not just this part's: making a voice means stopping the
    // performer (they can't share the card), so any still missing are all made in that one stop.
    // (Ones already made are skipped at once.)
    let mut voices = vec![("the narrator".to_string(), json!({ "voice": voice_name(asset, None), "instructions": cast.narrator }))];
    let mut everyone: std::collections::BTreeSet<String> = script.values().map(|(s, _)| s.clone()).collect();
    everyone.extend(cast.characters.keys().cloned());
    for who in &everyone {
        let desc = cast.characters.get(who).cloned().unwrap_or_default();
        voices.push((who.clone(), json!({ "voice": voice_name(asset, Some(who)), "instructions": desc })));
    }
    let tts = state.settings.read().await.ai.tts_url.clone();
    let total = voices.len() as u32;
    for (k, (who, v)) in voices.into_iter().enumerate() {
        // (One already made isn't asked for: the voice server may be busy warming up the
        // performer, and the question would wait for that.)
        let made = v["voice"].as_str().is_some_and(|n| state.data_dir.join("voices").join(format!("{n}.wav")).exists());
        if made {
            continue;
        }
        status(asset, format!("Finding a voice for {who}"), k as u32, total);
        state
            .http
            .post(format!("{}/voices", tts.trim_end_matches('/')))
            .json(&json!({ "voices": [v] }))
            .timeout(std::time::Duration::from_secs(1800))
            .send()
            .await
            .context("the voice server isn't running (Settings → Voices)")?;
    }
    write_json(&cast_path, &cast).await?;
    remember(state, asset, &cast).await;
    write_json(&script_path, &script).await?;
    Ok((script, cast))
}

/// When each word of a performed sentence starts in its recording, for the read-along: timed
/// by the forced aligner once and kept beside the recording.
pub async fn words(state: &State, asset: i64, chapter: usize, n: usize) -> anyhow::Result<Vec<f64>> {
    let base = dir(state, asset);
    let path = base.join(format!("{chapter}-{n}.words.json"));
    if let Some(w) = read_json::<Vec<f64>>(&path).await {
        return Ok(w);
    }
    let mp3 = base.join(format!("{chapter}-{n}.mp3"));
    anyhow::ensure!(tokio::fs::try_exists(&mp3).await.unwrap_or(false), "not performed yet");
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    let src = book::source(&a, &state.data_dir);
    let sentences = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, chapter)).await??;
    let text = sentences.get(n).cloned().context("no such sentence")?;
    let tts = state.settings.read().await.ai.tts_url.clone();
    let v: serde_json::Value = state
        .http
        .post(format!("{}/align-words", tts.trim_end_matches('/')))
        .json(&json!({ "audio": mp3, "text": text }))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?
        .error_for_status()?
        .json()
        .await?;
    let w: Vec<f64> = serde_json::from_value(v["words"].clone()).unwrap_or_default();
    write_json(&path, &w).await.ok();
    Ok(w)
}

/// Lines being performed right now: a second request for one waits for the first.
static PERFORMING: LazyLock<std::sync::Mutex<HashMap<(i64, usize, usize), Arc<Mutex<()>>>>> = LazyLock::new(Default::default);

/// Where the listener is in each book (chapter, sentence), and a count bumped when they move,
/// for the performer working ahead in the background.
static LISTENER: LazyLock<std::sync::Mutex<HashMap<i64, (usize, usize, u64)>>> = LazyLock::new(Default::default);

/// Keeps performing ahead of the listener, in the background, while the card is free: on through
/// this chapter and into the next, a line at a time, so the lines are ready by the time they're
/// reached even though performing takes longer than listening. Stops when the listener moves
/// elsewhere (a new one starts from there).
/// Books whose director is reading ahead.
static DIRECTING: LazyLock<std::sync::Mutex<std::collections::HashSet<i64>>> = LazyLock::new(Default::default);

/// The director reads ahead of the performers, through the whole book from here: the
/// chat model works on the CPU while the performer has the card (they don't both fit), a minute
/// or two a part, so it's done in parallel rather than as each part comes up.
fn direct_ahead(state: &State, asset: i64, chapter: usize) {
    if !DIRECTING.lock().unwrap().insert(asset) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let base = dir(&state, asset);
        // From here to the end of the book, then the beginning.
        let chapters = match db::asset(&state.db, asset).await {
            Ok(Some(a)) => {
                let src = book::source(&a, &state.data_dir);
                tokio::task::spawn_blocking(move || book::layout(&src).map(|l| l.chapters)).await.ok().and_then(Result::ok).unwrap_or(chapter + 3)
            }
            _ => chapter + 3,
        };
        for c in (chapter..chapters).chain(0..chapter) {
            let Ok(Some(a)) = db::asset(&state.db, asset).await else { break };
            let src = book::source(&a, &state.data_dir);
            let Ok(Ok(sentences)) = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, c)).await else { break };
            for part in 0..sentences.len().div_ceil(CHUNK) {
                let done = base.join(format!("script-{c}-{part}.json")).exists() || base.join(format!("script-{c}.json")).exists();
                if !done {
                    if let Err(e) = prepared(&state, asset, c, part * CHUNK, &sentences).await {
                        tracing::warn!("directing ahead ({c}, part {part}): {e:#}");
                    }
                }
            }
        }
        DIRECTING.lock().unwrap().remove(&asset);
    });
}

fn work_ahead(state: &State, asset: i64, chapter: usize, n: usize) {
    let generation = {
        let mut l = LISTENER.lock().unwrap();
        let entry = l.entry(asset).or_insert((chapter, n, 0));
        // Still in the stretch being worked on: nothing new to start.
        if entry.0 == chapter && n >= entry.1 && n < entry.1 + 40 && entry.2 > 0 {
            return;
        }
        *entry = (chapter, n, entry.2 + 1);
        entry.2
    };
    direct_ahead(state, asset, chapter);
    let state = state.clone();
    tokio::spawn(async move {
        let moved = || LISTENER.lock().unwrap().get(&asset).map(|e| e.2) != Some(generation);
        for c in chapter..chapter + 2 {
            let Ok(Some(a)) = db::asset(&state.db, asset).await else { return };
            let src = book::source(&a, &state.data_dir);
            let Ok(Ok(sentences)) = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, c)).await else { return };
            let from = if c == chapter { n } else { 0 };
            for k in from..sentences.len() {
                if moved() {
                    return;
                }
                // (If the voices are restarting, wait for them rather than give up: giving up left
                // the listener's stretch marked as being worked on, so nothing more was performed.)
                let mut tries = 0;
                while let Err(e) = perform(&state, asset, c, k, false).await {
                    tries += 1;
                    if moved() || tries > 12 {
                        tracing::warn!("performing ahead ({c}, {k}): {e:#}");
                        LISTENER.lock().unwrap().remove(&asset);
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
                if let Some(entry) = LISTENER.lock().unwrap().get_mut(&asset).filter(|e| e.2 == generation) {
                    // (Lets a request a little further on count as the same stretch.)
                    if entry.0 == c {
                        entry.1 = entry.1.max(k.saturating_sub(20));
                    }
                }
            }
        }
    });
}

/// One sentence, performed: MP3 bytes, from the cache when it's been performed before. Asking
/// for one also sets the performer working ahead from it.
pub async fn sentence(state: &State, asset: i64, chapter: usize, n: usize) -> anyhow::Result<Vec<u8>> {
    work_ahead(state, asset, chapter, n);
    perform(state, asset, chapter, n, true).await
}

/// `urgent`: the listener is waiting for it. Plain narration then doesn't wait for the
/// director (a minute or two for a part when the chat model is on the CPU): it's read in the
/// book's own style.
async fn perform(state: &State, asset: i64, chapter: usize, n: usize, urgent: bool) -> anyhow::Result<Vec<u8>> {
    let path = dir(state, asset).join(format!("{chapter}-{n}.mp3"));
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes);
    }
    let line = PERFORMING.lock().unwrap().entry((asset, chapter, n)).or_default().clone();
    let _one = line.lock().await;
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes);
    }
    let a = db::asset(&state.db, asset).await?.context("no such book")?;
    if a.format == "transcript" {
        crate::audiotext::ensure_chapter(state, &a, chapter).await?;
    }
    let src = book::source(&a, &state.data_dir);
    let sentences = tokio::task::spawn_blocking(move || book::chapter_sentences(&src, asset, chapter)).await??;
    let printed = sentences.get(n).cloned().context("no such sentence")?;
    let plain = !printed.contains(['“', '”', '"']) && open_quote_before(&sentences, n).is_none() && !printed.chars().any(|c| c.is_ascii_digit());
    let base = dir(state, asset);
    let directed = base.join(format!("script-{chapter}-{}.json", n / CHUNK)).exists() || base.join(format!("script-{chapter}.json")).exists();
    let cast_now: Cast = read_json(&base.join("cast.json")).await.unwrap_or_default();
    let narrator_ready = !cast_now.narrator.is_empty() && state.data_dir.join("voices").join(format!("{}.wav", voice_name(asset, None))).exists();
    let (script, cast) = if urgent && plain && !directed && narrator_ready {
        direct_ahead(state, asset, chapter);
        (Script::new(), cast_now)
    } else {
        prepared(state, asset, chapter, n, &sentences).await?
    };
    // The director's wording where the print would be misread; the rules catch headings anyway.
    let say: Say = read_json(&dir(state, asset).join(format!("say-{chapter}-{}.json", n / CHUNK))).await.unwrap_or_default();
    let text = speakable(say.get(&n).unwrap_or(&printed));
    // (Scripts directed before names were tidied: the same one voice per character.)
    // A sentence inside a speech begun earlier is that speaker's from its start (to the close).
    let opened = open_quote_before(&sentences, n);
    let mut line = script.get(&n).cloned();
    if let Some(k) = opened.filter(|_| line.is_none()) {
        line = if k / CHUNK == n / CHUNK {
            script.get(&k).cloned()
        } else {
            read_json::<Script>(&dir(state, asset).join(format!("script-{chapter}-{}.json", k / CHUNK))).await.and_then(|s| s.get(&k).cloned())
        };
    }
    let line = line.and_then(|(who, dir)| canonical(&who, &cast.characters).map(|w| (w, dir)));
    let text = if opened.is_some() && line.is_some() { format!("“{text}") } else { text };
    let narration: Say = read_json(&dir(state, asset).join(format!("narration-{chapter}-{}.json", n / CHUNK))).await.unwrap_or_default();
    let parts: Vec<_> = pieces(&text, line.as_ref(), narration.get(&n).map(String::as_str).unwrap_or(&cast.style))
        .into_iter()
        .map(|(t, who, direction)| {
            let input = if direction.is_empty() { t } else { format!("[{direction}] {t}") };
            let desc = who.as_ref().and_then(|w| cast.characters.get(w)).unwrap_or(&cast.narrator);
            json!({ "input": input, "voice": voice_name(asset, who.as_deref()), "instructions": desc })
        })
        .collect();
    let tts = state.settings.read().await.ai.tts_url.clone();
    status(asset, "Performing the next line", 0, 0);
    let resp = state
        .http
        .post(format!("{}/audio/script", tts.trim_end_matches('/')))
        .json(&json!({ "pieces": parts, "response_format": "mp3", "pause": 0.3 }))
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await
        .context("the voice server isn't running (Settings → Voices)")?;
    if !resp.status().is_success() {
        idle(asset);
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        anyhow::bail!("voice server: {}", v["error"].as_str().unwrap_or("failed"));
    }
    let bytes = resp.bytes().await?.to_vec();
    idle(asset);
    write_json(&path, &()).await.ok(); // makes the directory
    tokio::fs::write(&path, &bytes).await?;
    // Its words timed straight away, ready for the read-along when it plays.
    let st = state.clone();
    tokio::spawn(async move {
        let _ = words(&st, asset, chapter, n).await;
    });
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{canonical, open_quote_before, pieces, replace_words, speakable};

    #[test]
    fn roman_numerals_read_as_numbers() {
        assert_eq!(replace_words("George III and IIIa", "III", "the Third"), "George the Third and IIIa");
        assert_eq!(speakable("XII."), "Chapter twelve.");
        assert_eq!(speakable("I"), "Chapter one.");
        assert_eq!(speakable("Chapter IV"), "Chapter four");
        assert_eq!(speakable("I said so."), "I said so.");
        assert_eq!(speakable("Book II: The Pool"), "Book two: The Pool");
    }
    use std::collections::BTreeMap;

    #[test]
    fn one_name_per_character() {
        let known: BTreeMap<String, String> = [("Alice".to_string(), String::new()), ("White Rabbit".to_string(), String::new())].into();
        assert_eq!(canonical("Alice (reciting)", &known).as_deref(), Some("Alice"));
        assert_eq!(canonical("the white rabbit", &known).as_deref(), Some("White Rabbit"));
        assert_eq!(canonical("Narrator (reading address)", &known), None);
        assert_eq!(canonical("March Hare and Hatter", &known).as_deref(), Some("March Hare"));
        assert_eq!(canonical("Dodo", &known).as_deref(), Some("Dodo"));
    }

    #[test]
    fn splits_dialogue_from_narration() {
        let line = ("Alice".to_string(), "curious".to_string());
        let p = pieces("“Curiouser and curiouser!” cried Alice.", Some(&line), "");
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, "Curiouser and curiouser!");
        assert_eq!(p[0].1.as_deref(), Some("Alice"));
        assert_eq!(p[1].0, "cried Alice.");
        assert!(p[1].1.is_none());
    }

    #[test]
    fn speech_carries_over_sentences() {
        let s: Vec<String> = ["the Rabbit say to itself, “Oh dear!", "Oh dear!", "I shall be late!” (when she thought", "Next."]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(open_quote_before(&s, 1), Some(0));
        assert_eq!(open_quote_before(&s, 2), Some(0));
        assert_eq!(open_quote_before(&s, 3), None);
        let line = ("White Rabbit".to_string(), "flustered".to_string());
        let p = pieces(&format!("“{}", s[2]), Some(&line), "");
        assert_eq!(p[0].0, "I shall be late!");
        assert_eq!(p[0].1.as_deref(), Some("White Rabbit"));
        assert_eq!(p[1].0, "(when she thought");
        assert!(p[1].1.is_none());
    }
}
