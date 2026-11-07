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

const READABLE: [&str; 7] = ["epub", "txt", "md", "html", "htm", "xhtml", "transcript"];

/// The page colour to use: light unless you've chosen otherwise ("auto" from older settings
/// counts as light too).
fn page_theme(t: &str) -> &str {
    match t {
        "sepia" | "gray" | "dark" => t,
        _ => "light",
    }
}

fn readable(a: &Asset) -> bool {
    READABLE.contains(&a.format.as_str())
}

/// How an edition is named in the edition picker.
pub fn edition_label(a: &Asset) -> String {
    match (a.kind, a.origin == "generated") {
        (AssetKind::Ebook, true) => "Made from the audiobook (AI)".into(),
        (AssetKind::Audiobook, true) => "Full cast (AI)".into(),
        (AssetKind::Ebook, false) => format!("Original {}", a.format.to_uppercase()),
        (AssetKind::Audiobook, false) => "Original narration".into(),
    }
}

/// How the book is enjoyed. Every mode works straight away: the audiobook is matched to the
/// text as it plays, and the full cast is performed a few sentences ahead of the listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Read,
    ReadAudiobook,
    ReadCast,
    ListenAudiobook,
    ListenCast,
}

impl Mode {
    pub const ALL: [Mode; 5] = [Mode::Read, Mode::ReadAudiobook, Mode::ReadCast, Mode::ListenAudiobook, Mode::ListenCast];

    pub fn key(self) -> &'static str {
        match self {
            Mode::Read => "read",
            Mode::ReadAudiobook => "read-audiobook",
            Mode::ReadCast => "read-cast",
            Mode::ListenAudiobook => "listen",
            Mode::ListenCast => "listen-cast",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.key() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Read => "Read",
            Mode::ReadAudiobook => "Read along with the audiobook",
            Mode::ReadCast => "Read along with the full cast",
            Mode::ListenAudiobook => "Listen to the audiobook",
            Mode::ListenCast => "Listen to the full cast",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Mode::Read => "Reading",
            Mode::ReadAudiobook => "Reading along",
            Mode::ReadCast => "Full cast",
            Mode::ListenAudiobook => "Listening",
            Mode::ListenCast => "Full cast",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Mode::Read => "Just the book, no audio.",
            Mode::ReadAudiobook => "The recording plays and the sentence being read is highlighted.",
            Mode::ReadCast => "AI voices perform the book, a voice for every character, as you read.",
            Mode::ListenAudiobook => "Just the recording.",
            Mode::ListenCast => "AI voices perform the book, a voice for every character.",
        }
    }

    pub fn icons(self) -> &'static [&'static str] {
        use crate::icons::{BOOK, HEADPHONES, MASKS};
        match self {
            Mode::Read => &[BOOK],
            Mode::ReadAudiobook => &[BOOK, HEADPHONES],
            Mode::ReadCast => &[BOOK, MASKS],
            Mode::ListenAudiobook => &[HEADPHONES],
            Mode::ListenCast => &[MASKS],
        }
    }

    pub fn cast(self) -> bool {
        matches!(self, Mode::ReadCast | Mode::ListenCast)
    }

    pub fn audiobook(self) -> bool {
        matches!(self, Mode::ReadAudiobook | Mode::ListenAudiobook)
    }

    pub fn listening_only(self) -> bool {
        matches!(self, Mode::ListenAudiobook | Mode::ListenCast)
    }

    /// The modes this book's editions allow.
    pub fn available(assets: &[Asset]) -> Vec<Mode> {
        let audio = assets.iter().any(|a| a.kind == AssetKind::Audiobook);
        // An audiobook can always be read: its text is made from the recording as you go.
        let ebook = audio || assets.iter().any(|a| a.kind == AssetKind::Ebook);
        let text = audio || assets.iter().any(|a| a.kind == AssetKind::Ebook && readable(a));
        Mode::ALL
            .into_iter()
            .filter(|m| match m {
                Mode::Read => ebook,
                Mode::ReadAudiobook => ebook && audio,
                Mode::ReadCast | Mode::ListenCast => text,
                Mode::ListenAudiobook => audio,
            })
            .collect()
    }

    /// What to open with: the last mode used here, else reading along when there's a recording.
    pub fn default_for(id: i64, assets: &[Asset]) -> Mode {
        let available = Mode::available(assets);
        let last = storage().and_then(|s| s.get_item(&format!("duet-mode-{id}")).ok().flatten()).and_then(|k| Mode::parse(&k));
        last.filter(|m| available.contains(m))
            .or_else(|| [Mode::ReadAudiobook, Mode::Read, Mode::ListenAudiobook].into_iter().find(|m| available.contains(m)))
            .unwrap_or(Mode::Read)
    }

    fn remember(self, id: i64) {
        if let Some(s) = storage() {
            let _ = s.set_item(&format!("duet-mode-{id}"), self.key());
        }
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

    /// Adds newly placed sentences, replacing earlier placements of the same ones.
    fn merge(&mut self, new: Vec<SyncSpan>) {
        let fresh: std::collections::HashSet<(usize, usize)> = new.iter().map(|s| (s.chapter, s.sentence)).collect();
        self.by_time.retain(|s| !fresh.contains(&(s.chapter, s.sentence)));
        self.by_time.extend(new);
        self.by_time.sort_by(|a, b| (a.track, a.start).partial_cmp(&(b.track, b.start)).unwrap());
    }

    /// How far past `t` the sync reaches without a gap, on this track.
    fn covered_until(&self, track: usize, t: f64) -> f64 {
        let mut reach = t;
        // (A sentence synced before word times were measured counts as not synced yet: it's
        // done again, exactly, as the recording reaches it.)
        for s in self.by_time.iter().filter(|s| s.track == track && s.end >= t - 3.0 && !s.words.is_empty()) {
            if s.start > reach + 6.0 {
                break;
            }
            reach = reach.max(s.end);
        }
        reach
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

    /// Where in the recording a sentence that isn't synced yet most likely is: between the
    /// nearest synced sentences either side of it in its chapter (by how far it is between them),
    /// or, in a chapter with none, the same way through the track that chapter is read on (the
    /// track found from how other chapters line up with tracks). `n` is the chapter's sentence
    /// count; `tracks` the tracks' lengths.
    fn estimate(&self, chapter: usize, sentence: usize, n: usize, tracks: &[f64]) -> Option<(usize, f64)> {
        let mut before: Option<&SyncSpan> = None;
        let mut after: Option<&SyncSpan> = None;
        for sp in self.by_time.iter().filter(|sp| sp.chapter == chapter) {
            if sp.sentence <= sentence && before.is_none_or(|b| sp.sentence > b.sentence) {
                before = Some(sp);
            }
            if sp.sentence >= sentence && after.is_none_or(|a| sp.sentence < a.sentence) {
                after = Some(sp);
            }
        }
        match (before, after) {
            (Some(b), Some(a)) if b.track == a.track && a.sentence > b.sentence => {
                let f = (sentence - b.sentence) as f64 / (a.sentence - b.sentence) as f64;
                return Some((b.track, b.start + (a.start - b.start) * f));
            }
            (Some(b), _) => return Some((b.track, b.end)),
            (None, Some(a)) => return Some((a.track, a.start)),
            _ => {}
        }
        // The chapter → track offset most of the sync agrees on.
        let mut votes: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
        for sp in &self.by_time {
            *votes.entry(sp.track as i64 - sp.chapter as i64).or_default() += 1;
        }
        let offset = votes.into_iter().max_by_key(|(_, v)| *v)?.0;
        let track = usize::try_from(chapter as i64 + offset).ok().filter(|t| *t < tracks.len())?;
        Some((track, tracks[track] * sentence as f64 / n.max(1) as f64))
    }
}

#[derive(Clone, PartialEq)]
struct Message {
    speaker: String,
    text: String,
    mine: bool,
}

#[component]
pub fn Reader(id: i64, #[prop(optional)] start_mode: Option<Mode>) -> impl IntoView {
    let work = RwSignal::new(None::<WorkDetail>);
    let mode = RwSignal::new(start_mode.unwrap_or(Mode::Read));
    // The full cast's place: (chapter, sentence) being performed.
    let cast_at = RwSignal::new((0usize, 0usize));
    // Sentences in each chapter, as chapters are seen.
    let counts = RwSignal::new(std::collections::HashMap::<usize, usize>::new());
    // Waiting for the first sentence's voices (directing a chapter or casting can take a while).
    let cast_wait = RwSignal::new(false);
    // What the full cast is busy with while you wait for it (directing, finding voices, performing).
    let cast_status = RwSignal::new(None::<String>);
    // How far along a long wait is (warming up the voices), when that's known.
    let cast_progress = RwSignal::new(None::<f64>);
    // Where the audiobook was, kept while the full cast plays.
    let book_pos = RwSignal::new((0usize, 0.0f64));
    let ebook = RwSignal::new(None::<Asset>);
    let audio_asset = RwSignal::new(None::<Asset>);
    let layout = RwSignal::new(None::<BookLayout>);
    let sync = RwSignal::new(SyncIndex::default());
    // Synced (ebook, audiobook) pairings of this book.
    let pairs = RwSignal::new(Vec::<(i64, i64)>::new());
    let prefs = RwSignal::new(Settings::default().reader);
    let editions_open = RwSignal::new(false);
    // Bars and buttons fade away while you read; tap the middle of the page to bring them back.
    let chrome_hidden = RwSignal::new(false);
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
        let mut e = find(want_e).or_else(|| pick_ebook(&w.assets));
        let a = find(want_a).or_else(|| pick_audio(&w.assets, e.as_ref().map(|e| e.id), &synced));
        // Only a recording: read the text made from it.
        if e.is_none() && a.is_some() {
            let m = start_mode.unwrap_or_else(|| Mode::default_for(id, &w.assets));
            if !m.listening_only() || m.cast() {
                e = api::post::<_, Asset>(&format!("/api/works/{id}/transcript"), &()).await.ok();
            }
        }
        sync.set(load_sync(id, e.as_ref().map(|e| e.id), a.as_ref().map(|a| a.id)).await);
        pairs.set(synced);
        if let Some(e) = &e {
            if let Ok(l) = api::get::<BookLayout>(&format!("/api/assets/{}/layout", e.id)).await {
                layout.set(Some(l));
            }
        }
        ebook.set(e);
        audio_asset.set(a.clone());
        let available = Mode::available(&w.assets);
        let m = start_mode.filter(|m| available.contains(m)).unwrap_or_else(|| Mode::default_for(id, &w.assets));
        m.remember(id);
        mode.set(m);
        let pos = w.position.clone().unwrap_or_default();
        crate::set_media_session(&w.title, w.author.as_deref().unwrap_or(""), w.cover_url.as_deref().unwrap_or(""));
        if let Some(a) = a {
            let tr = pos.track.min(a.tracks.len().saturating_sub(1));
            book_pos.set((tr, pos.seconds));
            track.set(tr);
            if m.audiobook() {
                pending_seek.set(Some((pos.seconds, false)));
            }
        }
        cast_at.set((pos.chapter, pos.sentence));
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
        let (before, _) = page_info.get_untracked();
        page_info.set((p, n));
        spread_on.set(two);
        // Turning a page means you're reading: get the chrome out of the way.
        if p != before {
            chrome_hidden.set(true);
            view_open.set(false);
            editions_open.set(false);
        }
    })
        .into_js_value()
        .unchecked_into::<js_sys::Function>();
    // Lays the chapter out as pages (or a scrolling column) and goes to the wanted place.
    let lay_out = StoredValue::new_local(move || {
        let (Some(el), Some(b)) = (page.get_untracked(), book.get_untracked()) else { return };
        if paged() {
            // Before the pager starts, so the page stacks are right from the first page.
            let total = layout.get_untracked().map(|l| l.chapters).unwrap_or(1);
            crate::pager_set_place(&b, chapter.get_untracked().min(total.saturating_sub(1)) as u32, total as u32);
            if let Some(w) = work.get_untracked() {
                crate::book_cover(&b, &w.title);
            }
            crate::pager_set_curl(&b, prefs.get_untracked().page_curl);
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
    // Also whenever a chapter's text arrives, after the page has drawn it.
    Effect::new(move |_| {
        let has_text = html.with(|h| !h.is_empty());
        if book.get().is_some() && has_text {
            set_timeout(move || lay_out.with_value(|f| f()), std::time::Duration::ZERO);
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
    // Page turns curl over, or change at once; remembered in Settings.
    let set_curl = move |on: bool| {
        prefs.update(|p| p.page_curl = on);
        if let Some(b) = book.get_untracked() {
            crate::pager_set_curl(&b, on);
        }
        spawn_local(async move {
            if let Ok(mut s) = api::get::<Settings>("/api/settings").await {
                s.reader.page_curl = on;
                let _ = api::put::<_, Settings>("/api/settings", &s).await;
            }
        });
    };
    let toggle_layout = move |_| {
        let keep = page.get_untracked().map(|el| crate::visible_sentence(&el)).unwrap_or(0);
        prefs.update(|p| p.layout = if p.layout == "scroll" { "pages".into() } else { "scroll".into() });
        let layout_choice = prefs.get_untracked().layout;
        scroll_to.set(Some(keep));
        set_timeout(move || lay_out.with_value(|f| f()), std::time::Duration::ZERO);
        spawn_local(async move {
            if let Ok(mut s) = api::get::<Settings>("/api/settings").await {
                s.reader.layout = layout_choice;
                let _ = api::put::<_, Settings>("/api/settings", &s).await;
            }
        });
    };

    // The sidebar can be tucked away while reading (remembered), and the app can go full screen.
    let fullscreen = RwSignal::new(crate::is_fullscreen());
    let on_fs = Closure::<dyn Fn()>::new(move || fullscreen.set(crate::is_fullscreen()));
    let _ = document().add_event_listener_with_callback("fullscreenchange", on_fs.as_ref().unchecked_ref());
    let on_fs = on_fs.into_js_value();
    on_cleanup(move || {
        let _ = document().remove_event_listener_with_callback("fullscreenchange", on_fs.unchecked_ref());
    });

    // Home and End: the very beginning and the very end of the book (arrows and Page Up/Down
    // turn pages; see the pager in helpers.js).
    let on_key = Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
        if e.alt_key() || e.ctrl_key() || e.meta_key() {
            return;
        }
        let typing = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|el| el.closest("input, textarea, select, [contenteditable]").ok().flatten().is_some());
        if typing {
            return;
        }
        let total = layout.get_untracked().map(|l| l.chapters).unwrap_or(0);
        match e.key().as_str() {
            "Home" => {
                e.prevent_default();
                scroll_to.set(Some(0));
                start_at_end.set(false);
                chapter.set(usize::MAX);
                chapter.set(0);
            }
            "End" if total > 0 => {
                e.prevent_default();
                start_at_end.set(true);
                chapter.set(usize::MAX);
                chapter.set(total - 1);
            }
            _ => {}
        }
    });
    let _ = document().add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref());
    let on_key = on_key.into_js_value();
    on_cleanup(move || {
        let _ = document().remove_event_listener_with_callback("keydown", on_key.unchecked_ref());
    });

    // Load a chapter whenever it changes.
    Effect::new(move |_| {
        let c = chapter.get();
        let Some(e) = ebook.get() else { return };
        let Some(l) = layout.get() else { return };
        if !matches!(l.mode, ReadMode::Chapters | ReadMode::Images) || c == usize::MAX {
            return;
        }
        let c = c.min(l.chapters.saturating_sub(1));
        let transcript = e.format == "transcript";
        let done = StoredValue::new(false);
        if transcript {
            // Making a chapter from the recording takes a minute or two the first time.
            set_timeout(
                move || {
                    if !done.get_value() && chapter.get_untracked().min(l.chapters.saturating_sub(1)) == c {
                        html.set(r#"<div class="transcribing"><span class="spinner"></span><p>Listening to this chapter and setting it as text…</p><small>This takes a minute or two the first time.</small></div>"#.into());
                    }
                },
                std::time::Duration::from_millis(500),
            );
        }
        spawn_local(async move {
            let got = api::get::<Chapter>(&format!("/api/assets/{}/chapters/{c}", e.id)).await;
            // The reader may have closed while this was loading.
            if done.try_set_value(true).is_some() {
                return;
            }
            if transcript {
                // Its timings came with it; and have the next chapter ready by the time it's needed.
                if let Some(a) = audio_asset.get_untracked() {
                    let idx = load_sync(id, Some(e.id), Some(a.id)).await;
                    sync.set(idx);
                }
                if c + 1 < l.chapters {
                    let next = format!("/api/assets/{}/chapters/{}", e.id, c + 1);
                    spawn_local(async move { crate::prefetch(&next).await; });
                }
            }
            if chapter.get_untracked().min(l.chapters.saturating_sub(1)) != c {
                return;
            }
            match got {
                Ok(ch) => {
                    counts.update(|m| { m.insert(c, ch.sentences); });
                    // Laid out by the Effect that watches `html` (once: laying out twice would
                    // lose "open at the end" when coming back from the next chapter).
                    html.set(ch.html);
                    // The chapters either side, so a page can be dragged over into them. (Not
                    // for text made from the recording: its next chapter may not exist yet.)
                    if !transcript {
                        let eid = e.id;
                        let total = l.chapters;
                        spawn_local(async move {
                            let get = |n: usize| async move {
                                api::get::<Chapter>(&format!("/api/assets/{eid}/chapters/{n}")).await.ok().map(|c| c.html)
                            };
                            let next = if c + 1 < total { get(c + 1).await } else { None };
                            let prev = if c > 0 { get(c - 1).await } else { None };
                            if chapter.try_get_untracked() != Some(c) {
                                return;
                            }
                            if let Some(b) = book.get_untracked() {
                                crate::pager_set_neighbours(&b, next, prev);
                            }
                        });
                    }
                }
                Err(err) => toast.show(err.0),
            }
        });
    });

    let audio_el = move || audio.get_untracked();

    // Text made from the recording has a chapter per track: the page follows the track at once,
    // even before that chapter's words are timed.
    Effect::new(move |_| {
        let tr = track.get();
        if mode.get().audiobook() && ebook.get().is_some_and(|e| e.format == "transcript") && chapter.get_untracked() != tr {
            scroll_to.set(Some(0));
            chapter.set(tr);
        }
    });

    // The first sentence at or after (chapter, sentence), skipping chapters with none (covers,
    // image pages); None past the end of the book.
    let settle = move |mut c: usize, mut s: usize| async move {
        let e = ebook.get_untracked()?;
        let total = layout.get_untracked().map(|l| l.chapters).unwrap_or(0);
        while c < total {
            // (try_: the reader may close while a chapter is being fetched.)
            let n = match counts.try_with_untracked(|m| m.get(&c).copied())? {
                Some(n) => n,
                None => {
                    let n = api::get::<Chapter>(&format!("/api/assets/{}/chapters/{c}", e.id)).await.ok()?.sentences;
                    counts.try_update(|m| { m.insert(c, n); })?;
                    n
                }
            };
            if s < n {
                return Some((c, s));
            }
            c += 1;
            s = 0;
        }
        None
    };
    let cast_url = move |e: i64, (c, s): (usize, usize)| format!("/api/assets/{e}/cast/{c}/{s}");
    // In full cast mode the performer is loaded straight away (it takes a couple of minutes),
    // and kept loaded while you're here rather than unloaded as idle between lines.
    Effect::new(move |_| {
        if !mode.get().cast() {
            return;
        }
        spawn_local(async move {
            while mode.try_get_untracked().is_some_and(|m| m.cast()) {
                let _ = api::post::<_, serde_json::Value>("/api/cast/warm", &serde_json::json!({})).await;
                gloo_timers::future::TimeoutFuture::new(120_000).await;
            }
        });
    });
    // While the full cast keeps you waiting, ask every second what it's doing, to show it.
    Effect::new(move |_| {
        if !cast_wait.get() {
            cast_status.set(None);
            cast_progress.set(None);
            return;
        }
        let Some(e) = ebook.get_untracked().map(|e| e.id) else { return };
        spawn_local(async move {
            while cast_wait.try_get_untracked() == Some(true) {
                if let Ok(v) = api::get::<serde_json::Value>(&format!("/api/assets/{e}/cast/status")).await {
                    let text = v["what"].as_str().map(|what| {
                        if let Some(left) = v["left"].as_f64() {
                            let left = if left >= 90.0 { format!("about {} min left", (left / 60.0).round()) } else { format!("about {}s left", left.round()) };
                            return format!("{what}… {left}");
                        }
                        let (done, total, secs) = (v["done"].as_u64().unwrap_or(0), v["total"].as_u64().unwrap_or(0), v["secs"].as_u64().unwrap_or(0));
                        let of = if total > 1 { format!(" ({} of {total})", done + 1) } else { String::new() };
                        let wait = if secs >= 5 { format!(" · {secs}s") } else { String::new() };
                        format!("{what}{of}…{wait}")
                    });
                    let _ = cast_status.try_set(text);
                    let _ = cast_progress.try_set(v["progress"].as_f64());
                }
                gloo_timers::future::TimeoutFuture::new(1000).await;
            }
        });
    });
    // Moves the full cast to a sentence, carrying on playing if it was.
    let cast_go = move |c: usize, s: usize, play: bool| {
        spawn_local(async move {
            let found = settle(c, s).await;
            if mode.try_get_untracked().is_none() {
                return;
            }
            if let Some(at) = found {
                pending_seek.set(Some((0.0, play)));
                cast_at.set(at);
            } else if !play {
                cast_at.set((c, s));
            }
        });
    };
    // The full cast follows itself through the book: highlight what's being performed, and have
    // the next few sentences voiced ahead of time so there's no wait between them.
    let prefetched = StoredValue::new(std::collections::HashSet::<(usize, usize)>::new());
    let prefetching = RwSignal::new(false);
    Effect::new(move |_| {
        let (c, s) = cast_at.get();
        if !mode.get().cast() {
            return;
        }
        sentence.set(s as i32);
        if c != chapter.get_untracked() {
            scroll_to.set(Some(s as u32));
            chapter.set(c);
        } else if let Some(el) = page.get_untracked() {
            crate::highlight(&el, s as i32, prefs.get_untracked().follow_audio);
        }
        if prefetching.get_untracked() {
            return;
        }
        let Some(e) = ebook.get_untracked().map(|e| e.id) else { return };
        prefetching.set(true);
        spawn_local(async move {
            let mut at = (c, s);
            // Voices take about as long to perform a line as to speak it here, so run well ahead.
            for _ in 0..8 {
                let Some(next) = settle(at.0, at.1 + 1).await else { break };
                at = next;
                if prefetched.try_with_value(|p| p.contains(&at)).unwrap_or(true) {
                    continue;
                }
                prefetched.try_update_value(|p| { p.insert(at); });
                crate::prefetch(&cast_url(e, at)).await;
                // Stop running ahead once the listener has moved somewhere else (or left).
                if cast_at.try_get_untracked().is_none_or(|x| x.0 != c) || !mode.get_untracked().cast() {
                    break;
                }
            }
            // Nearing the end of a chapter, have the next one directed and cast in good time.
            let left = counts.try_with_untracked(|m| m.get(&c).copied()).flatten().unwrap_or(0).saturating_sub(s);
            if left <= 15 {
                if let Some(next) = settle(c + 1, 0).await {
                    if prefetched.try_with_value(|p| !p.contains(&next)).unwrap_or(false) {
                        prefetched.try_update_value(|p| { p.insert(next); });
                        crate::prefetch(&cast_url(e, next)).await;
                    }
                }
            }
            prefetching.try_set(false);
        });
    });

    // Follow the audio: highlight what's being said, turning chapters as needed.
    // Syncing while you listen: the next half-minute of audio is heard ahead of the narration
    // and placed in the book, so the highlight is ready before the words are spoken.
    let live_busy = RwSignal::new(false);
    let live_near = RwSignal::new(None::<i64>);
    let live_paused_until = RwSignal::new(0.0f64);
    let live_sync = move |t: f64| {
        if live_busy.get_untracked() || js_sys::Date::now() < live_paused_until.get_untracked() {
            return;
        }
        let (Some(e), Some(a)) = (ebook.get_untracked(), audio_asset.get_untracked()) else { return };
        // Text made from the recording is in sync already.
        if !READABLE.contains(&e.format.as_str()) || e.format == "transcript" {
            return;
        }
        let tr = track.get_untracked();
        let reach = sync.with_untracked(|s| s.covered_until(tr, t));
        if reach > t + 12.0 {
            return;
        }
        live_busy.set(true);
        let body = serde_json::json!({
            "ebook": e.id, "audio": a.id, "track": tr, "start": (reach - 1.0).max(0.0), "near": live_near.get_untracked(),
        });
        spawn_local(async move {
            match api::post::<_, serde_json::Value>(&format!("/api/works/{id}/sync/live"), &body).await {
                Ok(v) => {
                    let spans: Vec<SyncSpan> = serde_json::from_value(v["spans"].clone()).unwrap_or_default();
                    if spans.is_empty() {
                        // Nothing placed (music, an intro): skip ahead rather than asking again.
                        live_paused_until.set(js_sys::Date::now() + 8000.0);
                    }
                    if let Some(n) = v["near"].as_i64() {
                        live_near.set(Some(n));
                    }
                    sync.update(|s| s.merge(spans));
                    pairs.update(|p| if !p.contains(&(e.id, a.id)) { p.push((e.id, a.id)) });
                }
                Err(err) => {
                    // The voice server may be off or busy; try again a little later.
                    live_paused_until.set(js_sys::Date::now() + 30000.0);
                    leptos::logging::warn!("live sync: {}", err.0);
                }
            }
            live_busy.set(false);
        });
    };

    // The sentence whose words the read-along is following.
    let along_at = RwSignal::new(None::<(usize, usize)>);
    let on_time = move |_| {
        let Some(a) = audio_el() else { return };
        let t = a.current_time();
        time.set(t);
        let m = mode.get_untracked();
        if !m.audiobook() {
            return;
        }
        book_pos.set((track.get_untracked(), t));
        if playing.get_untracked() && m == Mode::ReadAudiobook {
            live_sync(t);
        }
        let s = sync.get_untracked();
        if s.is_empty() || !prefs.get_untracked().follow_audio && !playing.get_untracked() {
            return;
        }
        if let Some(span) = s.at(track.get_untracked(), t) {
            if span.sentence as i32 == sentence.get_untracked() && span.chapter == chapter.get_untracked() {
                // (Same sentence: make sure its words are being followed, e.g. after a pause.)
                if along_at.get_untracked() != Some((span.chapter, span.sentence)) {
                    if let Some(el) = page.get_untracked() {
                        crate::read_along(Some((*el).clone().into()), &a, span.sentence as i32, span.start, span.end, &span.words, prefs.get_untracked().follow_audio);
                        along_at.set(Some((span.chapter, span.sentence)));
                    }
                }
                return;
            }
            let looking = crate::browsing(book.get_untracked().map(|b| (*b).clone().into()));
            if span.chapter != chapter.get_untracked() && looking {
                // You've turned to another chapter yourself: stay there.
                return;
            }
            sentence.set(span.sentence as i32);
            if span.chapter != chapter.get_untracked() {
                scroll_to.set(Some(span.sentence as u32));
                chapter.set(span.chapter);
            } else if let Some(el) = page.get_untracked() {
                crate::highlight(&el, span.sentence as i32, prefs.get_untracked().follow_audio);
                crate::read_along(Some((*el).clone().into()), &a, span.sentence as i32, span.start, span.end, &span.words, prefs.get_untracked().follow_audio);
                along_at.set(Some((span.chapter, span.sentence)));
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

    let tap_seq = RwSignal::new(0u32);
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
        // A double or triple click is selecting a word or sentence: the single click that began it
        // mustn't also jump the audio or toggle the bars.
        tap_seq.update(|n| *n += 1);
        if ev.detail() > 1 {
            return;
        }
        let seq = tap_seq.get_untracked();
        let n = js_sys::Reflect::get(&target, &JsValue::from_str("sentence")).ok().and_then(|v| v.as_f64());
        set_timeout(
            move || {
                if tap_seq.try_get_untracked() != Some(seq) || !crate::selected_text().is_empty() {
                    return;
                }
                if let Some(n) = n {
                    let n = n as usize;
                    if mode.get_untracked().cast() {
                        cast_go(chapter.get_untracked(), n, playing.get_untracked());
                        return;
                    }
                    // Reading from here: the sentence tapped (where the sync has it, or where it most
                    // likely is from the sentences around it), and the page follows the recording
                    // again from it.
                    let c = chapter.get_untracked();
                    let target = sync.with_untracked(|x| {
                        x.of(c, n).map(|sp| (sp.track, sp.start)).or_else(|| {
                            let asset = audio_asset.get_untracked()?;
                            let lens: Vec<f64> = asset.tracks.iter().map(|t| t.duration_secs).collect();
                            let count = counts.with_untracked(|m| m.get(&c).copied()).unwrap_or(1);
                            x.estimate(c, n, count, &lens)
                        })
                    });
                    if let Some((tr, secs)) = target.filter(|_| mode.get_untracked().audiobook()) {
                        crate::stop_browsing(book.get_untracked().map(|b| (*b).clone().into()));
                        live_near.set(None);
                        sentence.set(n as i32);
                        if let Some(el) = page.get_untracked() {
                            crate::highlight(&el, n as i32, false);
                        }
                        book_pos.set((tr, secs));
                        seek_to(tr, secs);
                        return;
                    }
                }
                // A plain tap on the page shows or hides the bars.
                chrome_hidden.update(|h| *h = !*h);
            },
            std::time::Duration::from_millis(260),
        );
    };

    let position = move || -> Position {
        let a = audio_asset.get_untracked().filter(|_| mode.get_untracked().audiobook());
        let (tr, t) = book_pos.get_untracked();
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
    // (Sent so it outlives the page: a reload or closed tab still saves where you were. Not while
    // the book is still opening at your place: the first pages it lays out on the way there
    // aren't where you were.)
    let opened_at = js_sys::Date::now();
    let save = move || {
        if work.get_untracked().is_none() || js_sys::Date::now() - opened_at < 4000.0 {
            return;
        }
        if let Ok(json) = serde_json::to_string(&position()) {
            crate::put_keepalive(&format!("/api/works/{id}/position"), &json);
        }
    };
    // When the page goes away or out of sight, and after every page turn, as well as every few
    // seconds.
    let leave = Closure::<dyn Fn()>::new(save).into_js_value();
    let unhook = crate::on_leave(leave.unchecked_ref());
    on_cleanup(move || {
        let _ = unhook.call0(&JsValue::NULL);
    });
    Effect::new(move |_| {
        page_info.track();
        save();
    });
    if let Ok(handle) = set_interval_with_handle(save, std::time::Duration::from_secs(5)) {
        on_cleanup(move || {
            handle.clear();
            save();
        });
    }

    // Play starts from what you're reading: if the page on screen isn't where the recording is
    // (you've turned pages, or jumped), the recording moves there first.
    let toggle = move |_| {
        let Some(a) = audio_el() else { return };
        if !a.paused() {
            let _ = a.pause();
            return;
        }
        let m = mode.get_untracked();
        let c = chapter.get_untracked();
        let el = page.get_untracked();
        let showing = |s: usize| el.as_ref().is_some_and(|el| crate::sentence_on_page(el, s as i32));
        if m == Mode::ReadAudiobook && c != usize::MAX {
            let heard = sync.with_untracked(|x| x.at(track.get_untracked(), a.current_time()).cloned());
            if !heard.as_ref().is_some_and(|h| h.chapter == c && showing(h.sentence)) {
                // From the first word on the page (which may be partway through a sentence).
                let (s, w) = el.as_ref().map(|el| {
                    let v = crate::first_word_on_page(el);
                    (v.first().copied().unwrap_or(0) as usize, v.get(1).copied().unwrap_or(0) as usize)
                }).unwrap_or((0, 0));
                let target = sync.with_untracked(|x| {
                    x.of(c, s).map(|sp| (sp.track, sp.words.get(w).copied().unwrap_or(sp.start))).or_else(|| {
                        // Not synced yet: from the sentences around it that are (matching as it
                        // plays settles it on the right one).
                        let asset = audio_asset.get_untracked()?;
                        let lens: Vec<f64> = asset.tracks.iter().map(|t| t.duration_secs).collect();
                        let n = counts.with_untracked(|m| m.get(&c).copied()).unwrap_or(1);
                        x.estimate(c, s, n, &lens)
                    })
                }).or_else(|| {
                    // Not matched yet: the same point through the book, roughly; matching as it
                    // plays settles it on the right sentence.
                    let asset = audio_asset.get_untracked()?;
                    let chapters = layout.get_untracked()?.chapters.max(1);
                    let n = counts.with_untracked(|m| m.get(&c).copied()).unwrap_or(1).max(1);
                    let f = (c as f64 + s as f64 / n as f64) / chapters as f64;
                    let mut left = asset.duration_secs.unwrap_or(0.0) * f;
                    for (i, t) in asset.tracks.iter().enumerate() {
                        if left <= t.duration_secs {
                            return Some((i, left));
                        }
                        left -= t.duration_secs;
                    }
                    None
                });
                if let Some((tr, secs)) = target {
                    book_pos.set((tr, secs));
                    // (Where the sync last was is no guide to where this is.)
                    live_near.set(None);
                    crate::stop_browsing(book.get_untracked().map(|b| (*b).clone().into()));
                    if tr != track.get_untracked() {
                        pending_seek.set(Some((secs, true)));
                        track.set(tr);
                        return;
                    }
                    a.set_current_time(secs);
                }
            }
        } else if m == Mode::ReadCast {
            let (cc, cs) = cast_at.get_untracked();
            if !(cc == c && showing(cs)) && c != usize::MAX {
                let s = el.as_ref().map(|el| crate::visible_sentence(el) as usize).unwrap_or(0);
                cast_go(c, s, true);
                return;
            }
        }
        let _ = a.play();
    };
    let skip = move |d: f64| {
        if mode.get_untracked().cast() {
            let (c, s) = cast_at.get_untracked();
            let play = playing.get_untracked();
            if d > 0.0 {
                cast_go(c, s + 1, play);
            } else if s > 0 {
                cast_go(c, s - 1, play);
            } else if c > 0 {
                // Back into the previous chapter's last sentence.
                spawn_local(async move {
                    let n = counts.with_untracked(|m| m.get(&(c - 1)).copied()).unwrap_or(0);
                    pending_seek.set(Some((0.0, play)));
                    cast_at.set((c - 1, n.saturating_sub(1)));
                });
            }
            return;
        }
        if let Some(a) = audio_el() {
            a.set_current_time((a.current_time() + d).max(0.0));
        }
    };
    let track_url = move || match mode.get() {
        m if m.cast() => ebook.get().map(|e| cast_url(e.id, cast_at.get())).unwrap_or_default(),
        m if m.audiobook() => audio_asset.get().and_then(|a| a.tracks.get(track.get()).map(|t| t.url.clone())).unwrap_or_default(),
        _ => String::new(),
    };
    let has_player = move || match mode.get() {
        m if m.cast() => ebook.get().is_some(),
        m if m.audiobook() => audio_asset.get().is_some(),
        _ => false,
    };
    // Switching modes keeps your place: the audiobook picks up at the sentence you're on when the
    // two are matched there, the full cast starts at the sentence you were hearing or reading.
    let set_mode = move |m: Mode| {
        let old = mode.get_untracked();
        if old == m {
            return;
        }
        m.remember(id);
        editions_open.set(false);
        let was_playing = playing.get_untracked();
        let c = chapter.get_untracked();
        let s = if sentence.get_untracked() >= 0 {
            sentence.get_untracked() as usize
        } else {
            page.get_untracked().map(|el| crate::visible_sentence(&el) as usize).unwrap_or(0)
        };
        if old.cast() != m.cast() || old.audiobook() != m.audiobook() {
            if let Some(a) = audio_el() {
                let _ = a.pause();
            }
            if m.cast() {
                let (c, s) = if old.audiobook() {
                    let (tr, t) = book_pos.get_untracked();
                    sync.with_untracked(|x| x.at(tr, t).map(|sp| (sp.chapter, sp.sentence))).unwrap_or((c, s))
                } else {
                    (c, s)
                };
                cast_go(c, s, was_playing);
            } else if m.audiobook() {
                if let Some(span) = sync.with_untracked(|x| x.of(c, s).cloned()) {
                    book_pos.set((span.track, span.start));
                }
                let (tr, secs) = book_pos.get_untracked();
                pending_seek.set(Some((secs, was_playing)));
                track.set(tr);
            }
        }
        mode.set(m);
        if old.listening_only() && !m.listening_only() {
            scroll_to.set(Some(s as u32));
            set_timeout(move || lay_out.with_value(|f| f()), std::time::Duration::from_millis(30));
        }
    };
    let chapters = move || layout.get().map(|l| l.chapters).unwrap_or(0);

    // Starts the recording (or the full cast) at a sentence.
    let play_from = move |c: usize, n: usize| {
        let m = mode.get_untracked();
        if m.cast() {
            cast_go(c, n, true);
            return;
        }
        let Some(a) = audio_el() else { return };
        let target = sync.with_untracked(|x| x.of(c, n).map(|sp| (sp.track, sp.start)));
        match target {
            Some((tr, secs)) => {
                sentence.set(n as i32);
                book_pos.set((tr, secs));
                if tr != track.get_untracked() {
                    pending_seek.set(Some((secs, true)));
                    track.set(tr);
                } else {
                    a.set_current_time(secs);
                    let _ = a.play();
                }
            }
            // Not matched to the recording yet: Play finds the page's place (and matching settles it).
            None => {
                let _ = a.pause();
                set_timeout(move || { if let Some(b) = document().query_selector(".round.play").ok().flatten() { if let Ok(b) = b.dyn_into::<web_sys::HtmlElement>() { b.click(); } } }, std::time::Duration::ZERO);
            }
        }
    };

    // Right-click on the page: look up, explain or ask about the words there, or play from them.
    let menu = RwSignal::new(None::<(f64, f64, String, Option<(usize, usize)>)>);
    let assist = RwSignal::new(None::<(String, bool)>);
    // The dictionary card: where it shows, the word, and the sentence it's in.
    let dict = RwSignal::new(None::<(f64, f64, String, Option<String>)>);
    let on_context = move |ev: web_sys::MouseEvent| {
        ev.prevent_default();
        // The sentence first (selecting the word below would hide it).
        let n = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|el| el.closest(".s").ok().flatten())
            .and_then(|s| s.get_attribute("data-s"))
            .and_then(|v| v.parse::<f64>().ok());
        let mut text = crate::selected_text();
        if text.is_empty() {
            // Nothing selected: the word under the pointer (selected, so you can see which).
            text = crate::word_at(ev.client_x() as f64, ev.client_y() as f64);
        }
        let at = n.map(|n| (chapter.get_untracked(), n as usize));
        // Kept inside the window.
        let (w, h) = (
            window().inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1000.0),
            window().inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(800.0),
        );
        let x = (ev.client_x() as f64).min(w - 250.0).max(8.0);
        let y = (ev.client_y() as f64).min(h - 260.0).max(8.0);
        menu.set(Some((x, y, text.trim().to_string(), at)));
    };
    // Any click or key elsewhere closes it.
    let close_menu = Closure::<dyn Fn(web_sys::Event)>::new(move |ev: web_sys::Event| {
        if menu.get_untracked().is_none() && dict.get_untracked().is_none() {
            return;
        }
        let inside = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|el| el.closest(".context-menu, .dict-card").ok().flatten().is_some());
        if !inside {
            menu.set(None);
            dict.set(None);
        }
    });
    for kind in ["pointerdown", "keydown", "wheel"] {
        let _ = document().add_event_listener_with_callback(kind, close_menu.as_ref().unchecked_ref());
    }
    let close_menu = close_menu.into_js_value();
    on_cleanup(move || {
        for kind in ["pointerdown", "keydown", "wheel"] {
            let _ = document().remove_event_listener_with_callback(kind, close_menu.unchecked_ref());
        }
    });
    // The recording's real waveform behind the scrub bar, filled in up to where it's playing.
    let wave = NodeRef::<leptos::html::Canvas>::new();
    let peaks = RwSignal::new(Vec::<f32>::new());
    Effect::new(move |_| {
        let (Some(a), tr) = (audio_asset.get(), track.get()) else { return };
        if !mode.get().audiobook() {
            return;
        }
        peaks.set(Vec::new());
        spawn_local(async move {
            if let Ok(p) = api::get::<Vec<f32>>(&format!("/api/assets/{}/tracks/{tr}/peaks?v=2", a.id)).await {
                if track.try_get_untracked() == Some(tr) {
                    peaks.set(p);
                }
            }
        });
    });
    Effect::new(move |_| {
        let fraction = if duration.get() > 0.0 { time.get() / duration.get() } else { 0.0 };
        if let Some(c) = wave.get() {
            peaks.with(|p| crate::draw_wave(&c, &js_sys::Float32Array::from(&p[..]), fraction));
        }
    });
    let toc_title = move |c: usize| layout.get().and_then(|l| l.toc.iter().rev().find(|t| t.chapter <= c).map(|t| t.title.clone()));
    // The chapter being heard: the book's own chapter name when the audio is matched to the text,
    // otherwise the recording's.
    let now_chapter = move || {
        let m = mode.get();
        let heard = sync.with(|x| x.at(track.get(), time.get()).map(|sp| sp.chapter));
        match (m.cast(), heard) {
            (true, _) => toc_title(cast_at.get().0).unwrap_or_default(),
            (false, Some(c)) => toc_title(c).unwrap_or_default(),
            _ => audio_asset.get().and_then(|a| a.tracks.get(track.get()).map(|t| t.title.clone())).unwrap_or_default(),
        }
    };
    let go_chapter = move |c: usize| {
        scroll_to.set(Some(0));
        chapter.set(c);
        toc_open.set(false);
    };

    view! {
        <div class="reader" class:paged-mode=paged class:has-player=has_player class:listen-mode=move || mode.get().listening_only()
            class:chrome-hidden=move || chrome_hidden.get() && !mode.get().listening_only()
            on:mousemove=move |ev: web_sys::MouseEvent| {
                // On a desktop, moving toward the top or bottom edge brings the bars back.
                let h = window().inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(800.0);
                let y = ev.client_y() as f64;
                if chrome_hidden.get_untracked() && (y < 70.0 || y > h - 150.0) {
                    chrome_hidden.set(false);
                }
            }
            style=move || {
            let p = prefs.get();
            // Earlier versions saved plain Georgia as the default; read that as the book faces.
            let font = if p.font == "Georgia, serif" { duet_common::BOOK_FONT.to_string() } else { p.font.clone() };
            format!("--font-size:{}px;--line-height:{};--font:{}", font_size.get(), p.line_height, font)
        } data-theme=move || page_theme(&prefs.get().theme).to_string()>
            <header class="reader-bar">
                <a class="back" href=format!("#/work/{id}") aria-label="Back">"‹"</a>
                <div class="reader-title">
                    {move || work.get().map(|w| w.title)}
                    <small>{move || {
                        let c = chapter.get();
                        layout.get().and_then(|l| l.toc.iter().rev().find(|t| t.chapter <= c).map(|t| t.title.clone()))
                    }}</small>
                </div>
                <button class="icon-button theme-toggle" title="Light or dark pages" aria-label="Light or dark pages"
                    on:click=move |_| {
                        let next = if page_theme(&prefs.get_untracked().theme) == "dark" { "light" } else { "dark" };
                        prefs.update(|p| p.theme = next.into());
                        spawn_local(async move {
                            if let Ok(mut s) = api::get::<Settings>("/api/settings").await {
                                s.reader.theme = next.into();
                                let _ = api::put::<_, Settings>("/api/settings", &s).await;
                            }
                        });
                    }>
                    {move || if page_theme(&prefs.get().theme) == "dark" {
                        view! { <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg> }.into_any()
                    } else {
                        view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/></svg> }.into_any()
                    }}
                </button>
                <button class="icon-button" title=move || if fullscreen.get() { "Leave full screen" } else { "Full screen" }
                    aria-label="Full screen" on:click=move |_| spawn_local(async move {
                        let on = crate::toggle_fullscreen().await.as_bool().unwrap_or(false);
                        fullscreen.set(on);
                    })>
                    {move || if fullscreen.get() {
                        view! { <svg viewBox="0 0 24 24" aria-hidden="true" class="bar-icon"><path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5"/></svg> }.into_any()
                    } else {
                        view! { <svg viewBox="0 0 24 24" aria-hidden="true" class="bar-icon"><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/></svg> }.into_any()
                    }}
                </button>
                <button class="icon-button" title="Text and page view" on:click=move |_| view_open.update(|o| *o = !*o)>"Aa"</button>
                <Show when=move || work.get().is_some_and(|w| Mode::available(&w.assets).len() > 1 || w.assets.len() > 1)>
                    <button class="icon-button mode-button" title="Read or listen" on:click=move |_| editions_open.update(|o| *o = !*o)>
                        {move || mode.get().short()}
                        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 10l5 5 5-5"/></svg>
                    </button>
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
                        <span>"Page"</span>
                        <div class="swatches">
                            {[("light", "Light"), ("sepia", "Sepia"), ("gray", "Gray"), ("dark", "Dark")].map(|(t, label)| view! {
                                <button class=format!("swatch swatch-{t}") class:active=move || prefs.with(|p| page_theme(&p.theme) == t) title=label aria-label=label
                                    on:click=move |_| {
                                        prefs.update(|p| p.theme = t.into());
                                        spawn_local(async move {
                                            if let Ok(mut s) = api::get::<Settings>("/api/settings").await {
                                                s.reader.theme = t.into();
                                                let _ = api::put::<_, Settings>("/api/settings", &s).await;
                                            }
                                        });
                                    }></button>
                            })}
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
                            <span>"Page turns"</span>
                            <div class="segmented">
                                <button class:active=move || prefs.with(|p| p.page_curl) on:click=move |_| set_curl(true)>"Curl"</button>
                                <button class:active=move || prefs.with(|p| !p.page_curl) on:click=move |_| set_curl(false)>"Instant"</button>
                            </div>
                        </div>
                    </Show>
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
                <div class="editions-panel modes-panel">
                    {move || work.get().map(|w| {
                        let available = Mode::available(&w.assets);
                        let of = |k: AssetKind| w.assets.iter().filter(|a| a.kind == k).cloned().collect::<Vec<_>>();
                        let (texts, audios) = (of(AssetKind::Ebook), of(AssetKind::Audiobook));
                        // The text-from-the-recording edition, offered before it exists.
                        let make_text = !audios.is_empty() && !texts.iter().any(|a| a.format == "transcript");
                        view! {
                            <div class="mode-list">
                                {Mode::ALL.into_iter().map(|m| {
                                    let ok = available.contains(&m);
                                    let why = match m {
                                        Mode::Read => "Needs an ebook",
                                        Mode::ReadAudiobook => "Needs an ebook and an audiobook",
                                        Mode::ListenAudiobook => "Needs an audiobook",
                                        _ => "Needs an EPUB or text edition",
                                    };
                                    view! {
                                        <button class="mode-choice" class:on=move || mode.get() == m disabled=!ok
                                            on:click=move |_| set_mode(m)>
                                            <span class="mode-icon" aria-hidden="true">{m.icons().iter().map(|i| crate::icon(i)).collect_view()}</span>
                                            <span class="mode-text">
                                                <strong>{m.label()}</strong>
                                                <small>{if ok { m.detail() } else { why }}</small>
                                            </span>
                                        </button>
                                    }
                                }).collect_view()}
                            </div>
                            {(texts.len() + make_text as usize > 1).then(|| view! {
                                <div class="edition-group">
                                    <h4>"Text edition"</h4>
                                    {make_text.then(|| view! {
                                        <label class="choice">
                                            <input type="radio" name="text-edition" on:change=move |_| {
                                                spawn_local(async move {
                                                    match api::post::<_, Asset>(&format!("/api/works/{id}/transcript"), &()).await {
                                                        Ok(t) => {
                                                            work.update(|w| if let Some(w) = w { w.assets.push(t.clone()) });
                                                            switch_ebook(t);
                                                        }
                                                        Err(err) => toast.show(err.0),
                                                    }
                                                });
                                            }/>
                                            "Made from the audiobook (AI)"
                                        </label>
                                    })}
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
                            {(audios.len() > 1).then(|| view! {
                                <Show when=move || mode.get().audiobook()>
                                    <div class="edition-group">
                                        <h4>"Recording"</h4>
                                        {audios.clone().into_iter().map(|a| {
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
                                </Show>
                            })}
                        }
                    })}
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
                            inner_html=move || html.get() on:click=on_page_click on:contextmenu=on_context></article>
                    <Show when=move || paged() && l.mode != ReadMode::Images>
                        <div class="running-head" aria-hidden="true">
                            <span class="rh-book">{move || work.get().map(|w| w.title)}</span>
                            <span class="rh-chapter">{move || {
                                let c = chapter.get();
                                layout.get().and_then(|l| l.toc.iter().rev().find(|t| t.chapter <= c).map(|t| t.title.clone()))
                            }}</span>
                        </div>
                        // In two-page view each half sits under its own page, clear of the spine.
                        <div class="folio" aria-hidden="true">
                            <span class="folio-chapter">{move || {
                                let c = chapter.get();
                                let total = layout.get().map(|l| l.chapters).unwrap_or(0);
                                (total > 1).then(|| format!("Chapter {} of {total}", c.saturating_add(1).min(total)))
                            }}</span>
                            <span class="folio-page">{move || {
                                let (p, n) = page_info.get();
                                let rest = match n.saturating_sub(p) {
                                    0 => "last page in chapter".to_string(),
                                    1 => "1 page left in chapter".to_string(),
                                    k => format!("{k} pages left in chapter"),
                                };
                                format!("Page {p} of {n} · {rest}")
                            }}</span>
                        </div>
                    </Show>
                    </div>
                    // Scrolling: the way on sits at the end of the chapter, like turning to the next one.
                    <Show when=move || !paged()>
                        <nav class="chapter-end">
                            <Show when=move || { let c = chapter.get(); c > 0 && c != usize::MAX }>
                                <button class="ghost" on:click=move |_| go_chapter(chapter.get_untracked().saturating_sub(1))>"‹ Previous chapter"</button>
                            </Show>
                            <span class="muted">{move || format!("Chapter {} of {}", chapter.get().saturating_add(1).min(chapters()), chapters())}</span>
                            <Show when=move || chapter.get().saturating_add(1) < chapters()>
                                <button class="primary" on:click=move |_| go_chapter(chapter.get_untracked() + 1)>"Next chapter ›"</button>
                            </Show>
                        </nav>
                    </Show>
                }.into_any(),
                _ => view! {
                    <div class="listen-only">
                        {move || work.get().map(|w| crate::cover(w.cover_url, w.title))}
                    </div>
                }.into_any(),
            }}

            <Show when=move || mode.get().listening_only()>
                <div class="listen-stage">
                    <div class="listen-cover" class:playing=move || playing.get()>
                        {move || work.get().map(|w| crate::cover(w.cover_url, w.title))}
                    </div>
                    <h2>{move || work.get().map(|w| w.title)}</h2>
                    <p class="muted">{move || work.get().and_then(|w| w.author)}</p>
                    <p class="listen-chapter">{move || now_chapter()}</p>
                    <Show when=move || mode.get().cast()>
                        <blockquote class="listen-line">{move || {
                            html.track();
                            let (_, s) = cast_at.get();
                            page.get().map(|el| crate::sentence_text(&el, s as i32)).unwrap_or_default()
                        }}</blockquote>
                    </Show>
                </div>
            </Show>

            <Show when=has_player>
                // The full cast getting ready (the first lines of a chapter take a while): what it's doing.
                <Show when=move || cast_wait.get() && mode.get().cast()>
                    <div class="cast-wait" role="status">
                        <span class="spinner small"></span>
                        <span>{move || cast_status.get().unwrap_or_else(|| "Getting the full cast ready…".into())}</span>
                        <Show when=move || cast_progress.get().is_some()>
                            <div class="cast-progress"><div style=move || format!("width:{:.1}%", cast_progress.get().unwrap_or(0.0) * 100.0)></div></div>
                        </Show>
                    </div>
                </Show>
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
                            // The full cast's line: follow it word by word (its words timed once
                            // it's been performed).
                            if mode.get_untracked().cast() {
                                let Some(e) = ebook.get_untracked().map(|e| e.id) else { return };
                                let (c, n) = cast_at.get_untracked();
                                let dur = a.duration();
                                spawn_local(async move {
                                    let times = api::get::<Vec<f64>>(&format!("/api/assets/{e}/cast/{c}/{n}/words")).await.unwrap_or_default();
                                    if cast_at.try_get_untracked() != Some((c, n)) || chapter.try_get_untracked() != Some(c) {
                                        return;
                                    }
                                    let (Some(el), Some(a)) = (page.get_untracked(), audio_el()) else { return };
                                    crate::read_along(Some((*el).clone().into()), &a, n as i32, 0.0, dur, &times, prefs.get_untracked().follow_audio);
                                });
                            }
                        }
                        on:loadstart=move |_| if mode.get_untracked().cast() { cast_wait.set(true) }
                        on:canplay=move |_| cast_wait.set(false)
                        on:error=move |_| {
                            if !mode.get_untracked().cast() { return }
                            cast_wait.set(false);
                            // Ask again to find out why (the audio element doesn't say).
                            let Some(e) = ebook.get_untracked() else { return };
                            let url = cast_url(e.id, cast_at.get_untracked());
                            let at = cast_at.get_untracked();
                            spawn_local(async move {
                                match api::get::<serde_json::Value>(&url).await {
                                    // It works now (it was the voices restarting, or a moment's
                                    // hiccup): try the line again rather than give up on it.
                                    Ok(_) => {
                                        if cast_at.try_get_untracked() == Some(at) {
                                            if let Some(a) = audio_el() {
                                                pending_seek.set(Some((0.0, true)));
                                                a.load();
                                            }
                                        }
                                    }
                                    Err(err) if err.0.to_lowercase().contains("fetch") || err.0.contains("voice server") => {
                                        // The server (or its voices) is restarting: wait and try again.
                                        cast_wait.set(true);
                                        gloo_timers::future::TimeoutFuture::new(5000).await;
                                        if cast_at.try_get_untracked() == Some(at) {
                                            if let Some(a) = audio_el() {
                                                pending_seek.set(Some((0.0, true)));
                                                a.load();
                                            }
                                        }
                                    }
                                    Err(err) => toast.show(format!("The full cast couldn't perform this: {}", err.0)),
                                }
                            });
                        }
                        on:ended=move |_| {
                            if mode.get_untracked().cast() {
                                let (c, s) = cast_at.get_untracked();
                                cast_go(c, s + 1, true);
                                return;
                            }
                            let n = audio_asset.get_untracked().map(|a| a.tracks.len()).unwrap_or(0);
                            if track.get_untracked() + 1 < n {
                                pending_seek.set(Some((0.0, true)));
                                track.update(|t| *t += 1);
                            }
                        }></audio>
                    <div class="np">
                        {move || work.get().map(|w| crate::cover(w.cover_url, w.title))}
                        <div class="np-text">
                            <strong>{move || work.get().map(|w| w.title)}</strong>
                            <small>{move || {
                                let name = now_chapter();
                                if mode.get().cast() {
                                    if cast_wait.get() {
                                        return format!("{name} · {}", cast_status.get().unwrap_or_else(|| "getting the voices ready…".into()));
                                    }
                                    return format!("{name} · full cast");
                                }
                                let left = (duration.get() - time.get()).max(0.0);
                                let listening = if live_busy.get() { " · matching to the text" } else { "" };
                                format!("{name} · −{}{listening}", format_time(left))
                            }}</small>
                        </div>
                    </div>
                    <div class="transport">
                        <button class="round" title=move || if mode.get().cast() { "Previous sentence" } else { "Back 30 seconds" } on:click=move |_| skip(-30.0)>
                            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 12a8 8 0 1 0 2.3-5.6M4 4v4h4"/></svg><span>{move || if mode.get().cast() { "" } else { "30" }}</span>
                        </button>
                        <button class="round play" class:playing=move || playing.get() title="Play or pause" on:click=toggle>
                            {move || if playing.get() {
                                view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path class="fill" d="M7 5h3.5v14H7zM13.5 5H17v14h-3.5z"/></svg> }.into_any()
                            } else {
                                view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path class="fill" d="M8 5.5v13l11-6.5z"/></svg> }.into_any()
                            }}
                        </button>
                        <button class="round" title=move || if mode.get().cast() { "Next sentence" } else { "Forward 30 seconds" } on:click=move |_| skip(30.0)>
                            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 12a8 8 0 1 1-2.3-5.6M20 4v4h-4"/></svg><span>{move || if mode.get().cast() { "" } else { "30" }}</span>
                        </button>
                    </div>
                    <div class="np-extra">
                        <select class="speed" title="Speed" on:change=move |ev| {
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
                    <Show when=move || mode.get().cast()>
                        <div class="scrub">
                            <span>{move || format!("{}", cast_at.get().1 + 1)}</span>
                            <input type="range" min="0" step="1"
                                prop:max=move || counts.with(|m| m.get(&cast_at.get().0).copied().unwrap_or(1)).saturating_sub(1) as f64
                                prop:value=move || cast_at.get().1 as f64
                                style=move || {
                                    let n = counts.with(|m| m.get(&cast_at.get().0).copied().unwrap_or(1)).max(1);
                                    format!("--pct:{:.2}%", cast_at.get().1 as f64 / (n.saturating_sub(1)).max(1) as f64 * 100.0)
                                }
                                on:change=move |ev| {
                                    if let Ok(v) = event_target_value(&ev).parse::<usize>() { cast_go(cast_at.get_untracked().0, v, playing.get_untracked()) }
                                }/>
                            <span>{move || format!("of {} sentences", counts.with(|m| m.get(&cast_at.get().0).copied().unwrap_or(0)))}</span>
                        </div>
                    </Show>
                    <div class="scrub" class:hidden=move || mode.get().cast()>
                        <span>{move || format_time(time.get())}</span>
                        <div class="wave">
                            <canvas node_ref=wave aria-hidden="true"></canvas>
                            <input type="range" min="0" step="1" aria-label="Position in the recording"
                                prop:max=move || duration.get() prop:value=move || time.get()
                                on:input=move |ev| {
                                    if let (Some(a), Ok(v)) = (audio_el(), event_target_value(&ev).parse::<f64>()) { a.set_current_time(v) }
                                }/>
                        </div>
                        <span>{move || format_time(duration.get())}</span>
                    </div>
                </div>
            </Show>

            <Show when=move || menu.get().is_some()>
                {move || menu.get().map(|(x, y, text, at)| {
                    let short = if text.chars().count() > 28 { format!("{}…", text.chars().take(26).collect::<String>()) } else { text.clone() };
                    let (t1, t2, t3, t4) = (text.clone(), text.clone(), text.clone(), text.clone());
                    // The sentence the words are in, so the answer is about this use of them.
                    let context = at
                        .and_then(|(_, n)| page.get_untracked().map(|el| crate::sentence_text(&el, n as i32)))
                        .filter(|s| !s.is_empty() && s.trim() != text.trim());
                    view! {
                        <div class="context-menu" style=format!("left:{x}px;top:{y}px") on:contextmenu=|ev| ev.prevent_default()>
                            {(!text.is_empty()).then(|| view! {
                                <button on:click={
                                    let context = context.clone();
                                    move |_| {
                                        menu.set(None);
                                        if t1.split_whitespace().count() <= 3 {
                                            // Words and short phrases: the dictionary.
                                            dict.set(Some((x, y, t1.clone(), context.clone())));
                                        } else {
                                            assist.set(Some((
                                                match &context {
                                                    Some(sent) => format!("What does \"{t1}\" mean here: \"{sent}\"?"),
                                                    None => format!("What does \"{t1}\" mean here?"),
                                                },
                                                true,
                                            )));
                                        }
                                    }
                                }>
                                    <span>"Define"</span><small>{format!("“{short}”")}</small>
                                </button>
                                <button on:click=move |_| { menu.set(None); assist.set(Some((format!("Explain this passage: \"{t2}\""), true))); }>
                                    <span>"Explain"</span><small>"in plain words, no spoilers"</small>
                                </button>
                                <button on:click=move |_| { menu.set(None); assist.set(Some((format!("About \"{t3}\": "), false))); }>
                                    <span>"Ask AI about this…"</span>
                                </button>
                            })}
                            {text.is_empty().then(|| view! {
                                <button on:click=move |_| { menu.set(None); assist.set(Some((String::new(), false))); }>
                                    <span>"Ask AI about the book…"</span>
                                </button>
                            })}
                            {at.filter(|_| mode.get_untracked() != Mode::Read).map(|(c, n)| view! {
                                <button on:click=move |_| { menu.set(None); play_from(c, n); }>
                                    <span>"Play from here"</span>
                                </button>
                            })}
                            {(!text.is_empty()).then(|| view! {
                                <button on:click=move |_| {
                                    menu.set(None);
                                    crate::copy_text(&t4);
                                }>
                                    <span>"Copy"</span>
                                </button>
                            })}
                        </div>
                    }
                })}
            </Show>
            {move || dict.get().map(|(x, y, word, context)| view! { <DictCard x y word context dict assist/> })}
            <Assistant id work audio playing position request=assist dict/>
        </div>
    }
}

/// A word looked up in the dictionary, in a card beside it; the AI is one click away for what
/// the word means in this particular sentence.
#[component]
fn DictCard(
    x: f64,
    y: f64,
    word: String,
    context: Option<String>,
    dict: RwSignal<Option<(f64, f64, String, Option<String>)>>,
    assist: RwSignal<Option<(String, bool)>>,
) -> impl IntoView {
    let entries = RwSignal::new(None::<Result<Vec<duet_common::DictEntry>, String>>);
    let w = word.clone();
    spawn_local(async move {
        let r = api::get::<Vec<duet_common::DictEntry>>(&format!("/api/define?word={}", js_sys::encode_uri_component(&w))).await;
        let _ = entries.try_set(Some(r.map_err(|e| e.0)));
    });
    let (vw, vh) = (
        window().inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1000.0),
        window().inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(800.0),
    );
    let style = format!("left:{}px;top:{}px", x.min(vw - 380.0).max(8.0), y.min(vh - 120.0).max(8.0));
    let ask_ai = {
        let word = word.clone();
        move |_| {
            dict.set(None);
            assist.set(Some((
                match &context {
                    Some(sent) => format!("What does \"{word}\" mean in this sentence: \"{sent}\"?"),
                    None => format!("What does \"{word}\" mean here?"),
                },
                true,
            )));
        }
    };
    view! {
        <div class="dict-card" style=style on:contextmenu=|ev| ev.prevent_default()>
            <header>
                <strong class="dict-word">{move || entries.with(|e| match e { Some(Ok(v)) if !v.is_empty() => v[0].word.clone(), _ => word.clone() })}</strong>
                <span class="dict-phonetic">{move || entries.with(|e| match e { Some(Ok(v)) => v.iter().find_map(|x| x.phonetic.clone()), _ => None })}</span>
                <button class="icon-button" aria-label="Close" on:click=move |_| dict.set(None)>"×"</button>
            </header>
            <div class="dict-body">
                {move || match entries.get() {
                    None => view! { <p class="hint"><span class="spinner small"></span>" Looking it up…"</p> }.into_any(),
                    Some(Err(e)) => view! { <p class="hint">{format!("The dictionary isn't reachable ({e}).")}</p> }.into_any(),
                    Some(Ok(v)) if v.is_empty() => view! { <p class="hint">"Not in the dictionary."</p> }.into_any(),
                    Some(Ok(v)) => v.into_iter().flat_map(|e| e.meanings).take(4).map(|m| view! {
                        <section>
                            <em class="dict-pos">{m.part_of_speech}</em>
                            <ol>
                                {m.definitions.into_iter().take(4).map(|d| view! {
                                    <li>{d.definition}{d.example.map(|ex| view! { <span class="dict-example">{format!("“{ex}”")}</span> })}</li>
                                }).collect_view()}
                            </ol>
                        </section>
                    }).collect_view().into_any(),
                }}
            </div>
            <footer>
                <small>"Wiktionary"</small>
                <button class="link" on:click=ask_ai>"What does it mean here? Ask AI"</button>
            </footer>
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
    // Something to ask from elsewhere (the page's right-click menu): (text, ask now or just fill in).
    request: RwSignal<Option<(String, bool)>>,
    dict: RwSignal<Option<(f64, f64, String, Option<String>)>>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    // Opening the panel gets speech recognition ready for the microphone.
    Effect::new(move |_| {
        if open.get() && crate::mic_supported() {
            crate::warm_speech();
        }
    });
    let input = RwSignal::new(String::new());
    let messages = RwSignal::new(Vec::<Message>::new());
    let persona = RwSignal::new(None::<i64>);
    let speak = RwSignal::new(true);
    let recording = RwSignal::new(false);
    // Asking for the microphone (a permission prompt may be up), and how loud it hears you.
    let mic_starting = RwSignal::new(false);
    let mic_level = RwSignal::new(0.0f64);
    let thinking = RwSignal::new(false);
    let speaking = RwSignal::new(false);
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
            // The text first; the voice is fetched after (it can take a while to perform).
            speak: false,
            history,
        };
        let persona_id = persona.get_untracked();
        spawn_local(async move {
            match api::post::<_, AskReply>("/api/assistant/ask", &req).await {
                Ok(r) => {
                    let spoken = if r.spoken.is_empty() { r.answer.clone() } else { r.spoken.clone() };
                    messages.update(|m| m.push(Message { speaker: r.speaker, text: r.answer, mine: false }));
                    thinking.set(false);
                    if speak.get_untracked() {
                        speaking.set(true);
                        let say = duet_common::SayRequest { work_id: Some(id), persona: persona_id, text: spoken };
                        match api::post::<_, String>("/api/assistant/say", &say).await {
                            Ok(a) => crate::play_data_url(&a),
                            Err(e) => leptos::logging::warn!("speaking the answer: {}", e.0),
                        }
                        let _ = speaking.try_set(false);
                    }
                }
                Err(e) => {
                    toast.show(e.0);
                    thinking.set(false);
                }
            }
        });
    };

    let mic = move |_| {
        if recording.get_untracked() {
            recording.set(false);
            spawn_local(async move {
                match crate::stop_recording().await {
                    Ok(text) => {
                        let text = text.as_string().unwrap_or_default();
                        input.set(text.clone());
                        if text.trim().is_empty() {
                            toast.show("Didn't catch any words. Is the right microphone selected?".to_string());
                        }
                        ask(text)
                    }
                    Err(e) => toast.show(format!("Couldn't transcribe: {}", js_error(&e))),
                }
            });
        } else {
            // The words fill the box as you talk.
            let on_partial = Closure::<dyn Fn(JsValue)>::new(move |text: JsValue| {
                if recording.get_untracked() {
                    input.set(text.as_string().unwrap_or_default());
                }
            })
            .into_js_value();
            let on_level = Closure::<dyn Fn(f64)>::new(move |l: f64| mic_level.set(l)).into_js_value();
            if mic_starting.get_untracked() {
                return;
            }
            mic_starting.set(true);
            input.set(String::new());
            spawn_local(async move {
                let started = crate::start_recording(on_partial.unchecked_ref(), on_level.unchecked_ref()).await;
                mic_starting.set(false);
                match started {
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

    Effect::new(move |_| {
        let Some((text, now)) = request.get() else { return };
        request.set(None);
        open.set(true);
        if now {
            ask(text);
        } else {
            input.set(text);
            // Into the box, ready to finish the question.
            set_timeout(
                || {
                    if let Some(el) = document().query_selector(".assistant form input").ok().flatten() {
                        if let Ok(el) = el.dyn_into::<web_sys::HtmlElement>() {
                            let _ = el.focus();
                        }
                    }
                },
                std::time::Duration::from_millis(50),
            );
        }
    });

    let define = move |_| {
        let sel = crate::selected_text();
        if sel.is_empty() {
            toast.show("Select a word or phrase first");
        } else if sel.split_whitespace().count() <= 3 {
            let w = window().inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1000.0);
            let h = window().inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(800.0);
            dict.set(Some(((w - 400.0).max(8.0), (h - 480.0).max(8.0), sel.trim().to_string(), None)));
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
                            <p inner_html=light_markdown(&item.1.text)></p>
                        </div>
                    </For>
                    <Show when=move || thinking.get()><p class="hint thinking"><span class="spinner small"></span>"Thinking…"</p></Show>
                    <Show when=move || speaking.get()><p class="hint thinking"><span class="spinner small"></span>"Getting the voice ready…"</p></Show>
                </div>
                <form on:submit=move |ev| { ev.prevent_default(); ask(input.get_untracked()) }>
                    <input type="text" prop:value=move || input.get()
                        placeholder=move || if mic_starting.get() { "Waiting for the microphone…" } else if recording.get() { "Listening…" } else { "Type a question" }
                        on:input=move |ev| input.set(event_target_value(&ev))/>
                    <Show when=crate::mic_supported>
                        <button type="button" class="mic" class:recording=move || recording.get() class:starting=move || mic_starting.get() on:click=mic
                            style:--level=move || format!("{:.2}", if recording.get() { mic_level.get() } else { 0.0 })
                            title=move || if recording.get() { "Stop and send" } else { "Talk" } aria-label="Talk">
                            {move || if recording.get() {
                                view! { <svg viewBox="0 0 24 24" aria-hidden="true" class="mic-icon"><rect x="7" y="7" width="10" height="10" rx="2.5" class="fill"/></svg> }.into_any()
                            } else {
                                view! { <svg viewBox="0 0 24 24" aria-hidden="true" class="mic-icon"><rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21M8.5 21h7"/></svg> }.into_any()
                            }}
                        </button>
                    </Show>
                </form>
            </aside>
        </Show>
    }
}

/// Chat answers use a little Markdown: **bold**, *italic*, `code`. Everything else is escaped.
fn light_markdown(text: &str) -> String {
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut out = String::new();
    let mut rest = escaped.as_str();
    // Pairs of markers become tags; an unpaired one is left as it is.
    'outer: while !rest.is_empty() {
        for (mark, tag) in [("**", "strong"), ("*", "em"), ("`", "code")] {
            if let Some(body) = rest.strip_prefix(mark) {
                if let Some(end) = body.find(mark).filter(|&e| e > 0) {
                    out.push_str(&format!("<{tag}>{}</{tag}>", &body[..end]));
                    rest = &body[end + mark.len()..];
                    continue 'outer;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn js_error(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message())).or_else(|| e.as_string()).unwrap_or_else(|| "unknown error".into())
}
