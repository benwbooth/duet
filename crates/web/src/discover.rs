//! Finding books (free sources and the user's own indexers), downloads, and background jobs.

use duet_common::{AssetKind, Download, Job, NewDownload, SearchResult};
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

#[component]
pub fn Discover() -> impl IntoView {
    let query = RwSignal::new(String::new());
    let kind = RwSignal::new(AssetKind::Ebook);
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
        let k = kind.get_untracked();
        spawn_local(async move {
            let url = format!("/api/search?q={}&kind={}", js_sys::encode_uri_component(&q), k.as_str());
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
            <h1>"Discover"</h1>
        </header>
        <form class="search-form" on:submit=move |ev| { ev.prevent_default(); search() }>
            <div class="segmented">
                <button type="button" class:active=move || kind.get() == AssetKind::Ebook on:click=move |_| kind.set(AssetKind::Ebook)>"Ebooks"</button>
                <button type="button" class:active=move || kind.get() == AssetKind::Audiobook on:click=move |_| kind.set(AssetKind::Audiobook)>"Audiobooks"</button>
            </div>
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
            <ul class="results">
                {r.results.into_iter().map(|res| {
                    let r2 = res.clone();
                    view! {
                        <li>
                            <div class="result-main">
                                <div class="title">{res.title.clone()}</div>
                                <div class="meta">
                                    {res.author.clone().map(|a| view! { <span>{a}</span> })}
                                    <span class="source">{res.source.clone()}</span>
                                    {res.format.clone().map(|f| view! { <span>{f.to_uppercase()}</span> })}
                                    {res.size.map(|s| view! { <span>{size_label(s)}</span> })}
                                    {res.seeders.map(|s| view! { <span>{format!("{s} seeders")}</span> })}
                                    {res.info_url.clone().map(|u| view! { <a href=u target="_blank" rel="noopener">"Details"</a> })}
                                </div>
                            </div>
                            <button on:click=move |_| download(r2.clone())>"Get"</button>
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
        <header class="page-head"><h1>"Jobs"</h1></header>
        <p class="hint">"Syncing, full-cast recording and transcription run here one at a time. They can take a while: hours for a long book on one GPU."</p>
        {move || {
            let list = jobs.get().and_then(|r| r.ok()).unwrap_or_default();
            if list.is_empty() {
                return view! { <p class="empty">"No jobs yet. Start one from a book's page."</p> }.into_any();
            }
            view! {
                <ul class="jobs">
                    {list.into_iter().map(|j| view! {
                        <li class=format!("job {}", j.status)>
                            <div class="title"><a href=format!("#/work/{}", j.work_id)>{j.kind.label()}</a></div>
                            <div class="meta"><span>{j.status.clone()}</span><span>{j.message.clone()}</span></div>
                            <div class="progress"><div style=format!("width:{:.1}%", j.progress * 100.0)></div></div>
                        </li>
                    }).collect_view()}
                </ul>
            }.into_any()
        }}
    }
}
