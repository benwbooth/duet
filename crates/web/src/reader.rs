//! The reader and player. With a sync map, the sentence being spoken is highlighted (and followed
//! across chapters), and tapping a sentence jumps the audio there.

use duet_common::{
    AskReply, AskRequest, Asset, AssetKind, BookLayout, Chapter, ChatTurn, Position, ReadMode, Settings, SyncMap,
    SyncSpan, WorkDetail,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::{JsCast, JsValue};

use crate::{api, format_time, toast};

const READABLE: [&str; 6] = ["epub", "txt", "md", "html", "htm", "xhtml"];

/// The edition to read: an EPUB or text first, then PDF, then anything (shown as unsupported).
fn pick_ebook(assets: &[Asset]) -> Option<Asset> {
    let ebooks = || assets.iter().filter(|a| a.kind == AssetKind::Ebook);
    ebooks()
        .find(|a| READABLE.contains(&a.format.as_str()) && a.origin != "generated")
        .or_else(|| ebooks().find(|a| READABLE.contains(&a.format.as_str())))
        .or_else(|| ebooks().find(|a| a.format == "pdf"))
        .or_else(|| ebooks().next())
        .cloned()
}

fn pick_audio(assets: &[Asset], sync: Option<&SyncMap>) -> Option<Asset> {
    // The synced edition, if there is one.
    let synced = sync.and_then(|s| assets.iter().find(|a| a.id == s.audiobook_asset));
    synced.or_else(|| assets.iter().find(|a| a.kind == AssetKind::Audiobook)).cloned()
}

/// Sync spans in playback order, for looking up what's being said at a time.
#[derive(Clone, Default)]
struct SyncIndex {
    by_time: Vec<SyncSpan>,
}

impl SyncIndex {
    fn new(mut spans: Vec<SyncSpan>) -> Self {
        spans.sort_by(|a, b| (a.track, a.start).partial_cmp(&(b.track, b.start)).unwrap());
        SyncIndex { by_time: spans }
    }

    fn at(&self, track: usize, t: f64) -> Option<&SyncSpan> {
        let i = self.by_time.partition_point(|s| (s.track, s.start) <= (track, t + 0.05));
        self.by_time[..i].last().filter(|s| s.track == track && t <= s.end + 2.0)
    }

    fn of(&self, chapter: usize, sentence: usize) -> Option<&SyncSpan> {
        self.by_time.iter().find(|s| s.chapter == chapter && s.sentence == sentence)
    }

    fn is_empty(&self) -> bool {
        self.by_time.is_empty()
    }
}

#[derive(Clone, PartialEq)]
struct Message {
    speaker: String,
    text: String,
    mine: bool,
}

#[component]
pub fn Reader(id: i64) -> impl IntoView {
    let work = RwSignal::new(None::<WorkDetail>);
    let ebook = RwSignal::new(None::<Asset>);
    let audio_asset = RwSignal::new(None::<Asset>);
    let layout = RwSignal::new(None::<BookLayout>);
    let sync = RwSignal::new(SyncIndex::default());
    let prefs = RwSignal::new(Settings::default().reader);

    let chapter = RwSignal::new(usize::MAX);
    let html = RwSignal::new(String::new());
    let sentence = RwSignal::new(-1i32);
    // Where to scroll once the chapter's HTML is in the page.
    let scroll_to = RwSignal::new(None::<u32>);

    let track = RwSignal::new(0usize);
    let time = RwSignal::new(0.0f64);
    let duration = RwSignal::new(0.0f64);
    let playing = RwSignal::new(false);
    let rate = RwSignal::new(1.0f64);
    let pending_seek = RwSignal::new(None::<(f64, bool)>);
    let toc_open = RwSignal::new(false);
    let font_size = RwSignal::new(19u32);

    let page = NodeRef::<leptos::html::Article>::new();
    let audio = NodeRef::<leptos::html::Audio>::new();
    let toast = toast();

    // Load everything, then restore the saved position.
    spawn_local(async move {
        let w = match api::get::<WorkDetail>(&format!("/api/works/{id}")).await {
            Ok(w) => w,
            Err(e) => return toast.show(e.0),
        };
        if let Ok(s) = api::get::<Settings>("/api/settings").await {
            font_size.set(s.reader.font_size);
            rate.set(s.reader.playback_rate);
            prefs.set(s.reader);
        }
        let map = api::get::<Option<SyncMap>>(&format!("/api/works/{id}/sync")).await.ok().flatten();
        let e = map
            .as_ref()
            .and_then(|m| w.assets.iter().find(|a| a.id == m.ebook_asset).cloned())
            .or_else(|| pick_ebook(&w.assets));
        audio_asset.set(pick_audio(&w.assets, map.as_ref()));
        if let Some(m) = map {
            sync.set(SyncIndex::new(m.spans));
        }
        if let Some(e) = &e {
            if let Ok(l) = api::get::<BookLayout>(&format!("/api/assets/{}/layout", e.id)).await {
                layout.set(Some(l));
            }
        }
        ebook.set(e);
        let pos = w.position.clone().unwrap_or_default();
        if let Some(a) = audio_asset.get_untracked() {
            crate::set_media_session(&w.title, w.author.as_deref().unwrap_or(""), w.cover_url.as_deref().unwrap_or(""));
            track.set(pos.track.min(a.tracks.len().saturating_sub(1)));
            pending_seek.set(Some((pos.seconds, false)));
        }
        scroll_to.set(Some(pos.sentence as u32));
        chapter.set(pos.chapter);
        work.set(Some(w));
    });

    // Load a chapter whenever it changes.
    Effect::new(move |_| {
        let c = chapter.get();
        let Some(e) = ebook.get() else { return };
        let Some(l) = layout.get() else { return };
        if l.mode != ReadMode::Chapters || c == usize::MAX {
            return;
        }
        let c = c.min(l.chapters.saturating_sub(1));
        spawn_local(async move {
            match api::get::<Chapter>(&format!("/api/assets/{}/chapters/{c}", e.id)).await {
                Ok(ch) => {
                    html.set(ch.html);
                    // Let the DOM update first.
                    request_animation_frame(move || {
                        if let Some(el) = page.get_untracked() {
                            crate::scroll_to_sentence(&el, scroll_to.get_untracked().unwrap_or(0));
                            scroll_to.set(None);
                            crate::highlight(&el, sentence.get_untracked(), false);
                        }
                    });
                }
                Err(err) => toast.show(err.0),
            }
        });
    });

    let audio_el = move || audio.get_untracked();

    // Follow the audio: highlight what's being said, turning chapters as needed.
    let on_time = move |_| {
        let Some(a) = audio_el() else { return };
        let t = a.current_time();
        time.set(t);
        let s = sync.get_untracked();
        if s.is_empty() || !prefs.get_untracked().follow_audio && !playing.get_untracked() {
            return;
        }
        if let Some(span) = s.at(track.get_untracked(), t) {
            if span.sentence as i32 == sentence.get_untracked() && span.chapter == chapter.get_untracked() {
                return;
            }
            sentence.set(span.sentence as i32);
            if span.chapter != chapter.get_untracked() {
                scroll_to.set(Some(span.sentence as u32));
                chapter.set(span.chapter);
            } else if let Some(el) = page.get_untracked() {
                crate::highlight(&el, span.sentence as i32, prefs.get_untracked().follow_audio);
            }
        }
    };

    let seek_to = move |tr: usize, secs: f64| {
        let Some(a) = audio_el() else { return };
        if tr != track.get_untracked() {
            pending_seek.set(Some((secs, !a.paused())));
            track.set(tr);
        } else {
            a.set_current_time(secs);
        }
    };

    let on_page_click = move |ev: web_sys::MouseEvent| {
        let target = crate::click_target(&ev);
        if let Some(link) = js_sys::Reflect::get(&target, &JsValue::from_str("link")).ok().and_then(|v| v.as_string()) {
            let Some(e) = ebook.get_untracked() else { return };
            let file = link.split('#').next().unwrap_or("").to_string();
            spawn_local(async move {
                let url = format!("/api/assets/{}/locate?path={}", e.id, js_sys::encode_uri_component(&file));
                if let Ok(Some(c)) = api::get::<Option<usize>>(&url).await {
                    scroll_to.set(Some(0));
                    chapter.set(c);
                }
            });
            return;
        }
        if let Some(n) = js_sys::Reflect::get(&target, &JsValue::from_str("sentence")).ok().and_then(|v| v.as_f64()) {
            let n = n as usize;
            let s = sync.get_untracked();
            if let Some(span) = s.of(chapter.get_untracked(), n).cloned() {
                sentence.set(n as i32);
                if let Some(el) = page.get_untracked() {
                    crate::highlight(&el, n as i32, false);
                }
                seek_to(span.track, span.start);
            }
        }
    };

    let position = move || -> Position {
        let a = audio_asset.get_untracked();
        let t = time.get_untracked();
        let tr = track.get_untracked();
        let c = chapter.get_untracked();
        let s = if playing.get_untracked() && sentence.get_untracked() >= 0 {
            sentence.get_untracked() as usize
        } else {
            page.get_untracked().map(|el| crate::visible_sentence(&el) as usize).unwrap_or(0)
        };
        let fraction = match (&a, layout.get_untracked()) {
            (Some(a), _) if a.duration_secs.unwrap_or(0.0) > 0.0 => {
                let before: f64 = a.tracks.iter().take(tr).map(|t| t.duration_secs).sum();
                ((before + t) / a.duration_secs.unwrap()).clamp(0.0, 1.0)
            }
            (_, Some(l)) if l.chapters > 0 => ((c as f64 + 0.5) / l.chapters as f64).clamp(0.0, 1.0),
            _ => 0.0,
        };
        Position { chapter: if c == usize::MAX { 0 } else { c }, sentence: s, track: tr, seconds: t, fraction, updated_at: None }
    };
    let save = move || {
        if work.get_untracked().is_none() {
            return;
        }
        let p = position();
        spawn_local(async move {
            let _ = api::put::<_, ()>(&format!("/api/works/{id}/position"), &p).await;
        });
    };
    if let Ok(handle) = set_interval_with_handle(save, std::time::Duration::from_secs(10)) {
        on_cleanup(move || {
            handle.clear();
            save();
        });
    }

    let toggle = move |_| {
        if let Some(a) = audio_el() {
            if a.paused() {
                let _ = a.play();
            } else {
                let _ = a.pause();
            }
        }
    };
    let skip = move |d: f64| {
        if let Some(a) = audio_el() {
            a.set_current_time((a.current_time() + d).max(0.0));
        }
    };
    let track_url = move || {
        audio_asset.get().and_then(|a| a.tracks.get(track.get()).map(|t| t.url.clone())).unwrap_or_default()
    };
    let chapters = move || layout.get().map(|l| l.chapters).unwrap_or(0);
    let go_chapter = move |c: usize| {
        scroll_to.set(Some(0));
        chapter.set(c);
        toc_open.set(false);
    };

    view! {
        <div class="reader" style=move || {
            let p = prefs.get();
            format!("--font-size:{}px;--line-height:{};--font:{}", font_size.get(), p.line_height, p.font)
        } data-theme=move || prefs.get().theme>
            <header class="reader-bar">
                <a class="back" href=format!("#/work/{id}") aria-label="Back">"‹"</a>
                <div class="reader-title">
                    {move || work.get().map(|w| w.title)}
                    <small>{move || {
                        let c = chapter.get();
                        layout.get().and_then(|l| l.toc.iter().rev().find(|t| t.chapter <= c).map(|t| t.title.clone()))
                    }}</small>
                </div>
                <button class="icon-button" title="Smaller text" on:click=move |_| font_size.update(|f| *f = (*f - 1).max(12))>"A−"</button>
                <button class="icon-button" title="Larger text" on:click=move |_| font_size.update(|f| *f = (*f + 1).min(32))>"A+"</button>
                <Show when=move || layout.get().is_some_and(|l| !l.toc.is_empty())>
                    <button class="icon-button" title="Contents" on:click=move |_| toc_open.update(|o| *o = !*o)>"☰"</button>
                </Show>
            </header>

            <Show when=move || toc_open.get()>
                <nav class="toc">
                    {move || layout.get().map(|l| l.toc.into_iter().map(|t| {
                        let c = t.chapter;
                        view! {
                            <a href="#" style=format!("padding-left:{}rem", 1.0 + t.depth as f64)
                                class:current=move || chapter.get() == c
                                on:click=move |ev| { ev.prevent_default(); go_chapter(c) }>{t.title}</a>
                        }
                    }).collect_view())}
                </nav>
            </Show>

            {move || match (ebook.get(), layout.get()) {
                (Some(e), Some(l)) if l.mode == ReadMode::Pdf => view! {
                    <iframe class="pdf" src=format!("/api/assets/{}/file", e.id) title="PDF"></iframe>
                }.into_any(),
                (Some(_), Some(l)) if l.mode == ReadMode::Unsupported => view! {
                    <div class="empty">
                        <p>"This format needs converting before it can be read."</p>
                        <button on:click=move |_| spawn_local(crate::start_job(duet_common::JobKind::Convert, id))>"Convert to EPUB"</button>
                    </div>
                }.into_any(),
                (Some(_), Some(_)) => view! {
                    <article class="page" class:synced=move || !sync.get().is_empty() node_ref=page
                        inner_html=move || html.get() on:click=on_page_click></article>
                    <div class="chapter-nav">
                        <button disabled=move || chapter.get() == 0 || chapter.get() == usize::MAX
                            on:click=move |_| go_chapter(chapter.get_untracked().saturating_sub(1))>"‹ Previous"</button>
                        <span>{move || format!("{} / {}", chapter.get().saturating_add(1).min(chapters()), chapters())}</span>
                        <button disabled=move || chapter.get().saturating_add(1) >= chapters()
                            on:click=move |_| go_chapter(chapter.get_untracked() + 1)>"Next ›"</button>
                    </div>
                }.into_any(),
                _ => view! {
                    <div class="listen-only">
                        {move || work.get().map(|w| crate::cover(w.cover_url, w.title))}
                    </div>
                }.into_any(),
            }}

            <Show when=move || audio_asset.get().is_some()>
                <div class="player">
                    <audio node_ref=audio src=track_url preload="metadata"
                        on:timeupdate=on_time
                        on:play=move |_| playing.set(true)
                        on:pause=move |_| { playing.set(false); save() }
                        on:loadedmetadata=move |_| {
                            let Some(a) = audio_el() else { return };
                            duration.set(a.duration());
                            a.set_playback_rate(rate.get_untracked());
                            if let Some((secs, play)) = pending_seek.get_untracked() {
                                pending_seek.set(None);
                                a.set_current_time(secs);
                                if play { let _ = a.play(); }
                            }
                        }
                        on:ended=move |_| {
                            let n = audio_asset.get_untracked().map(|a| a.tracks.len()).unwrap_or(0);
                            if track.get_untracked() + 1 < n {
                                pending_seek.set(Some((0.0, true)));
                                track.update(|t| *t += 1);
                            }
                        }></audio>
                    <div class="scrub">
                        <span>{move || format_time(time.get())}</span>
                        <input type="range" min="0" step="1" prop:max=move || duration.get() prop:value=move || time.get()
                            on:change=move |ev| {
                                if let (Some(a), Ok(v)) = (audio_el(), event_target_value(&ev).parse::<f64>()) { a.set_current_time(v) }
                            }/>
                        <span>{move || format_time(duration.get())}</span>
                    </div>
                    <div class="controls">
                        <select title="Track" on:change=move |ev| {
                            if let Ok(t) = event_target_value(&ev).parse::<usize>() { seek_to(t, 0.0) }
                        }>
                            {move || audio_asset.get().map(|a| a.tracks.into_iter().enumerate().map(|(i, t)| view! {
                                <option value=i.to_string() selected=move || track.get() == i>{t.title}</option>
                            }).collect_view())}
                        </select>
                        <button class="round" title="Back 30 seconds" on:click=move |_| skip(-30.0)>"−30"</button>
                        <button class="round play" title="Play or pause" on:click=toggle>
                            {move || if playing.get() { "❚❚" } else { "▶" }}
                        </button>
                        <button class="round" title="Forward 30 seconds" on:click=move |_| skip(30.0)>"+30"</button>
                        <select title="Speed" on:change=move |ev| {
                            if let Ok(r) = event_target_value(&ev).parse::<f64>() {
                                rate.set(r);
                                if let Some(a) = audio_el() { a.set_playback_rate(r) }
                            }
                        }>
                            {[0.8, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5].map(|r| view! {
                                <option value=r.to_string() selected=move || (rate.get() - r).abs() < 0.01>{format!("{r}×")}</option>
                            })}
                        </select>
                    </div>
                </div>
            </Show>

            <Assistant id work audio playing position/>
        </div>
    }
}

/// Ask about the book, look up words, or talk with a character, by voice or text.
#[component]
fn Assistant(
    id: i64,
    work: RwSignal<Option<WorkDetail>>,
    audio: NodeRef<leptos::html::Audio>,
    playing: RwSignal<bool>,
    position: impl Fn() -> Position + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let input = RwSignal::new(String::new());
    let messages = RwSignal::new(Vec::<Message>::new());
    let persona = RwSignal::new(None::<i64>);
    let speak = RwSignal::new(true);
    let recording = RwSignal::new(false);
    let thinking = RwSignal::new(false);
    let toast = toast();

    let ask = move |question: String| {
        if question.trim().is_empty() {
            return;
        }
        // Don't talk over the audiobook.
        if playing.get_untracked() {
            if let Some(a) = audio.get_untracked() {
                let _ = a.pause();
            }
        }
        let history: Vec<ChatTurn> = messages
            .get_untracked()
            .iter()
            .rev()
            .take(10)
            .rev()
            .map(|m| ChatTurn { role: if m.mine { "user" } else { "assistant" }.into(), content: m.text.clone() })
            .collect();
        messages.update(|m| m.push(Message { speaker: "You".into(), text: question.clone(), mine: true }));
        input.set(String::new());
        thinking.set(true);
        let req = AskRequest {
            work_id: Some(id),
            question,
            persona: persona.get_untracked(),
            position: Some(position()),
            speak: speak.get_untracked(),
            history,
        };
        spawn_local(async move {
            match api::post::<_, AskReply>("/api/assistant/ask", &req).await {
                Ok(r) => {
                    if let Some(a) = &r.audio {
                        crate::play_data_url(a);
                    }
                    messages.update(|m| m.push(Message { speaker: r.speaker, text: r.answer, mine: false }));
                }
                Err(e) => toast.show(e.0),
            }
            thinking.set(false);
        });
    };

    let mic = move |_| {
        if recording.get_untracked() {
            recording.set(false);
            spawn_local(async move {
                match crate::stop_recording().await {
                    Ok(text) => ask(text.as_string().unwrap_or_default()),
                    Err(e) => toast.show(format!("Couldn't transcribe: {}", js_error(&e))),
                }
            });
        } else {
            spawn_local(async move {
                match crate::start_recording().await {
                    Ok(_) => recording.set(true),
                    Err(e) => toast.show(format!("Microphone unavailable: {}", js_error(&e))),
                }
            });
        }
    };

    let numbered = move || messages.get().into_iter().enumerate().collect::<Vec<(usize, Message)>>();

    let define = move |_| {
        let sel = crate::selected_text();
        if sel.is_empty() {
            toast.show("Select a word or phrase first");
        } else {
            open.set(true);
            ask(format!("What does \"{sel}\" mean in this passage?"));
        }
    };

    view! {
        <div class="assistant-buttons">
            <button class="fab secondary" title="Define the selected words" on:mousedown=|ev| ev.prevent_default() on:click=define>"Define"</button>
            <button class="fab" title="Ask about the book" on:click=move |_| open.update(|o| *o = !*o)>"Ask"</button>
        </div>
        <Show when=move || open.get()>
            <aside class="assistant">
                <header>
                    <select title="Who to talk to" on:change=move |ev| persona.set(event_target_value(&ev).parse().ok())>
                        <option value="">"Reading companion"</option>
                        {move || work.get().map(|w| w.cast.into_iter().map(|c| view! {
                            <option value=c.id.to_string()>{if c.is_narrator { "The narrator".to_string() } else { c.name }}</option>
                        }).collect_view())}
                    </select>
                    <label class="check"><input type="checkbox" prop:checked=move || speak.get()
                        on:change=move |ev| speak.set(event_target_checked(&ev))/>"Speak"</label>
                    <button class="icon-button" on:click=move |_| open.set(false) aria-label="Close">"×"</button>
                </header>
                <div class="messages">
                    <Show when=move || messages.get().is_empty()>
                        <p class="hint">"Ask anything about the book so far, look up a word, or pick a character above and talk to them."</p>
                    </Show>
                    <For each=numbered key=|(i, _)| *i let:item>
                        <div class="message" class:mine=item.1.mine>
                            <small>{item.1.speaker.clone()}</small>
                            <p>{item.1.text.clone()}</p>
                        </div>
                    </For>
                    <Show when=move || thinking.get()><p class="hint">"Thinking…"</p></Show>
                </div>
                <form on:submit=move |ev| { ev.prevent_default(); ask(input.get_untracked()) }>
                    <input type="text" placeholder="Type a question" prop:value=move || input.get()
                        on:input=move |ev| input.set(event_target_value(&ev))/>
                    <Show when=crate::mic_supported>
                        <button type="button" class="mic" class:recording=move || recording.get() on:click=mic
                            title="Talk">{move || if recording.get() { "■" } else { "🎙" }}</button>
                    </Show>
                </form>
            </aside>
        </Show>
    }
}

fn js_error(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message())).or_else(|| e.as_string()).unwrap_or_else(|| "unknown error".into())
}
