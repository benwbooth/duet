//! The full cast for a book: who reads what, the description each voice was made from, and its
//! clip to hear. A description can be changed and that one voice made again from it.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;

use crate::{api, toast};

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Member {
    /// None: the narrator.
    name: Option<String>,
    description: String,
    voice: String,
    clip: bool,
    #[serde(default)]
    engine: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Members {
    #[serde(default)]
    accent: String,
    #[serde(default)]
    members: Vec<Member>,
    #[serde(default)]
    busy: bool,
}

fn engine_label(e: &str) -> &str {
    match e {
        "omnivoice" => "OmniVoice",
        "maya1" => "Maya1",
        "qwen3-voicedesign" => "Qwen3-TTS",
        other => other,
    }
}

#[component]
pub fn CastPanel(asset: i64, on_close: impl Fn() + Clone + Send + Sync + 'static) -> impl IntoView {
    let toast = toast();
    let cast = RwSignal::new(None::<Members>);
    // Bumped when voices are remade, so the clips are fetched again.
    let version = RwSignal::new(0u32);
    // The member whose voice is being made again (by voice name).
    let remaking = RwSignal::new(None::<String>);
    let load = move || {
        spawn_local(async move {
            if let Ok(m) = api::get::<Members>(&format!("/api/assets/{asset}/cast/members")).await {
                let busy = m.busy;
                let _ = cast.try_set(Some(m));
                if !busy && remaking.get_untracked().is_some() {
                    remaking.set(None);
                    version.update(|v| *v += 1);
                }
            }
        });
    };
    load();
    // While a voice is being made, check every couple of seconds for it.
    let poll = set_interval_with_handle(
        move || {
            if remaking.get_untracked().is_some() {
                load();
            }
        },
        std::time::Duration::from_secs(2),
    )
    .ok();
    on_cleanup(move || {
        if let Some(p) = poll {
            p.clear();
        }
    });

    view! {
        <div class="editions-panel cast-panel">
            <div class="cast-head">
                <h4>"Voice cast"</h4>
                <button class="icon-button" title="Close" on:click={
                    let on_close = on_close.clone();
                    move |_| on_close()
                }>"×"</button>
            </div>
            <p class="cast-note">
                "Each voice is made from its description, then performs every line in it. Change a description and remake the voice to recast someone; their lines are read again in the new voice."
                {move || cast.get().filter(|c| !c.accent.is_empty()).map(|c| view! { <br/><span>"Book's accent: " {c.accent}</span> })}
            </p>
            {move || match cast.get() {
                None => view! { <p class="cast-note">"Loading…"</p> }.into_any(),
                Some(c) if c.members.is_empty() => view! {
                    <p class="cast-note">"No cast yet: it's chosen as the book is first read in full cast."</p>
                }.into_any(),
                Some(c) => c.members.into_iter().map(|m| {
                    let draft = RwSignal::new(m.description.clone());
                    let original = m.description.clone();
                    let (voice, voice2) = (m.voice.clone(), m.voice.clone());
                    let name = m.name.clone();
                    let title = m.name.clone().unwrap_or_else(|| "Narrator".into());
                    let engine = m.engine.as_deref().map(engine_label).map(str::to_string);
                    let busy = move || remaking.get().as_deref() == Some(voice.as_str());
                    let remake = move |_| {
                        let (name, description, voice) = (name.clone(), draft.get_untracked(), voice2.clone());
                        remaking.set(Some(voice));
                        spawn_local(async move {
                            let body = serde_json::json!({ "name": name, "description": description });
                            if let Err(e) = api::post::<_, serde_json::Value>(&format!("/api/assets/{asset}/cast/voice"), &body).await {
                                toast.show(e.0);
                                remaking.set(None);
                            }
                        });
                    };
                    let src = {
                        let v = m.voice.clone();
                        move || format!("/api/assets/{asset}/cast/clip/{v}?v={}", version.get())
                    };
                    view! {
                        <div class="cast-member">
                            <div class="cast-member-head">
                                <strong>{title}</strong>
                                {engine.map(|e| view! { <small>{e}</small> })}
                            </div>
                            {m.clip.then(|| view! { <audio controls preload="none" src=src></audio> })}
                            <textarea rows="6" prop:value=move || draft.get()
                                on:input=move |ev| draft.set(event_target_value(&ev))></textarea>
                            <div class="cast-actions">
                                {move || busy().then(|| view! { <span class="cast-busy">"Making the voice… (a minute or two)"</span> })}
                                <button class="button primary" disabled=move || remaking.get().is_some()
                                    on:click=remake>
                                    {let o = original.clone(); move || if draft.get() != o { "Remake with this description" } else { "Remake voice" }}
                                </button>
                            </div>
                        </div>
                    }
                }).collect_view().into_any(),
            }}
        </div>
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct PictureStyle {
    #[serde(default)]
    style: String,
    #[serde(default)]
    characters: std::collections::BTreeMap<String, String>,
}

/// The book's pictures: the art style they're all drawn in (yours to change: every picture is
/// drawn again in the new one) and how each character looks in them.
#[component]
pub fn PicturesPanel(asset: i64, on_close: impl Fn() + Clone + Send + Sync + 'static) -> impl IntoView {
    let toast = toast();
    let style = RwSignal::new(None::<PictureStyle>);
    let draft = RwSignal::new(String::new());
    let saved = RwSignal::new(false);
    spawn_local(async move {
        match api::get::<PictureStyle>(&format!("/api/assets/{asset}/illustrations/style")).await {
            Ok(s) => {
                draft.set(s.style.clone());
                style.set(Some(s));
            }
            Err(e) => toast.show(e.0),
        }
    });
    let save = move |_| {
        let text = draft.get_untracked();
        spawn_local(async move {
            match api::put::<_, serde_json::Value>(&format!("/api/assets/{asset}/illustrations/style"), &serde_json::json!({ "style": text })).await {
                Ok(_) => saved.set(true),
                // (No body comes back: that's fine.)
                Err(e) if e.0.contains("EOF") || e.0.contains("JSON") => saved.set(true),
                Err(e) => toast.show(e.0),
            }
        });
    };
    view! {
        <div class="editions-panel cast-panel">
            <div class="cast-head">
                <h4>"Pictures"</h4>
                <button class="icon-button" title="Close" on:click={
                    let on_close = on_close.clone();
                    move |_| on_close()
                }>"×"</button>
            </div>
            <p class="cast-note">
                "An illustration director reads each chapter and picks its scenes, about one every few pages, and every picture is drawn in this one style, with each character looking the same throughout. They're drawn on your graphics card a chapter or two ahead of you (not while the full cast is performing, which needs the card), and appear as you reach each chapter."
            </p>
            {move || match style.get() {
                None => view! { <p class="cast-note">"Choosing a style…"</p> }.into_any(),
                Some(s) => view! {
                    <div class="cast-member">
                        <div class="cast-member-head"><strong>"Art style"</strong></div>
                        <textarea rows="4" prop:value=move || draft.get() on:input=move |ev| { saved.set(false); draft.set(event_target_value(&ev)) }></textarea>
                        <div class="cast-actions">
                            {move || saved.get().then(|| view! { <span class="cast-busy">"Saved: the pictures are being drawn again."</span> })}
                            <button class="button primary" disabled=move || draft.get().trim().is_empty() on:click=save>"Use this style and redraw"</button>
                        </div>
                    </div>
                    {(!s.characters.is_empty()).then(|| view! {
                        <div class="cast-member">
                            <div class="cast-member-head"><strong>"How the characters look"</strong></div>
                            {s.characters.into_iter().map(|(n, l)| {
                                let src = format!("/api/assets/{asset}/illustrations/portrait/{}", js_sys::encode_uri_component(&n));
                                view! {
                                    <div class="look">
                                        <img src=src alt="" onerror="this.style.display='none'"/>
                                        <p><strong>{n}</strong>": "{l}</p>
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                    })}
                }.into_any(),
            }}
        </div>
    }
}
