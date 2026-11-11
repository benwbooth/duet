//! The duet web UI: a Leptos single-page app for the library, the synced reader/player, search
//! and downloads, background jobs and settings. Layouts adapt to desktop and phone widths.

mod api;
mod cast_panel;
mod discover;
mod editions;
mod podcast;
mod reader;
mod settings;
mod setup;

use duet_common::{Asset, AssetKind, Job, JobKind, NewJob, ScanReport, WorkDetail, WorkSummary};

#[derive(Clone, Debug, serde::Deserialize)]
struct CoverOption {
    index: usize,
    url: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct OnlineCover {
    url: String,
    thumb: String,
    label: String,
}
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/helpers.js")]
extern "C" {
    #[wasm_bindgen(js_name = micSupported)]
    fn mic_supported() -> bool;
    #[wasm_bindgen(js_name = readAlong)]
    fn read_along(container: Option<web_sys::Element>, audio: &web_sys::HtmlAudioElement, n: i32, start: f64, end: f64, times: &[f64], follow: bool);
    fn browsing(book: Option<web_sys::Element>) -> bool;
    #[wasm_bindgen(js_name = stopBrowsing)]
    fn stop_browsing(book: Option<web_sys::Element>);
    #[wasm_bindgen(js_name = firstWordOnPage)]
    fn first_word_on_page(container: &web_sys::Element) -> Vec<u32>;
    #[wasm_bindgen(js_name = warmSpeech)]
    fn warm_speech();
    #[wasm_bindgen(js_name = startRecording, catch)]
    async fn start_recording(on_partial: &js_sys::Function, on_level: &js_sys::Function) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = stopRecording, catch)]
    async fn stop_recording() -> Result<JsValue, JsValue>;
    fn highlight(container: &web_sys::Element, n: i32, scroll: bool);
    #[wasm_bindgen(js_name = visibleSentence)]
    fn visible_sentence(container: &web_sys::Element) -> u32;
    #[wasm_bindgen(js_name = scrollToSentence)]
    fn scroll_to_sentence(container: &web_sys::Element, n: u32);
    #[wasm_bindgen(js_name = comicLoad)]
    async fn comic_load(container: &web_sys::Element, asset: f64, translate: bool) -> JsValue;
    #[wasm_bindgen(js_name = comicClear)]
    fn comic_clear(container: &web_sys::Element);
    #[wasm_bindgen(js_name = comicSetRtl)]
    fn comic_set_rtl(container: &web_sys::Element, rtl: bool);
    #[wasm_bindgen(js_name = comicShow)]
    fn comic_show(container: &web_sys::Element, page: u32, line: i32, follow: bool);
    #[wasm_bindgen(js_name = visiblePage)]
    fn visible_page(container: &web_sys::Element) -> i32;
    #[wasm_bindgen(js_name = panelOpen)]
    fn panel_open(container: &web_sys::Element, asset: f64, page: u32, pages: u32, rtl: bool, translate: bool, on_close: &js_sys::Function);
    #[wasm_bindgen(js_name = panelClose)]
    fn panel_close();
    #[wasm_bindgen(js_name = clickTarget)]
    fn click_target(ev: &web_sys::MouseEvent) -> JsValue;
    #[wasm_bindgen(js_name = selectedText)]
    fn selected_text() -> String;
    #[wasm_bindgen(js_name = playDataUrl)]
    fn play_data_url(url: &str);
    #[wasm_bindgen(js_name = playAnswerPart)]
    async fn play_answer_part(url: &str);
    #[wasm_bindgen(js_name = stopAnswer)]
    fn stop_answer();
    #[wasm_bindgen(js_name = formatTime)]
    fn format_time(secs: f64) -> String;
    #[wasm_bindgen(js_name = setMediaSession)]
    fn set_media_session(title: &str, artist: &str, artwork: &str);
    #[wasm_bindgen(js_name = pagerInit)]
    fn pager_init(
        book: &web_sys::Element,
        article: &web_sys::Element,
        on_next: &js_sys::Function,
        on_prev: &js_sys::Function,
        on_page: &js_sys::Function,
        spread: &str,
    );
    #[wasm_bindgen(js_name = pagerSetSpread)]
    fn pager_set_spread(book: &web_sys::Element, spread: &str);
    #[wasm_bindgen(js_name = pagerDestroy)]
    fn pager_destroy(book: &web_sys::Element);
    #[wasm_bindgen(js_name = pagerEnd)]
    fn pager_end(book: &web_sys::Element);
    #[wasm_bindgen(js_name = sidebarHidden)]
    fn sidebar_hidden() -> bool;
    #[wasm_bindgen(js_name = setSidebarHidden)]
    fn set_sidebar_hidden(hidden: bool);
    #[wasm_bindgen(js_name = einkOn)]
    fn eink_on() -> bool;
    #[wasm_bindgen(js_name = setEink)]
    fn set_eink(on: bool);
    #[wasm_bindgen(js_name = isFullscreen)]
    fn is_fullscreen() -> bool;
    #[wasm_bindgen(js_name = toggleFullscreen)]
    async fn toggle_fullscreen() -> JsValue;
    #[wasm_bindgen(js_name = onLeave)]
    fn on_leave(save: &js_sys::Function) -> js_sys::Function;
    #[wasm_bindgen(js_name = putKeepalive)]
    fn put_keepalive(url: &str, json: &str);
    #[wasm_bindgen(js_name = pagerSetNeighbours)]
    fn pager_set_neighbours(book: &web_sys::Element, next: Option<String>, prev: Option<String>);
    #[wasm_bindgen(js_name = addPictures)]
    fn add_pictures(container: &web_sys::Element, list: &str) -> u32;
    #[wasm_bindgen(js_name = countChapterPages)]
    fn count_chapter_pages(book: &web_sys::Element, chapter: usize, html: &str) -> u32;
    #[wasm_bindgen(js_name = pagerSetBookPages)]
    fn pager_set_book_pages(book: &web_sys::Element, base: u32, total: u32);
    #[wasm_bindgen(js_name = pagerSetCurl)]
    fn pager_set_curl(book: &web_sys::Element, on: bool);
    #[wasm_bindgen(js_name = pagerSetPlace)]
    fn pager_set_place(book: &web_sys::Element, chapter: u32, chapters: u32);
    #[wasm_bindgen(js_name = bookCover)]
    fn book_cover(book: &web_sys::Element, title: &str, cover_url: Option<String>);
    #[wasm_bindgen(js_name = wordAt)]
    fn word_at(x: f64, y: f64) -> String;
    #[wasm_bindgen(js_name = copyText)]
    fn copy_text(text: &str);
    #[wasm_bindgen(js_name = sentenceOnPage)]
    fn sentence_on_page(container: &web_sys::Element, n: i32) -> bool;
    #[wasm_bindgen(js_name = sentenceText)]
    fn sentence_text(container: &web_sys::Element, n: i32) -> String;
    async fn prefetch(url: &str) -> JsValue;
    #[wasm_bindgen(js_name = drawWave)]
    fn draw_wave(canvas: &web_sys::HtmlCanvasElement, peaks: &js_sys::Float32Array, fraction: f64);
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

/// Pages, addressed by the URL hash so links and the back button work without server routes.
#[derive(Clone, Debug, PartialEq)]
enum Route {
    Library,
    Work(i64),
    Read(i64, Option<reader::Mode>),
    Discover,
    Briefing,
    Jobs,
    Settings,
    Setup,
}

impl Route {
    fn parse(hash: &str) -> Route {
        let parts: Vec<&str> = hash.trim_start_matches('#').trim_start_matches('/').split('/').collect();
        let id = || parts.get(1).and_then(|s| s.parse().ok());
        match parts.first().copied() {
            Some("work") => id().map(Route::Work).unwrap_or(Route::Library),
            Some("read") => id().map(|i| Route::Read(i, parts.get(2).and_then(|m| reader::Mode::parse(m)))).unwrap_or(Route::Library),
            Some("discover") => Route::Discover,
            Some("briefing") => Route::Briefing,
            Some("jobs") => Route::Jobs,
            Some("settings") => Route::Settings,
            Some("setup") => Route::Setup,
            _ => Route::Library,
        }
    }

    fn tab(&self) -> &'static str {
        match self {
            Route::Library | Route::Work(_) | Route::Read(..) => "library",
            Route::Discover => "discover",
            Route::Briefing => "briefing",
            Route::Jobs => "jobs",
            Route::Settings | Route::Setup => "settings",
        }
    }
}

fn current_hash() -> String {
    window().location().hash().unwrap_or_default()
}

pub fn go(path: &str) {
    let _ = window().location().set_hash(path);
}

/// A transient message at the bottom of the screen.
#[derive(Clone, Copy)]
pub struct Toast(RwSignal<Option<String>>);

impl Toast {
    pub fn show(&self, msg: impl Into<String>) {
        let sig = self.0;
        let msg = msg.into();
        sig.set(Some(msg.clone()));
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(4000).await;
            if sig.get_untracked().as_deref() == Some(msg.as_str()) {
                sig.set(None);
            }
        });
    }
}

pub fn toast() -> Toast {
    expect_context::<Toast>()
}

/// The downloads panel in the lower-right corner: it opens when a download starts and follows
/// progress until everything is done.
#[derive(Clone, Copy)]
pub struct DownloadsPanel {
    open: RwSignal<bool>,
    list: RwSignal<Vec<duet_common::Download>>,
    collapsed: RwSignal<bool>,
}

impl DownloadsPanel {
    pub fn opened(&self) {
        self.open.set(true);
        self.collapsed.set(false);
        self.refresh();
    }

    fn refresh(&self) {
        let list = self.list;
        spawn_local(async move {
            if let Ok(v) = api::get::<Vec<duet_common::Download>>("/api/downloads").await {
                // Don't touch the DOM when nothing material changed:
                // re-setting an identical list still re-renders every row.
                let same = list.with(|old| {
                    old.len() == v.len()
                        && old.iter().zip(v.iter()).all(|(a, b)| {
                            a.id == b.id
                                && a.state == b.state
                                && (a.progress * 100.0) as i64 == (b.progress * 100.0) as i64
                                && a.size == b.size
                                && a.error == b.error
                        })
                });
                if !same {
                    list.set(v);
                }
            }
        });
    }
}

pub fn downloads_panel() -> DownloadsPanel {
    expect_context::<DownloadsPanel>()
}

fn active(d: &duet_common::Download) -> bool {
    !matches!(d.state.as_str(), "imported" | "failed" | "uploading" | "stalledUP" | "pausedUP" | "stoppedUP" | "queuedUP" | "forcedUP")
        && d.progress < 1.0
}

/// Something duet is doing in the background (see the server's activity module).
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct Activity {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub done: u32,
    pub total: u32,
    /// 0..1, or below 0 when there's no telling.
    pub fraction: f64,
    pub seconds: u64,
}

/// The background work as a list with progress bars (the jobs pane, and the Jobs page).
#[component]
pub fn ActivityList(list: Signal<Vec<Activity>>) -> impl IntoView {
    view! {
        <ul class="activity-list">
            // (Keyed on what's shown too, not just which work it is: keyed on the work alone, a
            // row kept its first numbers, and a chapter's pictures read "0 of 15" however many
            // had been drawn.)
            <For each=move || list.get() key=|a| format!("{}|{}|{}|{}|{}", a.key, a.detail, a.done, a.total, a.title) let:a>
                {
                    let known = a.fraction >= 0.0;
                    let pct = (a.fraction * 100.0).clamp(0.0, 100.0);
                    // (Lining up the audio counts seconds of the track, and warming up the voices
                    // seconds of the start: their details say how far, as a time.)
                    let count = (a.total > 0 && a.kind != "Audio sync" && !a.detail.starts_with("Warming up")).then(|| format!("{} of {}", a.done.min(a.total), a.total));
                    view! {
                        <li>
                            <div class="dl-body">
                                <div class="dl-name" title=a.title.clone()>{format!("{} · {}", a.kind, a.title)}</div>
                                <div class="act-detail">{a.detail.clone()}</div>
                                <div class="progress" class:indeterminate=!known><div style=if known { format!("width:{pct:.1}%") } else { String::new() }></div></div>
                                {count.map(|c| view! { <div class="dl-meta"><span>{c}</span></div> })}
                            </div>
                        </li>
                    }
                }
            </For>
        </ul>
    }
}

/// The jobs pane in the lower-left corner: what's going on in the background (pictures being
/// drawn, the full cast directed and performed ahead, books being synced), with progress. It
/// opens when new work starts, and can be closed till the next.
#[component]
fn ActivityPane() -> impl IntoView {
    let list = RwSignal::new(Vec::<Activity>::new());
    let open = RwSignal::new(false);
    let collapsed = RwSignal::new(false);
    let seen = StoredValue::new(std::collections::HashSet::<String>::new());
    let refresh = move || {
        spawn_local(async move {
            if let Ok(v) = api::get::<Vec<Activity>>("/api/activity").await {
                // New work: show it.
                let fresh = v.iter().any(|a| !seen.with_value(|s| s.contains(&a.key)));
                seen.update_value(|s| s.extend(v.iter().map(|a| a.key.clone())));
                if fresh {
                    open.set(true);
                }
                list.set(v);
            }
        });
    };
    refresh();
    if let Ok(h) = set_interval_with_handle(refresh, std::time::Duration::from_secs(2)) {
        on_cleanup(move || h.clear());
    }
    view! {
        <Show when=move || open.get() && !list.with(Vec::is_empty)>
            <aside class="downloads-pane activity-pane" class:collapsed=move || collapsed.get()>
                <header on:click=move |_| collapsed.update(|c| *c = !*c)>
                    <span class="dl-head-icon"><span class="spinner small"></span></span>
                    <strong>{move || match list.with(Vec::len) {
                        1 => "Working on 1 thing".to_string(),
                        n => format!("Working on {n} things"),
                    }}</strong>
                    <button class="icon-button" aria-label="Collapse">{move || if collapsed.get() { "▴" } else { "▾" }}</button>
                    <button class="icon-button" aria-label="Close" on:click=move |ev| { ev.stop_propagation(); open.set(false) }>"×"</button>
                </header>
                <Show when=move || !collapsed.get()>
                    <ActivityList list=list.into()/>
                </Show>
            </aside>
        </Show>
    }
}

#[component]
fn DownloadsPane() -> impl IntoView {
    let panel = downloads_panel();
    let toast = toast();
    // Follow progress ONLY while something is actually moving. When idle the
    // pane goes completely quiet — no periodic repaints, no flicker. A
    // manual refresh button covers torrents started elsewhere.
    let tick = RwSignal::new(0u32);
    if let Ok(h) = set_interval_with_handle(
        move || {
            tick.update(|t| *t += 1);
            if web_sys::window().and_then(|w| w.document()).map(|d| d.hidden()).unwrap_or(false) {
                return;
            }
            let watching = panel.open.get_untracked() && panel.list.with(|l| l.iter().any(active));
            if watching || tick.get_untracked() % 24 == 0 {
                panel.refresh();
            }
        },
        std::time::Duration::from_secs(5),
    ) {
        on_cleanup(move || h.clear());
    }
    panel.refresh();
    // Open by itself when NEW work starts. (Not merely while work is
    // active: that fight re-opens the pane right after the user closes it.)
    let seen_active = RwSignal::new(std::collections::HashSet::<String>::new());
    Effect::new(move |_| {
        let fresh = panel.list.with(|l| {
            l.iter()
                .filter(|d| active(d) && !seen_active.with(|s| s.contains(&d.id)))
                .map(|d| d.id.clone())
                .collect::<Vec<_>>()
        });
        if !fresh.is_empty() {
            seen_active.update(|s| s.extend(fresh));
            panel.open.set(true);
        }
    });
    // Newest first; finished ones from earlier sessions aren't news.
    let shown = move || panel.list.get().into_iter().take(12).collect::<Vec<_>>();
    let busy = move || panel.list.with(|l| l.iter().filter(|d| active(d)).count());
    // Items with nothing left to do: dismissible one by one, or all at once.
    let any_finished = move || panel.list.with(|l| l.iter().any(|d| !active(d)));
    let dismiss = move |id: String| {
        spawn_local(async move {
            match api::remove(&format!("/api/downloads/{id}")).await {
                Ok(()) => panel.refresh(),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let clear_finished = move |_| {
        let ids: Vec<String> = panel.list.with(|l| l.iter().filter(|d| !active(d)).map(|d| d.id.clone()).collect());
        spawn_local(async move {
            for id in ids {
                let _ = api::remove(&format!("/api/downloads/{id}")).await;
            }
            panel.refresh();
        });
    };
    view! {
        <Show when=move || panel.open.get()>
            <aside class="downloads-pane" class:collapsed=move || panel.collapsed.get()>
                <header on:click=move |_| panel.collapsed.update(|c| *c = !*c)>
                    <span class="dl-head-icon">{move || if busy() > 0 { view! { <span class="spinner small"></span> }.into_any() } else { view! { {icon(icons::CHECK)} }.into_any() }}</span>
                    <strong>{move || {
                        let n = busy();
                        if n == 0 {
                            "Downloads".to_string()
                        } else if n == 1 {
                            "Downloading 1 item".to_string()
                        } else {
                            format!("Downloading {n} items")
                        }
                    }}</strong>
                    <button class="icon-button" aria-label="Collapse">{move || if panel.collapsed.get() { "▴" } else { "▾" }}</button>
                    <button class="icon-button" title="Refresh" aria-label="Refresh" on:click=move |ev| { ev.stop_propagation(); panel.refresh(); }>"⟳"</button>
                    <Show when=move || any_finished()>
                        <button class="icon-button clear-finished" title="Clear finished downloads" aria-label="Clear finished downloads"
                            on:click=move |ev| { ev.stop_propagation(); clear_finished(()); }>"Clear"</button>
                    </Show>
                    <button class="icon-button" aria-label="Close" on:click=move |ev| { ev.stop_propagation(); panel.open.set(false) }>"×"</button>
                </header>
                <Show when=move || !panel.collapsed.get()>
                    {move || (!panel.list.with(Vec::is_empty)).then(|| view! {
                    <ul>
                        <For each=shown key=|d| d.id.clone() let:d>
                            {
                                let done = d.state == "imported" || (!active(&d) && d.state != "failed");
                                let failed = d.state == "failed";
                                let pct = (d.progress * 100.0).clamp(0.0, 100.0);
                                view! {
                                    <li class:done=done class:failed=failed>
                                        <div class="dl-icon">{icon(if d.kind == AssetKind::Ebook { icons::BOOK } else { icons::HEADPHONES })}</div>
                                        <div class="dl-body">
                                            <div class="dl-name" title=d.name.clone()>{d.name.clone()}</div>
                                            <div class="progress"><div style=format!("width:{pct:.1}%")></div></div>
                                            <div class="dl-meta">
                                                <span>{if failed { "Failed".to_string() } else if done { "In your library".to_string() } else { format!("{pct:.0}%") }}</span>
                                                {d.size.map(|s| view! { <span>{size_label(s)}</span> })}
                                                <span>{d.source.clone()}</span>
                                            </div>
                                            {(failed && d.error.is_some()).then(|| view! {
                                                <p class="dl-error">{d.error.clone().unwrap_or_default()}</p>
                                            })}
                                        </div>
                                        {done.then(|| view! {
                                            <button class="dl-open link" on:click={
                                                let name = d.name.clone();
                                                let kind = d.kind;
                                                move |_| {
                                                    let name = name.clone();
                                                    spawn_local(async move {
                                                        let lib = api::get::<Vec<WorkSummary>>("/api/library").await.unwrap_or_default();
                                                        let key = duet_common::text::title_key(&name);
                                                        let hit = lib.iter().find(|w| duet_common::text::title_key(&w.title) == key);
                                                        match hit {
                                                            Some(w) if kind == AssetKind::Ebook && w.has_ebook => {
                                                                let _ = web_sys::window().map(|win| win.location().set_hash(&format!("/read/{}", w.id)));
                                                            }
                                                            Some(w) => {
                                                                let _ = web_sys::window().map(|win| win.location().set_hash(&format!("/work/{}", w.id)));
                                                            }
                                                            None => toast.show("Not in the library yet — press refresh, then Open again."),
                                                        }
                                                    });
                                                }
                                            }>"Open"</button>
                                        })}
                                        {(done || failed).then(|| view! {
                                            <button class="icon-button dl-dismiss" title="Remove from downloads" aria-label="Remove from downloads"
                                                on:click={
                                                    let id = d.id.clone();
                                                    move |_| dismiss(id.clone())
                                                }>"×"</button>
                                        })}
                                    </li>
                                }
                            }
                        </For>
                    </ul>
                    }).into_any()}
                    {move || panel.list.with(Vec::is_empty).then(|| view! {
                        <p class="muted dl-empty">"All done — new downloads appear here."</p>
                    })}
                </Show>
            </aside>
        </Show>
    }
}

/// Line icons (24×24, stroked), drawn for duet.
pub mod icons {
    pub const LIBRARY: &str = "M4 19V5a1 1 0 0 1 1-1h3v16H5a1 1 0 0 1-1-1zM8 4h3v16H8M13.5 5.2l3-.8 3.6 14.4-3 .8z";
    pub const DISCOVER: &str = "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20.5 20.5 16 16";
    pub const JOBS: &str = "M12 3l1.8 4.6L18.5 9l-4.7 1.6L12 15l-1.8-4.4L5.5 9l4.7-1.4zM18.5 15l.8 2 2 .7-2 .8-.8 2-.8-2-2-.8 2-.7z";
    pub const SETTINGS: &str = "M4 7h10M18 7h2M4 17h4M12 17h8M14 4.5v5M8 14.5v5";
    pub const BOOK: &str = "M4 5.5C6.5 4 9.5 4 12 6c2.5-2 5.5-2 8-.5V19c-2.5-1.5-5.5-1.5-8 .5-2.5-2-5.5-2-8-.5z M12 6v13.5";
    pub const HEADPHONES: &str = "M4 15v-3a8 8 0 0 1 16 0v3M4 15a2 2 0 0 1 2-2h1v7H6a2 2 0 0 1-2-2zM20 15a2 2 0 0 0-2-2h-1v7h1a2 2 0 0 0 2-2z";
    pub const SYNC: &str = "M4 12a8 8 0 0 1 13.7-5.6L20 9M20 4v5h-5M20 12a8 8 0 0 1-13.7 5.6L4 15M4 20v-5h5";
    pub const SPARKLE: &str = "M12 3l1.8 4.6L18.5 9l-4.7 1.6L12 15l-1.8-4.4L5.5 9l4.7-1.4z";
    pub const PLAY: &str = "M7 4.5v15l12-7.5z";
    pub const MASKS: &str = "M4 5h7v6a3.5 3.5 0 0 1-7 0zM13 9h7v6a3.5 3.5 0 0 1-7 0zM6 9h.01M9 9h.01M15.5 13h.01M18 13h.01M6.5 12.5c.6.6 1.4.6 2 0M15.5 16.5c.6-.6 1.4-.6 2 0";
    pub const QUILL: &str = "M20 4c-6 0-11 5-12.5 11.5L6 20M20 4c0 6-4 10-9.5 11.5M20 4l-6 6";
    pub const CONVERT: &str = "M5 8h12l-3-3M19 16H7l3 3";
    pub const SCAN: &str = "M4 8V5a1 1 0 0 1 1-1h3M16 4h3a1 1 0 0 1 1 1v3M20 16v3a1 1 0 0 1-1 1h-3M8 20H5a1 1 0 0 1-1-1v-3M7 12h10";
    pub const DOWNLOAD: &str = "M12 4v11M7 10l5 5 5-5M5 20h14";
    pub const CHECK: &str = "M5 12.5l4.5 4.5L19 7.5";
    pub const CLOCK: &str = "M12 4a8 8 0 1 0 0 16 8 8 0 0 0 0-16zM12 8v4.5l3 2";
    pub const ALERT: &str = "M12 4 3 20h18zM12 10v4M12 17h.01";
    pub const PERSON: &str = "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM5 20a7 7 0 0 1 14 0";
    pub const COMIC: &str = "M4 4h16v16H4zM4 11h9M13 4v16";
    pub const RADIO: &str = "M4 9h16v11H4zM8 9l9-5M8 14.5h.01M15 13a2 2 0 1 0 0 4 2 2 0 0 0 0-4z";
}

const TABS: [(&str, &str, &str); 5] = [
    ("library", "Library", icons::LIBRARY),
    ("discover", "Discover", icons::DISCOVER),
    ("briefing", "Briefing", icons::RADIO),
    ("jobs", "Jobs", icons::JOBS),
    ("settings", "Settings", icons::SETTINGS),
];

pub fn icon(path: &'static str) -> impl IntoView {
    view! {
        <svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=path/></svg>
    }
}

/// E-ink display mode (this device only): black-and-white chrome, no animations, pictures still
/// in colour.
#[derive(Clone, Copy)]
pub struct Eink(pub RwSignal<bool>);

impl Eink {
    /// Switches it, remembers it on this device, and says so.
    pub fn set(&self, on: bool) {
        self.0.set(on);
        set_eink(on);
        toast().show(if on { "E-ink view on" } else { "E-ink view off" });
    }
}

pub fn eink() -> Eink {
    expect_context::<Eink>()
}

/// Whether the sidebar is tucked away while reading.
#[derive(Clone, Copy)]
pub struct SidebarHidden(pub RwSignal<bool>);

#[component]
fn App() -> impl IntoView {
    let route = RwSignal::new(Route::parse(&current_hash()));
    let handler = Closure::<dyn Fn()>::new(move || {
        route.set(Route::parse(&current_hash()));
        window().scroll_to_with_x_and_y(0.0, 0.0);
    });
    let _ = window().add_event_listener_with_callback("hashchange", handler.as_ref().unchecked_ref());
    handler.forget();
    let toast_sig = RwSignal::new(None::<String>);
    provide_context(Toast(toast_sig));
    provide_context(DownloadsPanel { open: RwSignal::new(false), list: RwSignal::new(Vec::new()), collapsed: RwSignal::new(false) });
    // The reader takes the whole screen on phones.
    let immersive = move || matches!(route.get(), Route::Read(..));
    // While reading, the sidebar can be tucked away (remembered on this device).
    let sidebar_hidden = RwSignal::new(sidebar_hidden());
    provide_context(SidebarHidden(sidebar_hidden));
    provide_context(Eink(RwSignal::new(eink_on())));
    // Long-lived tabs run stale builds forever; offer a reload when the
    // server reports a different UI fingerprint.
    let update_ready = RwSignal::new(false);
    if let Ok(h) = set_interval_with_handle(
        move || {
            spawn_local(async move {
                #[derive(serde::Deserialize)]
                struct Ver {
                    ui: String,
                }
                static SEEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                if let Ok(v) = api::get::<Ver>("/api/version").await {
                    if let Some(first) = SEEN.get() {
                        if *first != v.ui {
                            update_ready.set(true);
                        }
                    } else {
                        let _ = SEEN.set(v.ui);
                    }
                }
            });
        },
        std::time::Duration::from_secs(60),
    ) {
        on_cleanup(move || h.clear());
    }

    view! {
        <div class="app" class:immersive=immersive class:no-sidebar=move || immersive() && sidebar_hidden.get()>
            <nav class="tabs">
                <a class="brand" href="#/" aria-label="duet"><span class="wordmark" aria-hidden="true"><span class="initial"></span><span class="rest">"uet"</span></span></a>
                {TABS.map(|(id, label, path)| view! {
                    <a href=format!("#/{}", if id == "library" { "" } else { id })
                        class:active=move || route.get().tab() == id>
                        {icon(path)}<span>{label}</span>
                    </a>
                })}
                {{
                    let panel = downloads_panel();
                    view! {
                        <button class="dl-tab" title="Downloads" aria-label="Downloads" on:click=move |_| panel.opened()>
                            {icon(icons::DOWNLOAD)}<span>"Downloads"</span>
                            {move || {
                                let n = panel.list.with(|l| l.iter().filter(|d| active(d)).count());
                                (n > 0).then(|| view! { <span class="dl-badge">{n}</span> })
                            }}
                        </button>
                    }
                }}
                <Show when=immersive>
                    <button class="tabs-hide" title="Hide the sidebar" on:click=move |_| {
                        sidebar_hidden.set(true);
                        set_sidebar_hidden(true);
                    }>
                        <svg viewBox="0 0 24 24" aria-hidden="true" class="icon" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M11.5 7l-5 5 5 5M17.5 7l-5 5 5 5"/></svg>
                        <span>"Hide sidebar"</span>
                    </button>
                </Show>
            </nav>
            // Where the hidden sidebar was: a tab to bring it back.
            <Show when=move || immersive() && sidebar_hidden.get()>
                <button class="sidebar-tab" title="Show the sidebar" aria-label="Show the sidebar" on:click=move |_| {
                    sidebar_hidden.set(false);
                    set_sidebar_hidden(false);
                }>
                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9.5 6l6 6-6 6"/></svg>
                </button>
            </Show>
            <main>
                {move || match route.get() {
                    Route::Library => view! { <Library/> }.into_any(),
                    Route::Work(id) => view! { <WorkPage id/> }.into_any(),
                    Route::Read(id, m) => match m {
                        Some(m) => view! { <reader::Reader id start_mode=m/> }.into_any(),
                        None => view! { <reader::Reader id/> }.into_any(),
                    },
                    Route::Discover => view! { <discover::Discover/> }.into_any(),
                    Route::Briefing => view! { <podcast::Briefing/> }.into_any(),
                    Route::Jobs => view! { <discover::Jobs/> }.into_any(),
                    Route::Settings => view! { <settings::SettingsPage/> }.into_any(),
                    Route::Setup => view! { <setup::SetupPage/> }.into_any(),
                }}
            </main>
            <DownloadsPane/>
            <ActivityPane/>
            <Show when=move || toast_sig.get().is_some()>
                <div class="toast" role="status" on:click=move |_| toast_sig.set(None)>{move || toast_sig.get()}</div>
            </Show>
            <Show when=move || update_ready.get()>
                <div class="update-banner" role="status">
                    <span>"A new duet is ready."</span>
                    <button class="ghost" on:click=move |_| {
                        let _ = web_sys::window().and_then(|w| w.location().reload().ok());
                    }>"Reload"</button>
                </div>
            </Show>
        </div>
    }
}

/// A stable hue for a string, for generated covers and avatars.
pub fn hue_of(s: &str) -> u32 {
    s.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360
}

pub fn cover(url: Option<String>, title: String) -> impl IntoView {
    match url {
        Some(u) => view! { <img class="cover" src=u alt="" loading="lazy" decoding="async"/> }.into_any(),
        None => {
            // A generated cover: the title set like a cloth binding, in a colour picked from it.
            let hue = hue_of(&title);
            view! {
                <div class="cover placeholder" style=format!("--hue:{hue}")>
                    <span class="rule"></span><span class="ptitle">{title}</span><span class="rule"></span>
                </div>
            }
            .into_any()
        }
    }
}

/// The small format marks on a library cover.
/// The small "AI" tag that sits beside anything performed by AI-generated voices.
pub fn ai_tag() -> impl IntoView {
    view! { <span class="ai-tag" title="Performed by AI-generated voices, not a human narrator">{icon(icons::SPARKLE)}"AI"</span> }
}

fn format_marks(w: &WorkSummary) -> impl IntoView + use<> {
    let w = w.clone();
    view! {
        <div class="marks">
            {w.has_sync.then(|| view! { <span class="mark sync" title="Text and audio in sync">{icon(icons::SYNC)}</span> })}
        </div>
    }
}

fn book_card(w: WorkSummary) -> impl IntoView {
    view! {
        <a class="card" href=format!("#/work/{}", w.id)>
            <div class="cover-wrap">
                {cover(w.cover_url.clone(), w.title.clone())}
                <span class="hinge" aria-hidden="true"></span>
                {format_marks(&w)}
                {w.progress.filter(|p| *p > 0.0).map(|p| view! {
                    <div class="cover-progress"><div style=format!("width:{:.1}%", p * 100.0)></div></div>
                })}
            </div>
            <div class="info">
                <div class="title">{w.title.clone()}</div>
                <div class="author">{byline(&w)}</div>
                <div class="format-badges">
                    {w.has_ebook.then(|| view! { <span class="fbadge ebook">{icon(icons::BOOK)}"Ebook"</span> })}
                    {w.has_narration.then(|| view! { <span class="fbadge audio">{icon(icons::HEADPHONES)}"Audiobook"</span> })}
                    {w.has_cast.then(|| view! { <span class="fbadge ai" title="Performed by AI-generated voices">{icon(icons::SPARKLE)}"AI full cast"</span> })}
                </div>
            </div>
        </a>
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Shelf {
    All,
    Ebooks,
    Audiobooks,
    Synced,
}

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    Shelf,
    Grid,
    List,
}

/// One row in list layout: cover thumbnail, title, byline, badges, progress.
fn book_row(w: WorkSummary) -> impl IntoView {
    let pct = w.progress.unwrap_or(0.0) * 100.0;
    view! {
        <a class="book-row" href=format!("#/work/{}", w.id)>
            <div class="row-cover">{cover(w.cover_url.clone(), w.title.clone())}</div>
            <div class="row-main">
                <div class="title">{w.title.clone()}</div>
                <div class="author">{byline(&w)}</div>
                <div class="format-badges">
                    {w.has_ebook.then(|| view! { <span class="fbadge ebook">{icon(icons::BOOK)}"Ebook"</span> })}
                    {w.has_narration.then(|| view! { <span class="fbadge audio">{icon(icons::HEADPHONES)}"Audiobook"</span> })}
                    {w.has_cast.then(|| view! { <span class="fbadge ai" title="Performed by AI-generated voices">{icon(icons::SPARKLE)}"AI full cast"</span> })}
                </div>
            </div>
            {w.progress.filter(|p| *p > 0.0).map(|_| view! {
                <div class="row-progress">
                    <div class="progress"><div style=format!("width:{pct:.1}%")></div></div>
                    <span class="pct">{format!("{pct:.0}%")}</span>
                </div>
            })}
        </a>
    }
}

/// "Author · 2011", or whichever half exists.
fn byline(w: &WorkSummary) -> String {
    match (&w.author, w.year) {
        (Some(a), Some(y)) => format!("{a} · {y}"),
        (Some(a), None) => a.clone(),
        (None, Some(y)) => y.to_string(),
        (None, None) => String::new(),
    }
}

#[component]
fn Library() -> impl IntoView {
    let works = LocalResource::new(|| api::get::<Vec<WorkSummary>>("/api/library"));
    // First run: the setup guide, until it's finished or skipped.
    let setup_done = RwSignal::new(true);
    spawn_local(async move {
        if let Ok(s) = api::get::<duet_common::Settings>("/api/settings").await {
            if !s.setup_done && s.library_dirs.is_empty() {
                go("/setup");
            }
            setup_done.set(s.setup_done);
        }
    });
    let filter = RwSignal::new(String::new());
    let shelf = RwSignal::new(Shelf::All);
    let layout = RwSignal::new(Layout::Shelf);
    let scanning = RwSignal::new(false);
    let toast = toast();
    let scan = move |_| {
        scanning.set(true);
        spawn_local(async move {
            match api::post::<_, ScanReport>("/api/library/scan", &()).await {
                Ok(r) => {
                    let errors = if r.errors.is_empty() { String::new() } else { format!(", {} couldn't be read", r.errors.len()) };
                    toast.show(format!("{} books: {} ebooks and {} audiobooks added{errors}", r.works, r.ebooks, r.audiobooks));
                }
                Err(e) => toast.show(e.0),
            }
            scanning.set(false);
            works.refetch();
        });
    };
    let all = move || works.get().and_then(|r| r.ok()).unwrap_or_default();
    let shown = move || {
        let f = filter.get().to_lowercase();
        let sh = shelf.get();
        all()
            .into_iter()
            .filter(|w| match sh {
                Shelf::All => true,
                Shelf::Ebooks => w.has_ebook,
                Shelf::Audiobooks => w.has_audiobook,
                Shelf::Synced => w.has_sync,
            })
            .filter(|w| {
                f.is_empty()
                    || w.title.to_lowercase().contains(&f)
                    || w.author.as_deref().unwrap_or("").to_lowercase().contains(&f)
                    || w.series.as_deref().unwrap_or("").to_lowercase().contains(&f)
            })
            .collect::<Vec<_>>()
    };
    // Books in progress, most recent first (the server already orders by last read).
    let reading = move || all().into_iter().filter(|w| w.progress.is_some_and(|p| p > 0.0 && p < 0.995)).take(8).collect::<Vec<_>>();

    view! {
        <header class="page-head">
            <div class="titles">
                <h1>"Library"</h1>
                <p class="subtitle">{move || match all().len() {
                    0 => String::new(),
                    1 => "1 book".into(),
                    n => format!("{n} books"),
                }}</p>
            </div>
            <div class="search-box">
                {icon(icons::DISCOVER)}
                <input type="search" placeholder="Title, author or series" aria-label="Filter the library"
                    prop:value=move || filter.get() on:input=move |ev| filter.set(event_target_value(&ev))/>
            </div>
            <button class="ghost" on:click=scan disabled=move || scanning.get() title="Look for new books in your folders">
                {icon(icons::SCAN)}{move || if scanning.get() { "Scanning…" } else { "Scan" }}
            </button>
        </header>
        <Show when=move || !setup_done.get()>
            <div class="banner">
                <span>"duet isn't fully set up yet."</span>
                <a class="button primary" href="#/setup">"Continue setup"</a>
            </div>
        </Show>
        <Suspense fallback=|| view! {
            <div class="grid">{(0..10).map(|_| view! { <div class="card skeleton"><div class="cover"></div><div class="line"></div><div class="line short"></div></div> }).collect_view()}</div>
        }>
            {move || {
                if works.get().is_some() && all().is_empty() {
                    return view! {
                        <div class="empty-state">
                            <div class="empty-icon">{icon(icons::LIBRARY)}</div>
                            <h2>"Your library is empty"</h2>
                            <p>"Add the folders where your books live, or start with a free classic."</p>
                            <div class="row center">
                                <a class="button primary" href="#/settings">"Add book folders"</a>
                                <a class="button" href="#/discover">"Find free books"</a>
                            </div>
                        </div>
                    }.into_any();
                }
                view! {
                    <Show when=move || !reading().is_empty() && filter.get().is_empty() && shelf.get() == Shelf::All>
                        <section class="shelf">
                            <h2>"Continue"</h2>
                            <div class="continue-row">
                                {move || reading().into_iter().map(|w| {
                                    let pct = (w.progress.unwrap_or(0.0) * 100.0).round();
                                    view! {
                                        <a class="continue-card" href=format!("#/read/{}", w.id)>
                                            {cover(w.cover_url.clone(), w.title.clone())}
                                            <div class="continue-info">
                                                <div class="title">{w.title.clone()}</div>
                                                <div class="author">{w.author.clone().unwrap_or_default()}</div>
                                                <div class="progress"><div style=format!("width:{pct}%")></div></div>
                                                <span class="pct">{format!("{pct}%")}</span>
                                                <span class="resume">{icon(icons::PLAY)}"Resume"</span>
                                            </div>
                                        </a>
                                    }
                                }).collect_view()}
                            </div>
                        </section>
                    </Show>
                    <div class="shelf-head">
                        <h2>"All books"</h2>
                        <div class="chips" role="tablist" aria-label="Layout">
                            {[(Layout::Shelf, "Shelf"), (Layout::Grid, "Grid"), (Layout::List, "List")].map(|(k, label)| view! {
                                <button class="chip-button" class:active=move || layout.get() == k on:click=move |_| layout.set(k)>{label}</button>
                            })}
                        </div>
                        <div class="chips" role="tablist" aria-label="Filter">
                            {[(Shelf::All, "All"), (Shelf::Ebooks, "Ebooks"), (Shelf::Audiobooks, "Audiobooks"), (Shelf::Synced, "Synced")].map(|(k, label)| view! {
                                <button class="chip-button" class:active=move || shelf.get() == k on:click=move |_| shelf.set(k)>{label}</button>
                            })}
                        </div>
                    </div>
                    {move || shown().is_empty().then(|| view! { <p class="empty">"No books match."</p> })}
                    {move || match layout.get() {
                        // A wooden bookcase: the frame (sides and top), and inside it the shelves.
                        Layout::Shelf => view! {
                            <div class="bookcase-frame">
                                <div class="grid bookcase">
                                    <For each=shown key=|w| (w.id, w.progress.map(|p| (p * 1000.0) as i64)) let:w>
                                        {book_card(w)}
                                    </For>
                                </div>
                            </div>
                        }.into_any(),
                        Layout::Grid => view! {
                            <div class="grid plain">
                                <For each=shown key=|w| (w.id, w.progress.map(|p| (p * 1000.0) as i64)) let:w>
                                    {book_card(w)}
                                </For>
                            </div>
                        }.into_any(),
                        Layout::List => view! {
                            <div class="book-list">
                                <For each=shown key=|w| (w.id, w.progress.map(|p| (p * 1000.0) as i64)) let:w>
                                    {book_row(w)}
                                </For>
                            </div>
                        }.into_any(),
                    }}
                }.into_any()
            }}
        </Suspense>
    }
}

fn size_label(bytes: i64) -> String {
    let b = bytes as f64;
    if b > 1e9 { format!("{:.1} GB", b / 1e9) } else if b > 1e6 { format!("{:.0} MB", b / 1e6) } else { format!("{:.0} KB", b / 1e3) }
}

fn asset_label(a: &Asset) -> String {
    let mut parts = vec![a.format.to_uppercase()];
    if let Some(d) = a.duration_secs {
        parts.push(format_time(d));
    }
    if a.tracks.len() > 1 {
        parts.push(format!("{} tracks", a.tracks.len()));
    }
    parts.push(size_label(a.size));
    parts.join(" · ")
}

pub async fn start_job(kind: JobKind, work_id: i64) {
    match api::post::<_, i64>("/api/jobs", &NewJob { kind, work_id, ebook_asset: None, audiobook_asset: None }).await {
        Ok(_) => toast().show(format!("{} started; follow it in Jobs", kind.label())),
        Err(e) => toast().show(e.0),
    }
}

/// One of the AI things duet can make from a book, as a card with a button.
fn action_card(
    path: &'static str,
    title: &'static str,
    about: &'static str,
    button: &'static str,
    busy: bool,
    run: impl Fn() + 'static,
) -> impl IntoView {
    view! {
        <div class="action-card">
            <div class="action-icon">{icon(path)}</div>
            <div class="action-text"><strong>{title}</strong><p>{about}</p></div>
            <button disabled=busy on:click=move |_| run()>{if busy { "Working…" } else { button }}</button>
        </div>
    }
}

#[component]
fn WorkPage(id: i64) -> impl IntoView {
    let work = LocalResource::new(move || async move { api::get::<WorkDetail>(&format!("/api/works/{id}")).await });
    let jobs = LocalResource::new(|| api::get::<Vec<Job>>("/api/jobs"));
    let more = RwSignal::new(false);
    // Cover picker: alternatives from the book's own files and Open Library.
    let picking = RwSignal::new(false);
    let cover_opts = RwSignal::new(None::<Vec<CoverOption>>);
    let online_opts = RwSignal::new(None::<Vec<OnlineCover>>);
    // What's being saved right now ("Saving cover…"), shown over the picker.
    let saving = RwSignal::new(None::<&'static str>);
    let toast = toast();
    // Sets a cover through `url` and reports how it went. The work's cover address
    // changes with each new cover, so refetching shows it at once.
    let save_cover = move |url: String, body: serde_json::Value, what: &'static str| {
        if saving.get_untracked().is_some() {
            return;
        }
        saving.set(Some(what));
        spawn_local(async move {
            let result = api::post::<_, ()>(&url, &body).await;
            saving.set(None);
            match result {
                Ok(()) => {
                    picking.set(false);
                    toast.show("Cover saved");
                    work.refetch();
                }
                Err(e) => toast.show(format!("Couldn't change the cover: {}", e.0)),
            }
        });
    };
    view! {
        <Suspense fallback=|| view! { <div class="work-skeleton skeleton"><div class="cover"></div><div class="lines"><div class="line"></div><div class="line short"></div></div></div> }>
            {move || work.get().map(|r| match r {
                Err(e) => view! { <p class="empty">{e.0}</p> }.into_any(),
                Ok(w) => {
                    let ebooks: Vec<Asset> = w.assets.iter().filter(|a| a.kind == AssetKind::Ebook).cloned().collect();
                    let audio: Vec<Asset> = w.assets.iter().filter(|a| a.kind == AssetKind::Audiobook).cloned().collect();
                    let readable = ebooks.iter().any(|a| ["epub", "txt", "md", "html", "htm", "xhtml"].contains(&a.format.as_str()));
                    let running: Vec<Job> = jobs.get().and_then(|r| r.ok()).unwrap_or_default().into_iter()
                        .filter(|j| j.work_id == id && (j.status == "queued" || j.status == "running")).collect();
                    let busy = move |k: JobKind| running.iter().any(|j| j.kind == k);
                    let (has_e, has_a) = (!ebooks.is_empty(), !audio.is_empty());
                    let cast_asset = reader::Mode::available(&w.assets).iter().any(|m| m.cast())
                        .then(|| w.default_ebook.filter(|d| ebooks.iter().any(|a| a.id == *d)).or_else(|| reader::pick_ebook(&w.assets).map(|a| a.id)))
                        .flatten();
                    let action = if has_e && has_a { "Read & listen" } else if has_a { "Listen" } else { "Read" };
                    let progress = w.position.as_ref().map(|p| p.fraction).filter(|f| *f > 0.0);
                    let cover_url = w.cover_url.clone();
                    let backdrop = w.cover_url.clone().map(|u| format!("background-image:url('{u}')"));
                    let long_description = w.description.as_ref().is_some_and(|d| d.len() > 420);
                    let opening = RwSignal::new(reader::opening_editions(&w));
                    view! {
                        <div class="work">
                            <div class="work-backdrop" style=backdrop.unwrap_or_default()></div>
                            <div class="work-hero">
                                <button class="work-cover-pick" title="Change cover" aria-label="Change cover"
                                    on:click=move |_| {
                                        picking.set(true);
                                        cover_opts.set(None);
                                        online_opts.set(None);
                                        // Both lists load side by side; each shows its own wait.
                                        spawn_local(async move {
                                            let opts = api::get::<Vec<CoverOption>>(&format!("/api/works/{id}/cover_options")).await;
                                            cover_opts.set(Some(opts.unwrap_or_default()));
                                        });
                                        spawn_local(async move {
                                            match api::get::<Vec<OnlineCover>>(&format!("/api/works/{id}/cover_search")).await {
                                                Ok(found) => online_opts.set(Some(found)),
                                                Err(e) => {
                                                    online_opts.set(Some(Vec::new()));
                                                    toast.show(format!("Couldn't search Open Library: {}", e.0));
                                                }
                                            }
                                        });
                                    }>
                                    <div class="work-cover">{cover(cover_url.clone(), w.title.clone())}</div>
                                    <span class="cover-edit">{"Change"}</span>
                                </button>
                                <div class="work-info">
                                    {w.series.clone().map(|s| view! {
                                        <p class="series">{s}{w.series_index.map(|i| format!(" · Book {i}")).unwrap_or_default()}</p>
                                    })}
                                    <h1>{w.title.clone()}</h1>
                                    <p class="author">{w.author.clone().unwrap_or_default()}{w.year.map(|y| format!(" · {y}")).unwrap_or_default()}{w.pages.map(|p| format!(" · {p} pages")).unwrap_or_default()}</p>
                                    {(!w.subjects.is_empty()).then(|| view! {
                                        <div class="subjects">{w.subjects.iter().map(|s| view! { <span class="subject">{s.clone()}</span> }).collect_view()}</div>
                                    })}
                                    <div class="pills">
                                        {has_e.then(|| view! { <span class="pill">{icon(icons::BOOK)}"Ebook"</span> })}
                                        {has_a.then(|| view! { <span class="pill">{icon(icons::HEADPHONES)}"Audiobook"</span> })}
                                    </div>
                                    <div class="actions">
                                        <a class="button primary big" href=format!("#/read/{id}")>
                                            {icon(icons::PLAY)}{if progress.is_some() { "Resume".to_string() } else { action.to_string() }}
                                        </a>
                                        {progress.map(|p| view! { <span class="muted">{format!("{:.0}% through", p * 100.0)}</span> })}
                                    </div>
                                    {w.description.clone().map(|d| view! {
                                        <p class="description" class:clamped=move || long_description && !more.get()>{d}</p>
                                        {long_description.then(|| view! {
                                            <button class="link" on:click=move |_| more.update(|m| *m = !*m)>{move || if more.get() { "Less" } else { "More" }}</button>
                                        })}
                                    })}
                                </div>
                            </div>

                            <section class="work-section">
                                <h2>"Editions"</h2>
                                <div class="edition-cards">
                                    {w.assets.iter().map(|a| {
                                        let ebook = a.kind == AssetKind::Ebook;
                                        let name = reader::edition_label(a);
                                        let (aid, kind) = (a.id, a.kind);
                                        // With more than one of a kind, tapping one makes it the one the book opens with.
                                        let choosable = w.assets.iter().filter(|b| b.kind == kind).count() > 1;
                                        let is_default = move || {
                                            let (e, au) = opening.get();
                                            choosable && (if ebook { e } else { au }) == Some(aid)
                                        };
                                        let choose = move |_| {
                                            if !choosable || is_default() {
                                                return;
                                            }
                                            opening.update(|(e, au)| if ebook { *e = Some(aid) } else { *au = Some(aid) });
                                            reader::remember(id, aid);
                                            toast.show(if ebook { "This edition opens from now on" } else { "This recording plays from now on" });
                                        };
                                        view! {
                                            <div class="edition-card" class:generated=a.origin == "generated" class:choosable=choosable class:default=is_default
                                                title=if choosable { format!("{}\nTap to open the book with this edition", a.path) } else { a.path.clone() }
                                                role=choosable.then_some("button") tabindex=choosable.then_some("0")
                                                on:click=choose>
                                                <div class="action-icon">{icon(if ebook { icons::BOOK } else { icons::HEADPHONES })}</div>
                                                <div>
                                                    <strong>{name}</strong>
                                                    <p>{asset_label(a)}</p>
                                                    // Editions of one kind otherwise look alike: name the file.
                                                    {choosable.then(|| {
                                                        let file = a.path.trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string();
                                                        view! { <p class="file">{file}</p> }
                                                    })}
                                                </div>
                                                {(a.origin == "generated").then(|| view! { <span class="pill ai">{icon(icons::SPARKLE)}"AI"</span> })}
                                                <Show when=is_default>
                                                    <span class="pill default">{icon(icons::CHECK)}"Default"</span>
                                                </Show>
                                            </div>
                                        }
                                    }).collect_view()}
                                    <editions::FindEdition title=w.title.clone()/>
                                </div>
                            </section>

                            <section class="work-section">
                                <h2>"Read or listen"</h2>
                                <div class="mode-cards">
                                    {reader::Mode::available(&w.assets).into_iter().map(|m| view! {
                                        <a class="mode-card" href=format!("#/read/{id}/{}", m.key())>
                                            <span class="mode-icon" aria-hidden="true">{m.icons().iter().map(|i| crate::icon(i)).collect_view()}</span>
                                            <span class="mode-text">
                                                <strong>{m.label()}{m.cast().then(ai_tag)}</strong>
                                                <small>{m.detail()}</small>
                                            </span>
                                        </a>
                                    }).collect_view()}
                                </div>
                                // The full cast got ready ahead of time, from where you left off: directed,
                                // voiced and its first lines performed, so it plays at once when you open it.
                                {cast_asset.map(|e| {
                                    let preparing = RwSignal::new(false);
                                    view! {
                                        <div class="action-cards">
                                            <div class="action-card">
                                                <div class="action-icon">{icon(icons::SPARKLE)}</div>
                                                <div class="action-text">
                                                    <strong>"Get the full cast ready" {ai_tag()}</strong>
                                                    <p>"Directs the book, makes everyone's AI voices and performs the first lines from where you left off, in the background, so the full cast plays straight away. Progress shows in the jobs pane."</p>
                                                </div>
                                                <button disabled=move || preparing.get() on:click=move |_| {
                                                    preparing.set(true);
                                                    spawn_local(async move {
                                                        match api::post::<_, serde_json::Value>(&format!("/api/assets/{e}/cast/prepare"), &serde_json::json!({})).await {
                                                            Ok(_) => toast.show("Getting the full cast ready"),
                                                            Err(err) => {
                                                                toast.show(err.0);
                                                                preparing.set(false);
                                                            }
                                                        }
                                                    });
                                                }>{move || if preparing.get() { "Started" } else { "Get ready" }}</button>
                                            </div>
                                        </div>
                                    }
                                })}
                            </section>

                            {(has_e && !readable && !ebooks.iter().any(|a| ["pdf", "cbz", "cbr", "cb7", "cbt"].contains(&a.format.as_str()))).then(|| view! {
                                <section class="work-section">
                                    <h2>"Make it readable"</h2>
                                    <div class="action-cards">
                                        {action_card(
                                            icons::CONVERT,
                                            "Convert to EPUB",
                                            "Make a readable copy of this format with Calibre.",
                                            "Convert",
                                            busy(JobKind::Convert),
                                            move || spawn_local(start_job(JobKind::Convert, id)),
                                        )}
                                    </div>
                                </section>
                            })}

                            <section class="work-section">
                                <CombineWith id title=w.title.clone()/>
                            </section>

                            {(!w.cast.is_empty()).then(|| view! {
                                <section class="work-section">
                                    <h2>"Cast" {ai_tag()}</h2>
                                    <p class="hint">"Their voices in the full cast are AI-generated, made from these descriptions. Talk to any of them from the reader's Ask button."</p>
                                    <div class="cast-cards">
                                        {w.cast.iter().map(|c| view! {
                                            <div class="cast-card">
                                                <div class="avatar" style=format!("--hue:{}", hue_of(&c.name))>
                                                    {c.name.chars().next().map(|ch| ch.to_uppercase().to_string()).unwrap_or_default()}
                                                </div>
                                                <div>
                                                    <strong>{if c.is_narrator { "Narrator".to_string() } else { c.name.clone() }}</strong>
                                                    <p>{c.description.clone()}</p>
                                                </div>
                                            </div>
                                        }).collect_view()}
                                    </div>
                                </section>
                            })}
                            <Show when=move || picking.get()>
                                // In <body>, like the edition finder: the transformed page would
                                // otherwise pin this panel to the page instead of the screen.
                                <leptos::portal::Portal>
                                <div class="check-modal-backdrop" on:click=move |_| if saving.get_untracked().is_none() { picking.set(false) }>
                                    <div class="check-modal cover-picker" class:saving=move || saving.get().is_some() on:click=|ev| ev.stop_propagation()>
                                        <h2>"Pick a cover"</h2>
                                        <p class="muted">"Pictures embedded in this book's ebook, and covers from Open Library. Click one to make it the cover."</p>
                                        {move || match cover_opts.get() {
                                            None => view! { <p class="cover-wait"><span class="spinner small"></span>"Looking through the ebook's pictures…"</p> }.into_any(),
                                            Some(opts) if opts.is_empty() => view! { <p class="muted">"No embedded pictures to choose from in this book's ebook."</p> }.into_any(),
                                            Some(opts) => view! {
                                                <h3 class="cover-subhead">"From the ebook"</h3>
                                                <div class="cover-grid">
                                                    {opts.into_iter().map(|o| {
                                                        let idx = o.index;
                                                        view! {
                                                            <button class="cover-choice" title=format!("Use image {}", idx + 1) disabled=move || saving.get().is_some()
                                                                on:click=move |_| save_cover(format!("/api/works/{id}/cover_pick"), serde_json::json!({ "index": idx }), "Saving cover…")>
                                                                <img src=o.url alt="" loading="lazy" decoding="async"/>
                                                            </button>
                                                        }
                                                    }).collect_view()}
                                                </div>
                                            }.into_any(),
                                        }}
                                        {move || match online_opts.get() {
                                            None => view! { <p class="cover-wait"><span class="spinner small"></span>"Searching Open Library…"</p> }.into_any(),
                                            Some(found) if found.is_empty() => view! { <p class="muted">"Open Library has no covers for this book."</p> }.into_any(),
                                            Some(found) => view! {
                                                <h3 class="cover-subhead">"From Open Library"</h3>
                                                <div class="cover-grid">
                                                    {found.into_iter().map(|o| {
                                                        let url = o.url;
                                                        let label = o.label.clone();
                                                        view! {
                                                            <button class="cover-choice" title=label disabled=move || saving.get().is_some()
                                                                on:click=move |_| save_cover(format!("/api/works/{id}/cover_fetch"), serde_json::json!({ "url": url.clone() }), "Downloading cover…")>
                                                                <img src=o.thumb alt=o.label loading="lazy" decoding="async"/>
                                                            </button>
                                                        }
                                                    }).collect_view()}
                                                </div>
                                            }.into_any(),
                                        }}
                                        <button class="ghost" disabled=move || saving.get().is_some() on:click=move |_| picking.set(false)>"Close"</button>
                                        {move || saving.get().map(|what| view! {
                                            <div class="cover-saving"><span class="spinner big"></span><p>{what}</p></div>
                                        })}
                                    </div>
                                </div>
                                </leptos::portal::Portal>
                            </Show>
                        </div>
                    }.into_any()
                }
            })}
        </Suspense>
    }
}

/// "This is the same book as…": folds another library entry (say, the audiobook that was
/// titled differently) into this one.
#[component]
fn CombineWith(id: i64, title: String) -> impl IntoView {
    let open = RwSignal::new(false);
    let works = LocalResource::new(|| api::get::<Vec<WorkSummary>>("/api/library"));
    let choice = RwSignal::new(None::<i64>);
    let toast = toast();
    let words = |t: &str| duet_common::text::normalize(t).split(' ').filter(|w| w.len() > 2).map(str::to_string).collect::<Vec<_>>();
    let mine = StoredValue::new(words(&title));
    // Other books, most similar title first.
    let others = move || {
        let mut list: Vec<(usize, WorkSummary)> = works
            .get()
            .and_then(|r| r.ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|w| w.id != id)
            .map(|w| (mine.with_value(|m| words(&w.title).iter().filter(|x| m.contains(x)).count()), w))
            .collect();
        list.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.title.cmp(&b.1.title)));
        list.into_iter().map(|(_, w)| w).collect::<Vec<_>>()
    };
    let combine = move |_| {
        let Some(other) = choice.get_untracked() else { return };
        spawn_local(async move {
            match api::post::<_, WorkDetail>(&format!("/api/works/{other}/merge"), &serde_json::json!({ "into": id })).await {
                Ok(_) => {
                    toast.show("Combined into one book");
                    // Reload this page with the editions it gained.
                    let _ = window().location().reload();
                }
                Err(e) => toast.show(e.0),
            }
        });
    };
    view! {
        <Show when=move || open.get() fallback=move || view! {
            <button class="link" on:click=move |_| open.set(true)>"Same book as another entry in your library? Combine them"</button>
        }>
            <div class="combine">
                <h2>"Combine with another entry"</h2>
                <p class="hint">"Its editions, your place and its sync move into this book."</p>
                <div class="row">
                    <select on:change=move |ev| choice.set(event_target_value(&ev).parse().ok())>
                        <option value="">"Choose a book"</option>
                        {move || others().into_iter().map(|w| view! {
                            <option value=w.id.to_string()>{format!("{}{}", w.title, w.author.map(|a| format!(" — {a}")).unwrap_or_default())}</option>
                        }).collect_view()}
                    </select>
                    <button class="primary" disabled=move || choice.get().is_none() on:click=combine>"Combine"</button>
                    <button on:click=move |_| open.set(false)>"Cancel"</button>
                </div>
            </div>
        </Show>
    }
}
