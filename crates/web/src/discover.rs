//! Finding books (free sources and the user's own indexers), downloads, and background jobs.

use duet_common::{AssetKind, Job, JobKind, NewDownload, SearchResult};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;

use crate::{api, size_label, toast};

#[derive(Deserialize, Clone, Default)]
struct SearchResponse {
    results: Vec<SearchResult>,
    errors: Vec<String>,
}

/// Re-runs `f` every few seconds while the page is open.
fn poll(every_ms: u64, f: impl Fn() + Clone + 'static) {
    if let Ok(h) = set_interval_with_handle(f, std::time::Duration::from_millis(every_ms)) {
        on_cleanup(move || h.clear());
    }
}

/// One book in the search results, with every way to get it.
#[derive(Clone)]
struct BookGroup {
    key: String,
    title: String,
    author: Option<String>,
    options: Vec<SearchResult>,
}

/// Groups hits by book. Free sources give a title and author; indexer releases only have a
/// release name, so they join the book whose title appears in it.
fn group(results: Vec<SearchResult>) -> Vec<BookGroup> {
    use duet_common::text::{normalize, title_key};
    let surname = |a: &Option<String>| a.as_deref().map(normalize).and_then(|n| n.rsplit(' ').next().map(str::to_string)).unwrap_or_default();
    let mut groups: Vec<BookGroup> = Vec::new();
    let (named, releases): (Vec<_>, Vec<_>) = results.into_iter().partition(|r| r.author.is_some());
    for r in named {
        let key = format!("{}|{}", title_key(&r.title), surname(&r.author));
        match groups.iter_mut().find(|g| g.key == key) {
            Some(g) => g.options.push(r),
            None => groups.push(BookGroup { key, title: r.title.clone(), author: r.author.clone(), options: vec![r] }),
        }
    }
    for r in releases {
        let name = normalize(&r.title);
        let home = groups.iter_mut().find(|g| {
            let t = g.key.split('|').next().unwrap_or("");
            !t.is_empty() && name.contains(t)
        });
        match home {
            Some(g) => g.options.push(r),
            None => {
                let key = format!("{}|", title_key(&r.title));
                match groups.iter_mut().find(|g| g.key == key) {
                    Some(g) => g.options.push(r),
                    None => groups.push(BookGroup { key, title: r.title.clone(), author: None, options: vec![r] }),
                }
            }
        }
    }
    for g in &mut groups {
        // Ebooks first, then the best-seeded.
        g.options.sort_by_key(|o| (o.kind == AssetKind::Audiobook, std::cmp::Reverse(o.seeders.unwrap_or(0))));
    }
    // Books available both ways first, then those with the most options.
    groups.sort_by_key(|g| {
        let both = g.options.iter().any(|o| o.kind == AssetKind::Ebook) && g.options.iter().any(|o| o.kind == AssetKind::Audiobook);
        (!both, std::cmp::Reverse(g.options.len()))
    });
    groups
}

/// One book in the results: pick an ebook, an audiobook or both, then get them together.
#[component]
fn FoundBook(g: BookGroup, download: impl Fn(SearchResult) + Copy + Send + Sync + 'static) -> impl IntoView {
    let of = |k: AssetKind| g.options.iter().filter(|o| o.kind == k).cloned().collect::<Vec<_>>();
    let (ebooks, audio) = (of(AssetKind::Ebook), of(AssetKind::Audiobook));
    // The best of each is picked to start with, so one click gets both.
    let pick_e = RwSignal::new((!ebooks.is_empty()).then_some(0usize));
    let pick_a = RwSignal::new((!audio.is_empty()).then_some(0usize));
    let (e2, a2) = (ebooks.clone(), audio.clone());
    let (book_title, book_author) = (g.title.clone(), g.author.clone());
    // Both halves are filed under this book's title and author, so they end up as one book.
    let as_this_book = move |mut r: SearchResult| {
        r.title = book_title.clone();
        if book_author.is_some() {
            r.author = book_author.clone();
        }
        r
    };
    let get = move |_| {
        if let Some(i) = pick_e.get_untracked() {
            download(as_this_book(e2[i].clone()));
        }
        if let Some(i) = pick_a.get_untracked() {
            download(as_this_book(a2[i].clone()));
        }
    };
    let label = move || match (pick_e.get().is_some(), pick_a.get().is_some()) {
        (true, true) => "Get ebook and audiobook",
        (true, false) => "Get ebook",
        (false, true) => "Get audiobook",
        (false, false) => "Pick an ebook or audiobook",
    };
    view! {
        <article class="found-book">
            <header>
                <div>
                    <h2>{g.title.clone()}</h2>
                    {g.author.clone().map(|a| view! { <p class="author">{a}</p> })}
                </div>
                <button class="primary" disabled=move || pick_e.get().is_none() && pick_a.get().is_none() on:click=get>
                    {crate::icon(crate::icons::DOWNLOAD)}{label}
                </button>
            </header>
            <div class="result-columns">
                {result_column("Ebook", crate::icons::BOOK, ebooks, pick_e)}
                {result_column("Audiobook", crate::icons::HEADPHONES, audio, pick_a)}
            </div>
        </article>
    }
}

/// The ebook or audiobook options for one book; click one to pick it, click again to drop it.
fn result_column(heading: &'static str, path: &'static str, options: Vec<SearchResult>, pick: RwSignal<Option<usize>>) -> impl IntoView {
    let count = options.len();
    view! {
        <section class="result-column" class:empty-col=count == 0>
            <h3>{crate::icon(path)}{heading}{(count > 0).then(|| view! { <span class="count">{count}</span> })}</h3>
            {(count == 0).then(|| view! { <p class="muted">"Not found"</p> })}
            <ul class="options">
                {options.into_iter().enumerate().map(|(i, res)| {
                    // Archive items are separate scans/editions, and indexer releases have their own
                    // names: show them so you know what you're getting.
                    let label = if res.source == "Internet Archive" {
                        res.info_url.as_deref().and_then(|u| u.rsplit('/').next()).map(str::to_string)
                    } else if res.author.is_none() {
                        Some(res.title.clone())
                    } else {
                        None
                    };
                    let on = move || pick.get() == Some(i);
                    view! {
                        <li class="option" class:picked=on role="radio" aria-checked=move || on().to_string()
                            on:click=move |_| pick.update(|p| *p = if *p == Some(i) { None } else { Some(i) })>
                            <span class="radio" aria-hidden="true"></span>
                            <div class="meta">
                                <span class="source">{res.source.clone()}</span>
                                {res.format.clone().map(|f| view! { <span>{f.to_uppercase()}</span> })}
                                {res.size.map(|s| view! { <span>{size_label(s)}</span> })}
                                {res.seeders.map(|s| view! { <span>{format!("{s} seeders")}</span> })}
                                {label.map(|l| view! { <span class="release">{l}</span> })}
                                {res.info_url.clone().map(|u| view! {
                                    <a href=u target="_blank" rel="noopener" on:click=|ev| ev.stop_propagation()>"Details"</a>
                                })}
                            </div>
                        </li>
                    }
                }).collect_view()}
            </ul>
        </section>
    }
}

/// The places Discover searches, as (id for the API, name shown).
const SOURCES: [(&str, &str); 5] = [
    ("gutenberg", "Project Gutenberg"),
    ("standard_ebooks", "Standard Ebooks"),
    ("internet_archive", "Internet Archive"),
    ("librivox", "LibriVox"),
    ("indexers", "Your indexers"),
];

#[component]
pub fn Discover() -> impl IntoView {
    let query = RwSignal::new(String::new());
    let typed = RwSignal::new(0u32);
    let results = RwSignal::new(None::<SearchResponse>);
    // Each source's progress: None while searching, then Ok(count) or Err(message).
    let status = RwSignal::new(Vec::<(&'static str, Option<Result<usize, String>>)>::new());
    let searching = move || status.with(|s| s.iter().any(|(_, st)| st.is_none()));
    // Which search the arriving answers belong to, so a slow source can't add to a newer search.
    let generation = RwSignal::new(0u32);
    let panel = crate::downloads_panel();
    let toast = toast();

    // Every source is asked separately, so results show up as each one answers.
    let search = move || {
        let q = query.get_untracked();
        if q.trim().is_empty() {
            return;
        }
        let this = generation.get_untracked() + 1;
        generation.set(this);
        results.set(Some(SearchResponse::default()));
        status.set(SOURCES.iter().map(|(_, label)| (*label, None)).collect());
        for (i, (id, _)) in SOURCES.iter().enumerate() {
            let url = format!("/api/search?q={}&source={id}", js_sys::encode_uri_component(&q));
            spawn_local(async move {
                let answer = api::get::<SearchResponse>(&url).await;
                if generation.get_untracked() != this {
                    return;
                }
                let outcome = match answer {
                    Ok(r) => {
                        let n = r.results.len();
                        let err = r.errors.first().cloned();
                        results.update(|all| {
                            if let Some(all) = all {
                                all.results.extend(r.results);
                            }
                        });
                        match err {
                            Some(e) if n == 0 => Err(e),
                            _ => Ok(n),
                        }
                    }
                    Err(e) => Err(e.0),
                };
                status.update(|s| s[i].1 = Some(outcome));
            });
        }
    };

    let download = move |r: SearchResult| {
        let req = NewDownload { kind: r.kind, title: r.title.clone(), target: r.download.clone(), author: r.author.clone() };
        spawn_local(async move {
            match api::post::<_, ()>("/api/downloads", &req).await {
                Ok(()) => panel.opened(),
                Err(e) => toast.show(e.0),
            }
        });
    };

    view! {
        <header class="page-head">
            <div class="titles">
                <h1>"Discover"</h1>
                <p class="subtitle">"Free classics, plus anything your own indexers find."</p>
            </div>
        </header>
        <form class="search-form big-search" on:submit=move |ev| { ev.prevent_default(); search() }>
            {crate::icon(crate::icons::DISCOVER)}
            <input type="search" placeholder="Search by title or author" aria-label="Search for books" autofocus
                prop:value=move || query.get()
                on:input=move |ev| {
                    query.set(event_target_value(&ev));
                    // Search as you type, once typing pauses.
                    let n = typed.get_untracked() + 1;
                    typed.set(n);
                    spawn_local(async move {
                        gloo_timers::future::TimeoutFuture::new(450).await;
                        if typed.get_untracked() == n && query.get_untracked().trim().chars().count() >= 3 {
                            search();
                        }
                    });
                }/>
            <Show when=searching>
                <span class="spinner"></span>
            </Show>
        </form>
        <p class="hint">"Free sources: Project Gutenberg, Standard Ebooks, Internet Archive and LibriVox. Add your own Torznab indexers in "<a href="#/settings">"Settings"</a>"."</p>

        <Show when=move || !status.with(Vec::is_empty)>
            <div class="source-status">
                {move || status.get().into_iter().map(|(label, st)| {
                    let (class, text) = match &st {
                        None => ("searching", format!("{label}")),
                        Some(Ok(0)) => ("none", format!("{label}: nothing")),
                        Some(Ok(n)) => ("found", format!("{label}: {n}")),
                        Some(Err(_)) => ("failed", format!("{label}: unavailable")),
                    };
                    let title = match st { Some(Err(e)) => e, _ => String::new() };
                    view! {
                        <span class=format!("source-chip {class}") title=title>
                            {(class == "searching").then(|| view! { <span class="spinner small"></span> })}
                            {text}
                        </span>
                    }
                }).collect_view()}
            </div>
        </Show>
        {move || results.get().map(|r| view! {
            {(r.results.is_empty() && !searching()).then(|| view! { <p class="empty">"Nothing found. Try fewer words, or just the title."</p> })}
            {(r.results.is_empty() && searching()).then(|| view! {
                <div class="search-wait"><span class="spinner big"></span><p>"Searching the libraries…"</p></div>
            })}
            <div class="found-books">
                {group(r.results).into_iter().map(|g| view! { <FoundBook g download/> }).collect_view()}
            </div>
        })}

    }
}

#[component]
pub fn Jobs() -> impl IntoView {
    let jobs = LocalResource::new(|| api::get::<Vec<Job>>("/api/jobs"));
    poll(2000, move || jobs.refetch());
    view! {
        <header class="page-head">
            <div class="titles">
                <h1>"Jobs"</h1>
                <p class="subtitle">"Syncing, full-cast recording and transcription"</p>
            </div>
        </header>
        <p class="hint">"Jobs run one at a time and can take a while: a long book can take hours on one graphics card."</p>
        {move || {
            let list = jobs.get().and_then(|r| r.ok()).unwrap_or_default();
            if list.is_empty() {
                return view! {
                    <div class="empty-state">
                        <div class="empty-icon">{crate::icon(crate::icons::JOBS)}</div>
                        <h2>"Nothing running"</h2>
                        <p>"Syncing, full-cast audiobooks and transcriptions show up here. Start one from a book's page."</p>
                    </div>
                }.into_any();
            }
            view! {
                <ul class="jobs">
                    {list.into_iter().map(|j| {
                        let path = match j.kind {
                            JobKind::Align => crate::icons::SYNC,
                            JobKind::FullCast => crate::icons::MASKS,
                            JobKind::Transcribe => crate::icons::QUILL,
                            JobKind::Convert => crate::icons::CONVERT,
                        };
                        let active = j.status == "running" || j.status == "queued";
                        view! {
                            <li class=format!("job {}", j.status)>
                                <div class="job-row">
                                    <div class="action-icon">{crate::icon(path)}</div>
                                    <div class="grow">
                                        <div class="title"><a href=format!("#/work/{}", j.work_id)>{j.kind.label()}</a></div>
                                        <div class="meta"><span>{j.message.clone()}</span></div>
                                    </div>
                                    <span class=format!("status-pill {}", j.status)>{j.status.clone()}</span>
                                </div>
                                {active.then(|| view! {
                                    <div class="progress"><div style=format!("width:{:.1}%", j.progress * 100.0)></div></div>
                                })}
                            </li>
                        }
                    }).collect_view()}
                </ul>
            }.into_any()
        }}
    }
}
