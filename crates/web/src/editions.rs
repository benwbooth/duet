//! "Find another edition" on a book's page: search the usual sources for a different ebook or
//! audiobook of this book, then add it alongside what the book has or put it in place of it.

use duet_common::{AssetKind, SearchResult, text::{normalize, title_key}};
use leptos::portal::Portal;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;

use crate::{api, icon, icons, size_label, toast};

#[derive(Deserialize, Clone, Default)]
struct SearchResponse {
    results: Vec<SearchResult>,
    #[serde(default)]
    errors: Vec<String>,
}

/// An edition on its way to this book (GET /api/works/{id}/editions).
#[derive(Deserialize, Clone, PartialEq)]
struct Pending {
    kind: AssetKind,
    replace: bool,
}

/// Where to look, as (id for the API, name shown): the built-in public-domain sources and the
/// user's own indexers. Each is asked on its own so hits show up as they come.
const SOURCES: [(&str, &str); 5] = [
    ("standard_ebooks", "Standard Ebooks"),
    ("gutenberg", "Project Gutenberg"),
    ("internet_archive", "Internet Archive"),
    ("librivox", "LibriVox"),
    ("indexers", "Your indexers"),
];

/// How one source's search went: None while it's still looking.
type SourceStatus = Option<Result<usize, String>>;

fn noun(kind: AssetKind) -> &'static str {
    if kind == AssetKind::Ebook { "ebook" } else { "audiobook" }
}

/// The button, the search panel, and a card for each edition still downloading. `arrived` runs
/// when one lands, so the page can show it.
#[component]
pub fn FindEdition(
    id: i64,
    title: String,
    author: Option<String>,
    has_ebook: bool,
    has_audiobook: bool,
    arrived: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let toast = toast();
    let panel = crate::downloads_panel();
    let open = RwSignal::new(false);
    // Start on whatever the book is missing.
    let kind = RwSignal::new(if has_ebook && !has_audiobook { AssetKind::Audiobook } else { AssetKind::Ebook });
    // Just the title: LibriVox and most indexers match fewer things when the author is added.
    let query = RwSignal::new(title.clone());
    let results = RwSignal::new(None::<Vec<SearchResult>>);
    let status = RwSignal::new(Vec::<(&'static str, SourceStatus)>::new());
    let generation = RwSignal::new(0u32);
    let searching = move || status.with(|s| s.iter().any(|(_, st)| st.is_none()));
    let starting = RwSignal::new(false);
    let pending = RwSignal::new(Vec::<Pending>::new());
    let want = StoredValue::new(title_key(&title));
    let surname = StoredValue::new(author.as_deref().map(normalize).and_then(|a| a.rsplit(' ').next().map(str::to_string)).unwrap_or_default());

    let refresh_pending = move || {
        spawn_local(async move {
            let Ok(now) = api::get::<Vec<Pending>>(&format!("/api/works/{id}/editions")).await else { return };
            let before = pending.get_untracked();
            if now.len() < before.len() {
                // One landed (or gave up): show the book as it is now.
                let gone = before.iter().find(|p| !now.contains(p)).map(|p| p.kind).unwrap_or(AssetKind::Ebook);
                toast.show(format!("The new {} is in", noun(gone)));
                arrived();
            }
            pending.set(now);
        });
    };
    refresh_pending();
    // Checks every few seconds while something is on its way.
    if let Ok(h) = set_interval_with_handle(
        move || {
            if !pending.with_untracked(|p| p.is_empty()) {
                refresh_pending();
            }
        },
        std::time::Duration::from_secs(4),
    ) {
        on_cleanup(move || h.clear());
    }

    let search = move || {
        let q = query.get_untracked();
        if q.trim().is_empty() {
            return;
        }
        // A new search wins over answers still coming in for the last one.
        generation.update(|g| *g += 1);
        let this = generation.get_untracked();
        results.set(Some(Vec::new()));
        status.set(SOURCES.iter().map(|(_, name)| (*name, None)).collect());
        for (i, (src, _)) in SOURCES.into_iter().enumerate() {
            let url = format!("/api/search?q={}&source={src}", js_sys::encode_uri_component(&q));
            spawn_local(async move {
                let answer = api::get::<SearchResponse>(&url).await;
                if generation.get_untracked() != this {
                    return;
                }
                let outcome = match answer {
                    Ok(r) => {
                        let n = r.results.len();
                        let err = r.errors.first().cloned();
                        results.update(|all| all.get_or_insert_with(Vec::new).extend(r.results));
                        match err {
                            Some(e) if n == 0 => Err(e),
                            _ => Ok(n),
                        }
                    }
                    Err(e) => Err(e.0),
                };
                status.update(|s| {
                    if let Some(slot) = s.get_mut(i) {
                        slot.1 = Some(outcome);
                    }
                });
            });
        }
    };
    let show = move |_| {
        open.set(true);
        if results.get_untracked().is_none() {
            search();
        }
    };
    // This kind's hits, the ones that look like this book first, then the best seeded.
    let hits = move || {
        let k = kind.get();
        let mut list: Vec<SearchResult> = results.get().unwrap_or_default().into_iter().filter(|r| r.kind == k).collect();
        let (w, sn) = (want.get_value(), surname.get_value());
        list.sort_by_key(|r| {
            let t = title_key(&r.title);
            let same_title = t == w || t.starts_with(w.as_str()) || (w.len() >= 4 && t.contains(w.as_str()));
            // Indexer releases carry the author in their name rather than as an author.
            let by = format!("{} {}", r.author.as_deref().unwrap_or(""), r.title);
            let same_author = sn.is_empty() || normalize(&by).split(' ').any(|x| x == sn);
            (!same_title, !same_author, std::cmp::Reverse(r.seeders.unwrap_or(0)))
        });
        list
    };
    let get = move |r: SearchResult, replace: bool| {
        if starting.get_untracked() {
            return;
        }
        starting.set(true);
        spawn_local(async move {
            let k = r.kind;
            // Everything the search card knows goes along (sources add their own fields, like a
            // torrent link); the server reads it as a NewDownload.
            let mut download = serde_json::to_value(&r).unwrap_or_default();
            if let Some(d) = download.as_object_mut() {
                let target = d.remove("download").unwrap_or_default();
                d.insert("target".into(), target);
            }
            let body = serde_json::json!({ "download": download, "replace": replace });
            let result = api::post::<_, ()>(&format!("/api/works/{id}/editions"), &body).await;
            starting.set(false);
            match result {
                Ok(()) => {
                    open.set(false);
                    panel.opened();
                    toast.show(if replace {
                        format!("Getting the new {}. It takes the old one's place when it's done, and your place carries over.", noun(k))
                    } else {
                        format!("Getting the {}. It'll show up in Editions when it's done.", noun(k))
                    });
                    refresh_pending();
                }
                Err(e) => toast.show(format!("Couldn't get it: {}", e.0)),
            }
        });
    };
    let has = move |k: AssetKind| if k == AssetKind::Ebook { has_ebook } else { has_audiobook };

    view! {
        {move || pending.get().into_iter().map(|p| view! {
            <div class="edition-card pending-edition">
                <div class="action-icon"><span class="spinner"></span></div>
                <div>
                    <strong>{format!("New {} on its way", noun(p.kind))}</strong>
                    <p>{if p.replace { "Replaces the current one when it's done" } else { "Joins this book when it's done" }}</p>
                </div>
            </div>
        }).collect_view()}
        <button class="edition-card find-edition" on:click=show>
            <div class="action-icon">{icon(icons::DISCOVER)}</div>
            <div>
                <strong>"Find another edition"</strong>
                <p>"A different ebook or audiobook of this book"</p>
            </div>
        </button>
        <Show when=move || open.get()>
            // In <body>: the book page is transformed (its entrance animation), which would pin a
            // fixed panel to the page instead of the screen.
            <Portal>
            <div class="check-modal-backdrop" on:click=move |_| if !starting.get_untracked() { open.set(false) }>
                <div class="check-modal edition-finder" class:saving=move || starting.get() on:click=|ev| ev.stop_propagation()>
                    <h2>"Find another edition"</h2>
                    <p class="muted">"Search your sources for a different ebook or audiobook of this book. Add it next to what you have, or let it take the current one's place. Your place in the book is kept either way."</p>
                    <div class="row">
                        <div class="segmented" role="tablist">
                            {[AssetKind::Ebook, AssetKind::Audiobook].into_iter().map(|k| view! {
                                <button class:active=move || kind.get() == k role="tab" aria-selected=move || (kind.get() == k).to_string()
                                    on:click=move |_| kind.set(k)>
                                    {icon(if k == AssetKind::Ebook { icons::BOOK } else { icons::HEADPHONES })}
                                    {if k == AssetKind::Ebook { "Ebook" } else { "Audiobook" }}
                                </button>
                            }).collect_view()}
                        </div>
                        <form class="edition-search" on:submit=move |ev| { ev.prevent_default(); search(); }>
                            <input type="search" aria-label="Search for" prop:value=move || query.get()
                                on:input=move |ev| query.set(event_target_value(&ev))/>
                            <button class="primary" type="submit">"Search"</button>
                        </form>
                    </div>
                    <div class="source-status">
                        {move || status.get().into_iter().map(|(name, st)| {
                            let (class, text) = match &st {
                                None => ("searching", name.to_string()),
                                Some(Ok(0)) => ("none", format!("{name}: nothing")),
                                Some(Ok(n)) => ("found", format!("{name}: {n}")),
                                Some(Err(_)) => ("failed", format!("{name}: unavailable")),
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
                    {move || {
                        let list = hits();
                        let k = kind.get();
                        if list.is_empty() && searching() {
                            return view! { <p class="cover-wait"><span class="spinner small"></span>{format!("Looking for {}s…", noun(k))}</p> }.into_any();
                        }
                        view! {
                            {list.is_empty().then(|| view! { <p class="muted">{format!("No {}s found. Try fewer words, or just the title.", noun(k))}</p> })}
                            <ul class="options edition-results">
                                {list.into_iter().map(|r| {
                                    // Archive items and indexer releases differ by name: show it.
                                    let name = if r.source == "Internet Archive" {
                                        r.info_url.as_deref().and_then(|u| u.rsplit('/').next()).map(str::to_string).unwrap_or_else(|| r.title.clone())
                                    } else {
                                        format!("{}{}", r.title, r.author.as_ref().map(|a| format!(" · {a}")).unwrap_or_default())
                                    };
                                    let (r1, r2) = (r.clone(), r.clone());
                                    view! {
                                        <li class="edition-hit">
                                            <div class="hit-text">
                                                <span class="hit-name" title=name.clone()>{name.clone()}</span>
                                                <div class="meta">
                                                    <span class="source">{r.source.clone()}</span>
                                                    {r.format.clone().map(|f| view! { <span>{f.to_uppercase()}</span> })}
                                                    {r.size.map(|s| view! { <span>{size_label(s)}</span> })}
                                                    {r.seeders.map(|s| view! { <span>{format!("{s} seeders")}</span> })}
                                                    {r.info_url.clone().map(|u| view! { <a href=u target="_blank" rel="noopener">"Details"</a> })}
                                                </div>
                                            </div>
                                            <div class="hit-actions">
                                                {has(k).then(|| view! {
                                                    <button disabled=move || starting.get() on:click=move |_| get(r1.clone(), true)>"Replace"</button>
                                                })}
                                                <button class="primary" disabled=move || starting.get() on:click=move |_| get(r2.clone(), false)>
                                                    {if has(k) { "Add" } else { "Get" }}
                                                </button>
                                            </div>
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                        }.into_any()
                    }}
                    <button class="ghost" disabled=move || starting.get() on:click=move |_| open.set(false)>"Close"</button>
                    {move || starting.get().then(|| view! {
                        <div class="cover-saving"><span class="spinner big"></span><p>"Starting the download…"</p></div>
                    })}
                </div>
            </div>
            </Portal>
        </Show>
    }
}
