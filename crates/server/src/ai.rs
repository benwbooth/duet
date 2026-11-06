//! The voice assistant: speech to text, a chat model that knows the book and where the reader
//! is, and text to speech in the narrator's or a character's voice. All three are
//! OpenAI-compatible HTTP endpoints configured in settings.

use anyhow::Context;
use base64::Engine;
use duet_common::{AskReply, AskRequest, AssetKind, CastMember};
use serde_json::json;

use crate::{State, book, db};

/// How much of the book before the reader's position goes into the prompt.
const CONTEXT_SENTENCES: usize = 80;

/// Talking to the companion is transcribed as you speak, so it uses a small, quick model: the
/// accuracy setting's big models take tens of seconds a clip without a GPU CTranslate2 can use.
const DICTATION_MODEL: &str = "Systran/faster-whisper-small.en";

pub async fn transcribe(state: &State, audio: Vec<u8>, filename: String, mime: String) -> anyhow::Result<String> {
    let ai = state.settings.read().await.ai.clone();
    // The bundled voice server does speech recognition too, so it's the fallback when the
    // configured speech-to-text server isn't running.
    let mut urls = vec![ai.stt_url.clone()];
    if ai.tts_url.trim_end_matches('/') != ai.stt_url.trim_end_matches('/') {
        urls.push(ai.tts_url.clone());
    }
    let mut last = None;
    for url in urls {
        let part = reqwest::multipart::Part::bytes(audio.clone()).file_name(filename.clone()).mime_str(&mime)?;
        let form = reqwest::multipart::Form::new().part("file", part).text("model", DICTATION_MODEL);
        match state
            .http
            .post(format!("{}/audio/transcriptions", url.trim_end_matches('/')))
            .bearer_auth(&ai.llm_api_key)
            .multipart(form)
            .send()
            .await
        {
            Ok(resp) => {
                let v: serde_json::Value = resp.error_for_status()?.json().await?;
                return Ok(v["text"].as_str().unwrap_or("").trim().to_string());
            }
            Err(e) => last = Some(e),
        }
    }
    Err(anyhow::Error::from(last.unwrap())).context("speech-to-text server unreachable (Settings → Voices)")
}

pub async fn speak(state: &State, text: &str, voice: &str, instructions: Option<&str>) -> anyhow::Result<String> {
    let ai = state.settings.read().await.ai.clone();
    let mut body = json!({ "model": ai.tts_model, "input": text, "voice": voice, "response_format": "mp3" });
    // Voice-design TTS servers (Qwen3-TTS VoiceDesign, VoxCPM...) can build a voice from this.
    if let Some(i) = instructions.filter(|i| !i.is_empty()) {
        body["instructions"] = json!(i);
    }
    let bytes = state
        .http
        .post(format!("{}/audio/speech", ai.tts_url.trim_end_matches('/')))
        .bearer_auth(&ai.llm_api_key)
        .json(&body)
        .send()
        .await
        .context("text-to-speech server unreachable")?
        .error_for_status()?
        .bytes()
        .await?;
    Ok(format!("data:audio/mpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)))
}

/// The text just before the reader's position, so answers stay spoiler-free and in context.
async fn recent_text(state: &State, work_id: i64, chapter: usize, sentence: usize) -> anyhow::Result<String> {
    let assets = db::work_assets(&state.db, work_id).await?;
    let Some(ebook) = assets.iter().find(|a| a.kind == AssetKind::Ebook && !matches!(book::source(a, &state.data_dir), book::Source::Pdf(_) | book::Source::Comic(_) | book::Source::NeedsConversion)) else {
        return Ok(String::new());
    };
    let src = book::source(ebook, &state.data_dir);
    let id = ebook.id;
    let sentences = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        // Pull from earlier chapters when the current one has only just started.
        for c in (0..=chapter).rev() {
            let all = book::chapter_sentences(&src, id, c)?;
            let upto = if c == chapter { sentence.min(all.len()) + 1 } else { all.len() };
            let mut part: Vec<String> = all.into_iter().take(upto).collect();
            part.extend(out);
            out = part;
            if out.len() >= CONTEXT_SENTENCES {
                break;
            }
        }
        let skip = out.len().saturating_sub(CONTEXT_SENTENCES);
        Ok(out.split_off(skip))
    })
    .await??;
    Ok(sentences.join(" "))
}

pub async fn ask(state: &State, req: AskRequest) -> anyhow::Result<AskReply> {
    let ai = state.settings.read().await.ai.clone();
    let work = match req.work_id {
        Some(id) => db::work_detail(&state.db, id).await?,
        None => None,
    };
    let persona: Option<CastMember> = match (&work, req.persona) {
        (Some(w), Some(p)) => w.cast.iter().find(|c| c.id == p).cloned(),
        _ => None,
    };
    let mut system = String::new();
    if let Some(w) = &work {
        let by = w.author.as_deref().map(|a| format!(" by {a}")).unwrap_or_default();
        let context = match &req.position {
            Some(p) => recent_text(state, w.id, p.chapter, p.sentence).await.unwrap_or_default(),
            None => String::new(),
        };
        match &persona {
            Some(c) if !c.is_narrator => system.push_str(&format!(
                "You are {}, a character in \"{}\"{by}. {}\nStay in character and speak as {} would. You only know what has \
                 happened up to the reader's current place in the story; never reveal later events. Begin every \
                 reply with a short acting direction in square brackets describing how you say it, e.g. \
                 [warily, keeping my voice low].",
                c.name, w.title, c.description, c.name
            )),
            Some(_) => system.push_str(&format!(
                "You are the narrator of \"{}\"{by}. Answer in the narrator's voice and style, without revealing anything \
                 beyond the reader's current place in the story.",
                w.title
            )),
            None => system.push_str(&format!(
                "You are a reading companion for \"{}\"{by}. Answer questions about the book, explain references, and \
                 define words as they are used in the passage. Be concise: answers are read aloud. Never spoil events \
                 beyond the reader's current place unless they explicitly ask for spoilers.",
                w.title
            )),
        }
        if !context.is_empty() {
            system.push_str(&format!("\n\nThe reader has just read this passage:\n\"\"\"\n{context}\n\"\"\""));
        }
    } else {
        system.push_str("You are a helpful reading companion. Be concise: answers are read aloud.");
    }
    let mut turns: Vec<crate::llm::Turn> = req.history.iter().map(|t| (t.role.clone(), t.content.clone())).collect();
    turns.push(("user".into(), req.question.clone()));
    let mut answer = crate::llm::chat(state, &system, &turns).await?;
    // Characters open with an acting direction ("[wry, a little tired]") for the voice engine;
    // it's spoken, not shown.
    let spoken = answer.clone();
    if let Some((_, text)) = answer.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
        answer = text.trim().to_string();
    }
    let speaker = persona.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Assistant".into());
    let audio = if req.speak {
        let (voice, instructions) = voice_for(&ai, persona.as_ref());
        match speak(state, &spoken, &voice, instructions.as_deref()).await {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!("tts: {e:#}");
                None
            }
        }
    } else {
        None
    };
    Ok(AskReply { question: req.question, answer, audio, speaker, spoken })
}

/// The voice an answer is spoken in: the character's own, or the narrator's.
fn voice_for(ai: &duet_common::AiSettings, persona: Option<&CastMember>) -> (String, Option<String>) {
    match persona {
        Some(p) => (p.voice.clone().unwrap_or_else(|| ai.narrator_voice.clone()), Some(p.description.clone())),
        None => (ai.narrator_voice.clone(), Some(ai.assistant_voice.clone())),
    }
}

/// Speaks an answer that's already been shown.
pub async fn say(state: &State, req: duet_common::SayRequest) -> anyhow::Result<String> {
    let ai = state.settings.read().await.ai.clone();
    let work = match req.work_id {
        Some(id) => db::work_detail(&state.db, id).await?,
        None => None,
    };
    let persona = match (&work, req.persona) {
        (Some(w), Some(p)) => w.cast.iter().find(|c| c.id == p).cloned(),
        _ => None,
    };
    let (voice, instructions) = voice_for(&ai, persona.as_ref());
    speak(state, &req.text, &voice, instructions.as_deref()).await
}

/// Model names an OpenAI-compatible server offers (GET {url}/models), for the settings picker.
pub async fn list_models(http: &reqwest::Client, url: &str, key: &str) -> anyhow::Result<Vec<String>> {
    let resp: serde_json::Value = http
        .get(format!("{}/models", url.trim_end_matches('/')))
        .bearer_auth(key)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .context("nothing answered at that address")?
        .error_for_status()?
        .json()
        .await
        .context("that address isn't an OpenAI-compatible API")?;
    let mut names: Vec<String> =
        resp["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
    names.sort();
    Ok(names)
}
