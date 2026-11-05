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
    async fn start_recording() -> Result<JsValue, JsValue>;
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
    fn pager_init(book: &web_sys::Element, article: &web_sys::Element, on_next: &js_sys::Function, on_prev: &js_sys::Function, on_page: &js_sys::Function);
    #[wasm_bindgen(js_name = pagerDestroy)]
    fn pager_destroy(book: &web_sys::Element);
    #[wasm_bindgen(js_name = pagerEnd)]
    fn pager_end(book: &web_sys::Element);
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
    Read(i64),
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
            Some("read") => id().map(Route::Read).unwrap_or(Route::Library),
            Some("discover") => Route::Discover,
            Some("jobs") => Route::Jobs,
            Some("settings") => Route::Settings,
            Some("setup") => Route::Setup,
            _ => Route::Library,
        }
    }

    fn tab(&self) -> &'static str {
        match self {
            Route::Library | Route::Work(_) | Route::Read(_) => "library",
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

const TABS: [(&str, &str, &str); 4] = [
    ("library", "Library", "M4 4h5v16H4zM10 4h4v16h-4zM15.5 4.5l4-1 3 15.5-4 1z"),
    ("discover", "Discover", "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4"),
    ("jobs", "Jobs", "M12 3v4M12 17v4M3 12h4M17 12h4M5.6 5.6l2.8 2.8M15.6 15.6l2.8 2.8M5.6 18.4l2.8-2.8M15.6 8.4l2.8-2.8"),
    ("settings", "Settings", "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM4 12h2M18 12h2M12 4v2M12 18v2"),
];

pub fn icon(path: &'static str) -> impl IntoView {
    view! {
        <svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=path/></svg>
    }
}

#[component]
fn App() -> impl IntoView {
    let route = RwSignal::new(Route::parse(&current_hash()));
    let handler = Closure::<dyn Fn()>::new(move || route.set(Route::parse(&current_hash())));
    let _ = window().add_event_listener_with_callback("hashchange", handler.as_ref().unchecked_ref());
    handler.forget();
    let toast_sig = RwSignal::new(None::<String>);
    provide_context(Toast(toast_sig));
    // The reader takes the whole screen on phones.
    let immersive = move || matches!(route.get(), Route::Read(_));

    view! {
        <div class="app" class:immersive=immersive>
            <nav class="tabs">
                <a class="brand" href="#/">"duet"</a>
                {TABS.map(|(id, label, path)| view! {
                    <a href=format!("#/{}", if id == "library" { "" } else { id })
                        class:active=move || route.get().tab() == id>
                        {icon(path)}<span>{label}</span>
                    </a>
                })}
            </nav>
            <main>
                {move || match route.get() {
                    Route::Library => view! { <Library/> }.into_any(),
                    Route::Work(id) => view! { <WorkPage id/> }.into_any(),
                    Route::Read(id) => view! { <reader::Reader id/> }.into_any(),
                    Route::Discover => view! { <discover::Discover/> }.into_any(),
                    Route::Jobs => view! { <discover::Jobs/> }.into_any(),
                    Route::Settings => view! { <settings::SettingsPage/> }.into_any(),
                    Route::Setup => view! { <setup::SetupPage/> }.into_any(),
                }}
            </main>
            <Show when=move || toast_sig.get().is_some()>
                <div class="toast" on:click=move |_| toast_sig.set(None)>{move || toast_sig.get()}</div>
            </Show>
        </div>
    }
}

fn cover(url: Option<String>, title: String) -> impl IntoView {
    match url {
        Some(u) => view! { <img class="cover" src=u alt="" loading="lazy"/> }.into_any(),
        None => {
            // A generated cover: the title on a colour picked from it.
            let hue = title.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
            view! {
                <div class="cover placeholder" style=format!("--hue:{hue}")><span>{title}</span></div>
            }
            .into_any()
        }
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
    let scanning = RwSignal::new(false);
    let toast = toast();
    let scan = move |_| {
        scanning.set(true);
        spawn_local(async move {
            match api::post::<_, ScanReport>("/api/library/scan", &()).await {
                Ok(r) => {
                    let errors = if r.errors.is_empty() { String::new() } else { format!(", {} errors", r.errors.len()) };
                    toast.show(format!("{} books: {} ebooks and {} audiobooks added{errors}", r.works, r.ebooks, r.audiobooks));
                }
                Err(e) => toast.show(e.0),
            }
            scanning.set(false);
            works.refetch();
        });
    };
    let shown = move || {
        let f = filter.get().to_lowercase();
        works
            .get()
            .and_then(|r| r.ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|w| {
                f.is_empty()
                    || w.title.to_lowercase().contains(&f)
                    || w.author.as_deref().unwrap_or("").to_lowercase().contains(&f)
                    || w.series.as_deref().unwrap_or("").to_lowercase().contains(&f)
            })
            .collect::<Vec<_>>()
    };
    view! {
        <header class="page-head">
            <h1>"Library"</h1>
            <input class="search" type="search" placeholder="Filter by title, author or series"
                prop:value=move || filter.get() on:input=move |ev| filter.set(event_target_value(&ev))/>
            <button on:click=scan disabled=move || scanning.get()>
                {move || if scanning.get() { "Scanning…" } else { "Scan folders" }}
            </button>
        </header>
        <Show when=move || !setup_done.get()>
            <div class="banner">
                <span>"duet isn't fully set up yet."</span>
                <a class="button primary" href="#/setup">"Continue setup"</a>
            </div>
        </Show>
        <Suspense fallback=|| view! { <p class="empty">"Loading…"</p> }>
            {move || {
                let list = shown();
                if works.get().is_some() && list.is_empty() && filter.get().is_empty() {
                    return view! {
                        <div class="empty">
                            <p>"Your library is empty."</p>
                            <p>"Add your book folders in "<a href="#/settings">"Settings"</a>
                               " and scan them, or find free books in "<a href="#/discover">"Discover"</a>"."</p>
                        </div>
                    }.into_any();
                }
                view! {
                    <div class="grid">
                        <For each=move || shown() key=|w| (w.id, w.progress.map(|p| (p * 1000.0) as i64)) let:w>
                            <a class="card" href=format!("#/work/{}", w.id)>
                                {cover(w.cover_url.clone(), w.title.clone())}
                                <div class="card-body">
                                    <div class="title">{w.title.clone()}</div>
                                    <div class="author">{w.author.clone().unwrap_or_default()}</div>
                                    <div class="badges">
                                        {w.has_ebook.then(|| view! { <span class="badge" title="Ebook">"Read"</span> })}
                                        {w.has_audiobook.then(|| view! { <span class="badge" title="Audiobook">"Listen"</span> })}
                                        {w.has_sync.then(|| view! { <span class="badge sync" title="Text and audio in sync">"Synced"</span> })}
                                    </div>
                                    {w.progress.map(|p| view! {
                                        <div class="progress"><div style=format!("width:{:.1}%", p * 100.0)></div></div>
                                    })}
                                </div>
                            </a>
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
    let mut s = a.format.to_uppercase();
    if let Some(d) = a.duration_secs {
        s.push_str(&format!(" · {}", format_time(d)));
    }
    if a.tracks.len() > 1 {
        s.push_str(&format!(" · {} tracks", a.tracks.len()));
    }
    s.push_str(&format!(" · {}", size_label(a.size)));
    if a.origin == "generated" {
        s.push_str(" · generated");
    }
    s
}

pub async fn start_job(kind: JobKind, work_id: i64) {
    match api::post::<_, i64>("/api/jobs", &NewJob { kind, work_id, ebook_asset: None, audiobook_asset: None }).await {
        Ok(_) => toast().show(format!("{} started; follow it in Jobs", kind.label())),
        Err(e) => toast().show(e.0),
    }
}

#[component]
fn WorkPage(id: i64) -> impl IntoView {
    let work = LocalResource::new(move || async move { api::get::<WorkDetail>(&format!("/api/works/{id}")).await });
    let jobs = LocalResource::new(|| api::get::<Vec<Job>>("/api/jobs"));
    view! {
        <Suspense fallback=|| view! { <p class="empty">"Loading…"</p> }>
            {move || work.get().map(|r| match r {
                Err(e) => view! { <p class="empty">{e.0}</p> }.into_any(),
                Ok(w) => {
                    let ebooks: Vec<Asset> = w.assets.iter().filter(|a| a.kind == AssetKind::Ebook).cloned().collect();
                    let audio: Vec<Asset> = w.assets.iter().filter(|a| a.kind == AssetKind::Audiobook).cloned().collect();
                    let convertible = ebooks.iter().any(|a| ["mobi", "azw", "azw3", "fb2", "docx", "rtf", "odt", "lit"].contains(&a.format.as_str()));
                    let readable = ebooks.iter().any(|a| ["epub", "txt", "md", "html", "htm", "xhtml"].contains(&a.format.as_str()));
                    let running: Vec<Job> = jobs.get().and_then(|r| r.ok()).unwrap_or_default().into_iter()
                        .filter(|j| j.work_id == id && (j.status == "queued" || j.status == "running")).collect();
                    let busy = move |k: JobKind| running.iter().any(|j| j.kind == k);
                    let (has_e, has_a) = (!ebooks.is_empty(), !audio.is_empty());
                    let action = if has_e && has_a { "Read & listen" } else if has_a { "Listen" } else { "Read" };
                    view! {
                        <div class="work">
                            <div class="work-cover">{cover(w.cover_url.clone(), w.title.clone())}</div>
                            <div class="work-info">
                                <h1>{w.title.clone()}</h1>
                                <p class="author">{w.author.clone().unwrap_or_default()}</p>
                                {w.series.clone().map(|s| view! {
                                    <p class="series">{s}{w.series_index.map(|i| format!(" #{i}")).unwrap_or_default()}</p>
                                })}
                                <div class="actions">
                                    <a class="button primary" href=format!("#/read/{id}")>{action}</a>
                                    {w.has_sync.then(|| view! { <span class="badge sync">"Text and audio in sync"</span> })}
                                </div>
                                {w.description.clone().map(|d| view! { <p class="description">{d}</p> })}

                                <h2>"Editions"</h2>
                                <ul class="editions">
                                    {w.assets.iter().map(|a| view! {
                                        <li>
                                            <span class="kind">{if a.kind == AssetKind::Ebook { "Ebook" } else { "Audiobook" }}</span>
                                            <span class="meta">{asset_label(a)}</span>
                                            <span class="path" title=a.path.clone()>{a.path.rsplit('/').next().unwrap_or("").to_string()}</span>
                                        </li>
                                    }).collect_view()}
                                </ul>

                                <h2>"Make more of it"</h2>
                                <div class="job-actions">
                                    {(has_e && has_a).then(|| {
                                        let b = busy(JobKind::Align);
                                        view! {
                                            <button disabled=b on:click=move |_| spawn_local(start_job(JobKind::Align, id))>
                                                {if w.has_sync { "Re-sync text and audio" } else { "Sync text and audio" }}
                                            </button>
                                        }
                                    })}
                                    {(has_e && !readable && convertible).then(|| view! {
                                        <button on:click=move |_| spawn_local(start_job(JobKind::Convert, id))>"Convert to EPUB"</button>
                                    })}
                                    {(has_e && readable).then(|| view! {
                                        <button on:click=move |_| spawn_local(start_job(JobKind::FullCast, id))>"Create full-cast audiobook"</button>
                                    })}
                                    {(has_a && !has_e).then(|| view! {
                                        <button on:click=move |_| spawn_local(start_job(JobKind::Transcribe, id))>"Create ebook from audio"</button>
                                    })}
                                </div>

                                {(!w.cast.is_empty()).then(|| view! {
                                    <h2>"Cast"</h2>
                                    <ul class="cast">
                                        {w.cast.iter().map(|c| view! {
                                            <li><strong>{c.name.clone()}</strong><span>{c.description.clone()}</span></li>
                                        }).collect_view()}
                                    </ul>
                                })}
                            </div>
                        </div>
                    }.into_any()
                }
            })}
        </Suspense>
    }
}
