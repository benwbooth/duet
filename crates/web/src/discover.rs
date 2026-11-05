//! Finding books (free sources and the user's own indexers), downloads, and background jobs.

use duet_common::{AssetKind, Download, Job, JobKind, NewDownload, SearchResult};
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

#[component]
pub fn Discover() -> impl IntoView {
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(None::<SearchResponse>);
    let searching = RwSignal::new(false);
    let downloads = LocalResource::new(|| api::get::<Vec<Download>>("/api/downloads"));
    poll(3000, move || downloads.refetch());
    let toast = toast();

    let search = move || {
        let q = query.get_untracked();
        if q.trim().is_empty() {
            return;
        }
        searching.set(true);
        spawn_local(async move {
            let url = format!("/api/search?q={}", js_sys::encode_uri_component(&q));
            match api::get::<SearchResponse>(&url).await {
                Ok(r) => results.set(Some(r)),
                Err(e) => toast.show(e.0),
            }
            searching.set(false);
        });
    };

    let download = move |r: SearchResult| {
        let req = NewDownload { kind: r.kind, title: r.title.clone(), target: r.download.clone() };
        spawn_local(async move {
            match api::post::<_, ()>("/api/downloads", &req).await {
                Ok(()) => {
                    toast.show(format!("Downloading {}", req.title));
                    downloads.refetch();
                }
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
        <form class="search-form" on:submit=move |ev| { ev.prevent_default(); search() }>
            <input type="search" placeholder="Title or author" prop:value=move || query.get()
                on:input=move |ev| query.set(event_target_value(&ev))/>
            <button type="submit" class="primary" disabled=move || searching.get()>
                {move || if searching.get() { "Searching…" } else { "Search" }}
            </button>
        </form>
        <p class="hint">"Free sources: Project Gutenberg, Standard Ebooks, Internet Archive and LibriVox. Add your own Torznab indexers in "<a href="#/settings">"Settings"</a>"."</p>

        {move || results.get().map(|r| view! {
            {(!r.errors.is_empty()).then(|| view! {
                <ul class="errors">{r.errors.iter().map(|e| view! { <li>{e.clone()}</li> }).collect_view()}</ul>
            })}
            {r.results.is_empty().then(|| view! { <p class="empty">"Nothing found."</p> })}
            <ul class="books">
                {group(r.results).into_iter().map(|g| {
                    let has = |k: AssetKind| g.options.iter().any(|o| o.kind == k);
                    let (ebook, audio) = (has(AssetKind::Ebook), has(AssetKind::Audiobook));
                    view! {
                        <li class="book">
                            <div class="book-head">
                                <div>
                                    <div class="title">{g.title.clone()}</div>
                                    {g.author.clone().map(|a| view! { <div class="author">{a}</div> })}
                                </div>
                                <div class="availability">
                                    <span class="chip" class:on=ebook>{if ebook { "✓ Ebook" } else { "No ebook" }}</span>
                                    <span class="chip" class:on=audio>{if audio { "✓ Audiobook" } else { "No audiobook" }}</span>
                                </div>
                            </div>
                            <ul class="options">
                                {g.options.into_iter().map(|res| {
                                    let r2 = res.clone();
                                    view! {
                                        <li>
                                            <span class="kind">{if res.kind == AssetKind::Ebook { "Ebook" } else { "Audiobook" }}</span>
                                            <div class="meta">
                                                <span class="source">{res.source.clone()}</span>
                                                {res.format.clone().map(|f| view! { <span>{f.to_uppercase()}</span> })}
                                                {res.size.map(|s| view! { <span>{size_label(s)}</span> })}
                                                {res.seeders.map(|s| view! { <span>{format!("{s} seeders")}</span> })}
                                                // Indexer releases have their own names; show them so you know what you're getting.
                                                {(res.author.is_none() || res.source == "Internet Archive").then(|| {
                                                    // Archive items are separate scans/editions; tell them apart by item id.
                                                    let label = if res.source == "Internet Archive" {
                                                        res.info_url.as_deref().and_then(|u| u.rsplit('/').next()).unwrap_or(&res.title).to_string()
                                                    } else {
                                                        res.title.clone()
                                                    };
                                                    view! { <span class="release">{label}</span> }
                                                })}
                                                {res.info_url.clone().map(|u| view! { <a href=u target="_blank" rel="noopener">"Details"</a> })}
                                            </div>
                                            <button on:click=move |_| download(r2.clone())>"Get"</button>
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                        </li>
                    }
                }).collect_view()}
            </ul>
        })}

        <h2>"Downloads"</h2>
        {move || {
            let list = downloads.get().and_then(|r| r.ok()).unwrap_or_default();
            if list.is_empty() {
                return view! { <p class="empty">"No downloads yet."</p> }.into_any();
            }
            view! {
                <ul class="downloads">
                    {list.into_iter().map(|d| view! {
                        <li>
                            <div class="title">{d.name}</div>
                            <div class="meta"><span>{d.source}</span><span>{d.state.clone()}</span></div>
                            <div class="progress"><div style=format!("width:{:.1}%", d.progress * 100.0)></div></div>
                        </li>
                    }).collect_view()}
                </ul>
            }.into_any()
        }}
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
