//! The reader and player. With a sync map, the sentence being spoken is highlighted (and followed
//! across chapters), and tapping a sentence jumps the audio there.

use duet_common::{
    AskReply, AskRequest, Asset, AssetKind, BookLayout, Chapter, ChatTurn, Position, ReadMode, Settings, SyncMap,
    SyncSpan, WorkDetail,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

use crate::{api, format_time, toast};

const READABLE: [&str; 6] = ["epub", "txt", "md", "html", "htm", "xhtml"];

fn readable(a: &Asset) -> bool {
    READABLE.contains(&a.format.as_str())
}

/// How an edition is named in the edition picker.
pub fn edition_label(a: &Asset) -> String {
    match (a.kind, a.origin == "generated") {
        (AssetKind::Ebook, true) => "Transcribed from the audiobook (AI)".into(),
        (AssetKind::Audiobook, true) => "Full cast (AI)".into(),
        (AssetKind::Ebook, false) => format!("Original {}", a.format.to_uppercase()),
        (AssetKind::Audiobook, false) => "Original narration".into(),
    }
}

/// The edition to read: an original EPUB or text first, then a generated one, then PDF, then
/// anything (shown as needing conversion).
fn pick_ebook(assets: &[Asset]) -> Option<Asset> {
    let ebooks = || assets.iter().filter(|a| a.kind == AssetKind::Ebook);
    ebooks()
        .find(|a| readable(a) && a.origin != "generated")
        .or_else(|| ebooks().find(|a| readable(a)))
        .or_else(|| ebooks().find(|a| a.format == "pdf"))
        .or_else(|| ebooks().next())
        .cloned()
}

/// The audiobook to play: one synced with the chosen ebook if possible, originals first.
fn pick_audio(assets: &[Asset], ebook: Option<i64>, pairs: &[(i64, i64)]) -> Option<Asset> {
    let audio = || assets.iter().filter(|a| a.kind == AssetKind::Audiobook);
    let synced = |a: &&Asset| ebook.is_some_and(|e| pairs.contains(&(e, a.id)));
    audio()
        .filter(synced)
        .min_by_key(|a| a.origin == "generated")
        .or_else(|| audio().min_by_key(|a| a.origin == "generated"))
        .cloned()
}

fn storage() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}

/// The editions last chosen for a book on this device.
fn remembered(work: i64) -> (Option<i64>, Option<i64>) {
    let v = storage().and_then(|s| s.get_item(&format!("duet-editions-{work}")).ok().flatten()).unwrap_or_default();
    let mut parts = v.split(',').map(|p| p.parse::<i64>().ok());
    (parts.next().flatten(), parts.next().flatten())
}

fn remember(work: i64, ebook: Option<i64>, audio: Option<i64>) {
    let fmt = |x: Option<i64>| x.map(|v| v.to_string()).unwrap_or_default();
    if let Some(s) = storage() {
        let _ = s.set_item(&format!("duet-editions-{work}"), &format!("{},{}", fmt(ebook), fmt(audio)));
    }
}

async fn load_sync(work: i64, ebook: Option<i64>, audio: Option<i64>) -> SyncIndex {
    let (Some(e), Some(a)) = (ebook, audio) else { return SyncIndex::default() };
    match api::get::<Option<SyncMap>>(&format!("/api/works/{work}/sync?ebook={e}&audio={a}")).await {
        Ok(Some(m)) => SyncIndex::new(m.spans),
        _ => SyncIndex::default(),
    }
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
    // Synced (ebook, audiobook) pairings of this book.
    let pairs = RwSignal::new(Vec::<(i64, i64)>::new());
    let prefs = RwSignal::new(Settings::default().reader);
    let editions_open = RwSignal::new(false);
    let view_open = RwSignal::new(false);

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
    let book = NodeRef::<leptos::html::Div>::new();
    let paged = move || prefs.with(|p| p.layout != "scroll");
    // (page, pages) within the chapter, in the paged layout.
    let page_info = RwSignal::new((1u32, 1u32));
    // Whether two facing pages are showing right now.
    let spread_on = RwSignal::new(false);
    // Coming back from the next chapter opens this one on its last page.
    let start_at_end = RwSignal::new(false);
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
        let synced: Vec<(i64, i64)> = api::get(&format!("/api/works/{id}/sync")).await.unwrap_or_default();
        let (want_e, want_a) = remembered(id);
        let find = |want: Option<i64>| want.and_then(|want| w.assets.iter().find(|a| a.id == want).cloned());
        let e = find(want_e).or_else(|| pick_ebook(&w.assets));
        let a = find(want_a).or_else(|| pick_audio(&w.assets, e.as_ref().map(|e| e.id), &synced));
        sync.set(load_sync(id, e.as_ref().map(|e| e.id), a.as_ref().map(|a| a.id)).await);
        pairs.set(synced);
        if let Some(e) = &e {
            if let Ok(l) = api::get::<BookLayout>(&format!("/api/assets/{}/layout", e.id)).await {
                layout.set(Some(l));
            }
        }
        ebook.set(e);
        audio_asset.set(a.clone());
        let pos = w.position.clone().unwrap_or_default();
        if let Some(a) = a {
            crate::set_media_session(&w.title, w.author.as_deref().unwrap_or(""), w.cover_url.as_deref().unwrap_or(""));
            track.set(pos.track.min(a.tracks.len().saturating_sub(1)));
            pending_seek.set(Some((pos.seconds, false)));
        }
        scroll_to.set(Some(pos.sentence as u32));
        chapter.set(pos.chapter);
        work.set(Some(w));
    });

    // Turning past the first or last page moves to the neighbouring chapter.
    let chapter_count = move || layout.get_untracked().map(|l| l.chapters).unwrap_or(0);
    let pager_next = Closure::<dyn Fn()>::new(move || {
        let c = chapter.get_untracked();
        if c + 1 < chapter_count() {
            scroll_to.set(Some(0));
            chapter.set(c + 1);
        }
    })
    .into_js_value()
    .unchecked_into::<js_sys::Function>();
    let pager_prev = Closure::<dyn Fn()>::new(move || {
        let c = chapter.get_untracked();
        if c > 0 && c != usize::MAX {
            start_at_end.set(true);
            chapter.set(c - 1);
        }
    })
    .into_js_value()
    .unchecked_into::<js_sys::Function>();
    let pager_page = Closure::<dyn Fn(u32, u32, bool)>::new(move |p, n, two| {
        page_info.set((p, n));
        spread_on.set(two);
    })
        .into_js_value()
        .unchecked_into::<js_sys::Function>();
    // Lays the chapter out as pages (or a scrolling column) and goes to the wanted place.
    let lay_out = StoredValue::new_local(move || {
        let (Some(el), Some(b)) = (page.get_untracked(), book.get_untracked()) else { return };
        if paged() {
            crate::pager_init(&b, &el, &pager_next, &pager_prev, &pager_page, &prefs.get_untracked().spread);
        } else {
            crate::pager_destroy(&b);
            el.set_attribute("style", "").ok();
        }
        if start_at_end.get_untracked() && paged() {
            start_at_end.set(false);
            crate::pager_end(&b);
        } else {
            crate::scroll_to_sentence(&el, scroll_to.get_untracked().unwrap_or(0));
        }
        scroll_to.set(None);
        crate::highlight(&el, sentence.get_untracked(), false);
    });
    // The page element is rebuilt when the edition or layout changes; lay it out again then.
    Effect::new(move |_| {
        if book.get().is_some() && !html.get_untracked().is_empty() {
            request_animation_frame(move || lay_out.with_value(|f| f()));
        }
    });
    on_cleanup(move || {
        if let Some(b) = book.get_untracked() {
            crate::pager_destroy(&b);
        }
    });
    // One page or two facing pages; remembered in Settings.
    let toggle_spread = move |_| {
        let choice = if spread_on.get_untracked() { "one" } else { "two" };
        prefs.update(|p| p.spread = choice.into());
        if let Some(b) = book.get_untracked() {
            crate::pager_set_spread(&b, choice);
        }
        spawn_local(async move {
            if let Ok(mut s) = api::get::<Settings>("/api/settings").await {
                s.reader.spread = choice.into();
                let _ = api::put::<_, Settings>("/api/settings", &s).await;
            }
        });
    };
    let toggle_layout = move |_| {
        let keep = page.get_untracked().map(|el| crate::visible_sentence(&el)).unwrap_or(0);
        prefs.update(|p| p.layout = if p.layout == "scroll" { "pages".into() } else { "scroll".into() });
        let layout_choice = prefs.get_untracked().layout;
        scroll_to.set(Some(keep));
        request_animation_frame(move || lay_out.with_value(|f| f()));
        spawn_local(async move {
            if let Ok(mut s) = api::get::<Settings>("/api/settings").await {
                s.reader.layout = layout_choice;
                let _ = api::put::<_, Settings>("/api/settings", &s).await;
            }
        });
    };

    // Load a chapter whenever it changes.
    Effect::new(move |_| {
        let c = chapter.get();
        let Some(e) = ebook.get() else { return };
        let Some(l) = layout.get() else { return };
        if !matches!(l.mode, ReadMode::Chapters | ReadMode::Images) || c == usize::MAX {
            return;
        }
        let c = c.min(l.chapters.saturating_sub(1));
        spawn_local(async move {
            match api::get::<Chapter>(&format!("/api/assets/{}/chapters/{c}", e.id)).await {
                Ok(ch) => {
                    html.set(ch.html);
                    // Let the DOM update first.
                    request_animation_frame(move || lay_out.with_value(|f| f()));
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
    // Switching editions keeps your place: through the sync map when the new pairing has one,
    // otherwise at the same point proportionally.
    let switch_audio = move |new: Asset| {
        let Some(a) = audio_el() else { return };
        let was_playing = !a.paused();
        let _ = a.pause();
        let here = position();
        // The sentence being heard (or last tapped), else the one at the top of the page.
        let s = if sentence.get_untracked() >= 0 { sentence.get_untracked() as usize } else { here.sentence };
        let c = chapter.get_untracked();
        let e = ebook.get_untracked().map(|e| e.id);
        remember(id, e, Some(new.id));
        spawn_local(async move {
            let idx = load_sync(id, e, Some(new.id)).await;
            let (tr, secs) = match idx.of(c, s) {
                Some(span) => (span.track, span.start),
                None => {
                    // Same fraction of the whole recording.
                    let total = new.duration_secs.unwrap_or(0.0) * here.fraction;
                    let mut left = total;
                    let mut at = (0, 0.0);
                    for (i, t) in new.tracks.iter().enumerate() {
                        if left <= t.duration_secs {
                            at = (i, left);
                            break;
                        }
                        left -= t.duration_secs;
                    }
                    at
                }
            };
            sync.set(idx);
            // The new source loads and then applies the pending seek (see on:loadedmetadata).
            pending_seek.set(Some((secs, was_playing)));
            track.set(tr);
            audio_asset.set(Some(new));
        });
    };
    let switch_ebook = move |new: Asset| {
        let old_chapters = layout.get_untracked().map(|l| l.chapters).unwrap_or(1).max(1);
        let old_chapter = chapter.get_untracked().min(old_chapters - 1);
        let a = audio_asset.get_untracked().map(|a| a.id);
        remember(id, Some(new.id), a);
        spawn_local(async move {
            let idx = load_sync(id, Some(new.id), a).await;
            let l = api::get::<BookLayout>(&format!("/api/assets/{}/layout", new.id)).await.ok();
            let new_chapters = l.as_ref().map(|l| l.chapters).unwrap_or(1).max(1);
            let (c, s) = match idx.at(track.get_untracked(), time.get_untracked()) {
                Some(span) => (span.chapter, span.sentence as u32),
                None => ((old_chapter * new_chapters) / old_chapters, 0),
            };
            sync.set(idx);
            sentence.set(-1);
            layout.set(l);
            ebook.set(Some(new));
            scroll_to.set(Some(s));
            chapter.set(usize::MAX);
            chapter.set(c);
        });
    };
    let sync_these = move |_| {
        let (Some(e), Some(a)) = (ebook.get_untracked(), audio_asset.get_untracked()) else { return };
        spawn_local(async move {
            match api::post::<_, i64>(
                "/api/jobs",
                &duet_common::NewJob { kind: duet_common::JobKind::Align, work_id: id, ebook_asset: Some(e.id), audiobook_asset: Some(a.id) },
            )
            .await
            {
                Ok(_) => toast.show("Syncing these editions; follow it in Jobs"),
                Err(err) => toast.show(err.0),
            }
        });
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
        <div class="reader" class:paged-mode=paged class:has-player=move || audio_asset.get().is_some() style=move || {
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
                <button class="icon-button" title="Text and page view" on:click=move |_| view_open.update(|o| *o = !*o)>"Aa"</button>
                <Show when=move || work.get().is_some_and(|w| w.assets.len() > 1)>
                    <button class="icon-button" title="Editions" on:click=move |_| editions_open.update(|o| *o = !*o)>"Editions"</button>
                </Show>
                <Show when=move || layout.get().is_some_and(|l| !l.toc.is_empty())>
                    <button class="icon-button" title="Contents" on:click=move |_| toc_open.update(|o| *o = !*o)>"☰"</button>
                </Show>
            </header>

            <Show when=move || view_open.get()>
                <div class="editions-panel view-panel">
                    <div class="view-row">
                        <span>"Text size"</span>
                        <div class="segmented">
                            <button title="Smaller text" on:click=move |_| font_size.update(|f| *f = (*f - 1).max(12))>"A−"</button>
                            <button title="Larger text" on:click=move |_| font_size.update(|f| *f = (*f + 1).min(32))>"A+"</button>
                        </div>
                    </div>
                    <div class="view-row">
                        <span>"Layout"</span>
                        <div class="segmented">
                            <button class:active=paged on:click=move |ev| if !paged() { toggle_layout(ev) }>"Pages"</button>
                            <button class:active=move || !paged() on:click=move |ev| if paged() { toggle_layout(ev) }>"Scroll"</button>
                        </div>
                    </div>
                    <Show when=paged>
                        <div class="view-row">
                            <span>"Pages at a time"</span>
                            <div class="segmented">
                                <button class:active=move || !spread_on.get() on:click=move |ev| if spread_on.get_untracked() { toggle_spread(ev) }>"One"</button>
                                <button class:active=move || spread_on.get() on:click=move |ev| if !spread_on.get_untracked() { toggle_spread(ev) }>"Two"</button>
                            </div>
                        </div>
                    </Show>
                </div>
            </Show>

            <Show when=move || editions_open.get()>
                <div class="editions-panel">
                    {move || work.get().map(|w| {
                        let of = |k: AssetKind| w.assets.iter().filter(|a| a.kind == k).cloned().collect::<Vec<_>>();
                        let (texts, audios) = (of(AssetKind::Ebook), of(AssetKind::Audiobook));
                        view! {
                            {(!texts.is_empty()).then(|| view! {
                                <div class="edition-group">
                                    <h4>"Text"</h4>
                                    {texts.into_iter().map(|a| {
                                        let (aid, label, a2) = (a.id, edition_label(&a), a.clone());
                                        view! {
                                            <label class="choice" class:on=move || ebook.get().is_some_and(|e| e.id == aid)>
                                                <input type="radio" name="text-edition" prop:checked=move || ebook.get().is_some_and(|e| e.id == aid)
                                                    on:change=move |_| switch_ebook(a2.clone())/>
                                                {label}
                                            </label>
                                        }
                                    }).collect_view()}
                                </div>
                            })}
                            {(!audios.is_empty()).then(|| view! {
                                <div class="edition-group">
                                    <h4>"Audio"</h4>
                                    {audios.into_iter().map(|a| {
                                        let (aid, label, a2) = (a.id, edition_label(&a), a.clone());
                                        view! {
                                            <label class="choice" class:on=move || audio_asset.get().is_some_and(|x| x.id == aid)>
                                                <input type="radio" name="audio-edition" prop:checked=move || audio_asset.get().is_some_and(|x| x.id == aid)
                                                    on:change=move |_| switch_audio(a2.clone())/>
                                                {label}
                                            </label>
                                        }
                                    }).collect_view()}
                                </div>
                            })}
                        }
                    })}
                    {move || match (ebook.get(), audio_asset.get()) {
                        (Some(e), Some(a)) => {
                            if pairs.get().contains(&(e.id, a.id)) || !sync.get().is_empty() {
                                view! { <p class="ok">"✓ These two are in sync."</p> }.into_any()
                            } else {
                                view! {
                                    <p class="muted">"These two aren't synced yet, so the text won't follow the audio."</p>
                                    <button class="primary" on:click=sync_these>"Sync these editions"</button>
                                }.into_any()
                            }
                        }
                        _ => view! { <span></span> }.into_any(),
                    }}
                </div>
            </Show>

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
                (Some(_), Some(l)) if l.mode == ReadMode::Unsupported => view! {
                    <div class="empty">
                        <p>"This book is being converted so it can be read. It usually takes a minute; follow it in "<a href="#/jobs">"Jobs"</a>"."</p>
                        <button on:click=move |_| spawn_local(crate::start_job(duet_common::JobKind::Convert, id))>"Convert again"</button>
                    </div>
                }.into_any(),
                (Some(_), Some(l)) => view! {
                    <div class="bookview" class:paged=paged class:images=l.mode == ReadMode::Images node_ref=book>
                        <article class="page" class:synced=move || !sync.get().is_empty() node_ref=page
                            inner_html=move || html.get() on:click=on_page_click></article>
                    </div>
                    <div class="chapter-nav">
                        <button disabled=move || chapter.get() == 0 || chapter.get() == usize::MAX
                            on:click=move |_| go_chapter(chapter.get_untracked().saturating_sub(1))>"‹ Previous"</button>
                        <span>{move || {
                            let c = format!("Chapter {} of {}", chapter.get().saturating_add(1).min(chapters()), chapters());
                            if paged() {
                                let (p, n) = page_info.get();
                                format!("{c} · page {p} of {n}")
                            } else {
                                c
                            }
                        }}</span>
                        <button disabled=move || chapters() <= chapter.get().saturating_add(1)
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

    // "Define" shows only while some text is selected.
    let has_selection = RwSignal::new(false);
    let on_select = Closure::<dyn Fn()>::new(move || has_selection.set(!crate::selected_text().is_empty()));
    let _ = document().add_event_listener_with_callback("selectionchange", on_select.as_ref().unchecked_ref());
    let on_select = on_select.into_js_value();
    on_cleanup(move || {
        let _ = document().remove_event_listener_with_callback("selectionchange", on_select.unchecked_ref());
    });

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
            <Show when=move || has_selection.get()>
                <button class="fab secondary" title="Define the selected words" on:mousedown=|ev| ev.prevent_default() on:click=define>"Define"</button>
            </Show>
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
