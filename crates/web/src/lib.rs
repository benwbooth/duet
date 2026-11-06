//! The duet web UI: a Leptos single-page app for the library, the synced reader/player, search
//! and downloads, background jobs and settings. Layouts adapt to desktop and phone widths.

mod api;
mod discover;
mod reader;
mod settings;
mod setup;

use duet_common::{Asset, AssetKind, Job, JobKind, NewJob, ScanReport, WorkDetail, WorkSummary};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/helpers.js")]
extern "C" {
    #[wasm_bindgen(js_name = micSupported)]
    fn mic_supported() -> bool;
    #[wasm_bindgen(js_name = startRecording, catch)]
    async fn start_recording(on_partial: &js_sys::Function) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = stopRecording, catch)]
    async fn stop_recording() -> Result<JsValue, JsValue>;
    fn highlight(container: &web_sys::Element, n: i32, scroll: bool);
    #[wasm_bindgen(js_name = visibleSentence)]
    fn visible_sentence(container: &web_sys::Element) -> u32;
    #[wasm_bindgen(js_name = scrollToSentence)]
    fn scroll_to_sentence(container: &web_sys::Element, n: u32);
    #[wasm_bindgen(js_name = clickTarget)]
    fn click_target(ev: &web_sys::MouseEvent) -> JsValue;
    #[wasm_bindgen(js_name = selectedText)]
    fn selected_text() -> String;
    #[wasm_bindgen(js_name = playDataUrl)]
    fn play_data_url(url: &str);
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
    #[wasm_bindgen(js_name = isFullscreen)]
    fn is_fullscreen() -> bool;
    #[wasm_bindgen(js_name = toggleFullscreen)]
    async fn toggle_fullscreen() -> JsValue;
    #[wasm_bindgen(js_name = pagerSetCurl)]
    fn pager_set_curl(book: &web_sys::Element, on: bool);
    #[wasm_bindgen(js_name = pagerSetPlace)]
    fn pager_set_place(book: &web_sys::Element, chapter: u32, chapters: u32);
    #[wasm_bindgen(js_name = bookCover)]
    fn book_cover(book: &web_sys::Element, title: &str);
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
                list.set(v);
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

#[component]
fn DownloadsPane() -> impl IntoView {
    let panel = downloads_panel();
    // Follow progress while the panel is open; look in now and then for torrents started elsewhere.
    let tick = RwSignal::new(0u32);
    if let Ok(h) = set_interval_with_handle(
        move || {
            tick.update(|t| *t += 1);
            if panel.open.get_untracked() || tick.get_untracked() % 15 == 0 {
                panel.refresh();
            }
        },
        std::time::Duration::from_secs(2),
    ) {
        on_cleanup(move || h.clear());
    }
    panel.refresh();
    // Open by itself when something is downloading.
    Effect::new(move |_| {
        if panel.list.with(|l| l.iter().any(active)) {
            panel.open.set(true);
        }
    });
    // Newest first; finished ones from earlier sessions aren't news.
    let shown = move || panel.list.get().into_iter().take(12).collect::<Vec<_>>();
    let busy = move || panel.list.with(|l| l.iter().filter(|d| active(d)).count());
    view! {
        <Show when=move || panel.open.get() && !panel.list.with(Vec::is_empty)>
            <aside class="downloads-pane" class:collapsed=move || panel.collapsed.get()>
                <header on:click=move |_| panel.collapsed.update(|c| *c = !*c)>
                    {move || if busy() > 0 { view! { <span class="spinner small"></span> }.into_any() } else { view! { {icon(icons::CHECK)} }.into_any() }}
                    <strong>{move || match busy() {
                        0 => "Downloads finished".to_string(),
                        1 => "Downloading 1 item".to_string(),
                        n => format!("Downloading {n} items"),
                    }}</strong>
                    <button class="icon-button" aria-label="Collapse">{move || if panel.collapsed.get() { "▴" } else { "▾" }}</button>
                    <button class="icon-button" aria-label="Close" on:click=move |ev| { ev.stop_propagation(); panel.open.set(false) }>"×"</button>
                </header>
                <Show when=move || !panel.collapsed.get()>
                    <ul>
                        <For each=shown key=|d| (d.id.clone(), (d.progress * 100.0) as i64, d.state.clone()) let:d>
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
                                        </div>
                                        {done.then(|| view! { <a class="dl-open" href="#/">"Open"</a> })}
                                    </li>
                                }
                            }
                        </For>
                    </ul>
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
}

const TABS: [(&str, &str, &str); 4] = [
    ("library", "Library", icons::LIBRARY),
    ("discover", "Discover", icons::DISCOVER),
    ("jobs", "Jobs", icons::JOBS),
    ("settings", "Settings", icons::SETTINGS),
];

pub fn icon(path: &'static str) -> impl IntoView {
    view! {
        <svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=path/></svg>
    }
}

/// The app's mark: two pages, one with a sound wave.
fn logo() -> impl IntoView {
    view! {
        <svg class="logo" viewBox="0 0 32 32" aria-hidden="true">
            <rect width="32" height="32" rx="8" fill="var(--accent)"/>
            <path d="M7 9.5c3-1.4 6-1 8.5 1v13c-2.5-2-5.5-2.4-8.5-1z" fill="var(--bg)"/>
            <path d="M25 9.5c-3-1.4-6-1-8.5 1v13c2.5-2 5.5-2.4 8.5-1z" fill="var(--bg)" opacity="0.75"/>
            <path d="M19.5 15v3.5M21.5 13.5v6.5M23.3 15.5v2.5" stroke="var(--accent)" stroke-width="1.3" stroke-linecap="round"/>
        </svg>
    }
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

    view! {
        <div class="app" class:immersive=immersive class:no-sidebar=move || immersive() && sidebar_hidden.get()>
            <nav class="tabs">
                <a class="brand" href="#/">{logo()}<span>"duet"</span></a>
                {TABS.map(|(id, label, path)| view! {
                    <a href=format!("#/{}", if id == "library" { "" } else { id })
                        class:active=move || route.get().tab() == id>
                        {icon(path)}<span>{label}</span>
                    </a>
                })}
                <Show when=immersive>
                    <button class="tabs-hide" title="Hide the sidebar" on:click=move |_| {
                        sidebar_hidden.set(true);
                        set_sidebar_hidden(true);
                    }>
                        <svg viewBox="0 0 24 24" aria-hidden="true" class="icon" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><rect x="3.5" y="4.5" width="17" height="15" rx="2.5"/><path d="M9 4.5v15M15.5 10l-2 2 2 2"/></svg>
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
                    Route::Jobs => view! { <discover::Jobs/> }.into_any(),
                    Route::Settings => view! { <settings::SettingsPage/> }.into_any(),
                    Route::Setup => view! { <setup::SetupPage/> }.into_any(),
                }}
            </main>
            <DownloadsPane/>
            <Show when=move || toast_sig.get().is_some()>
                <div class="toast" role="status" on:click=move |_| toast_sig.set(None)>{move || toast_sig.get()}</div>
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
                {format_marks(&w)}
                {w.progress.filter(|p| *p > 0.0).map(|p| view! {
                    <div class="cover-progress"><div style=format!("width:{:.1}%", p * 100.0)></div></div>
                })}
            </div>
            <div class="title">{w.title.clone()}</div>
            <div class="author">{w.author.clone().unwrap_or_default()}</div>
            <div class="format-badges">
                {w.has_ebook.then(|| view! { <span class="fbadge ebook">{icon(icons::BOOK)}"Ebook"</span> })}
                {w.has_audiobook.then(|| view! { <span class="fbadge audio">{icon(icons::HEADPHONES)}"Audiobook"</span> })}
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
                        <div class="chips" role="tablist">
                            {[(Shelf::All, "All"), (Shelf::Ebooks, "Ebooks"), (Shelf::Audiobooks, "Audiobooks"), (Shelf::Synced, "Synced")].map(|(k, label)| view! {
                                <button class="chip-button" class:active=move || shelf.get() == k on:click=move |_| shelf.set(k)>{label}</button>
                            })}
                        </div>
                    </div>
                    {move || shown().is_empty().then(|| view! { <p class="empty">"No books match."</p> })}
                    <div class="grid">
                        <For each=shown key=|w| (w.id, w.progress.map(|p| (p * 1000.0) as i64)) let:w>
                            {book_card(w)}
                        </For>
                    </div>
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
                    let action = if has_e && has_a { "Read & listen" } else if has_a { "Listen" } else { "Read" };
                    let progress = w.position.as_ref().map(|p| p.fraction).filter(|f| *f > 0.0);
                    let backdrop = w.cover_url.clone().map(|u| format!("background-image:url('{u}')"));
                    let long_description = w.description.as_ref().is_some_and(|d| d.len() > 420);
                    view! {
                        <div class="work">
                            <div class="work-backdrop" style=backdrop.unwrap_or_default()></div>
                            <div class="work-hero">
                                <div class="work-cover">{cover(w.cover_url.clone(), w.title.clone())}</div>
                                <div class="work-info">
                                    {w.series.clone().map(|s| view! {
                                        <p class="series">{s}{w.series_index.map(|i| format!(" · Book {i}")).unwrap_or_default()}</p>
                                    })}
                                    <h1>{w.title.clone()}</h1>
                                    <p class="author">{w.author.clone().unwrap_or_default()}</p>
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
                                        view! {
                                            <div class="edition-card" class:generated=a.origin == "generated" title=a.path.clone()>
                                                <div class="action-icon">{icon(if ebook { icons::BOOK } else { icons::HEADPHONES })}</div>
                                                <div>
                                                    <strong>{name}</strong>
                                                    <p>{asset_label(a)}</p>
                                                </div>
                                                {(a.origin == "generated").then(|| view! { <span class="pill ai">{icon(icons::SPARKLE)}"AI"</span> })}
                                            </div>
                                        }
                                    }).collect_view()}
                                </div>
                            </section>

                            <section class="work-section">
                                <h2>"Read or listen"</h2>
                                <div class="mode-cards">
                                    {reader::Mode::available(&w.assets).into_iter().map(|m| view! {
                                        <a class="mode-card" href=format!("#/read/{id}/{}", m.key())>
                                            <span class="mode-icon" aria-hidden="true">{m.icons().iter().map(|i| crate::icon(i)).collect_view()}</span>
                                            <span class="mode-text">
                                                <strong>{m.label()}</strong>
                                                <small>{m.detail()}</small>
                                            </span>
                                        </a>
                                    }).collect_view()}
                                </div>
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
                                    <h2>"Cast"</h2>
                                    <p class="hint">"Talk to any of them from the reader's Ask button."</p>
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
