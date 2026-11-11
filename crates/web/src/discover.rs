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
    #[serde(default)]
    need_check: bool,
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

/// Surname for grouping: "Hill, Joe", "Hill, Joe(Author)" and "Joe Hill"
/// must all meet at "hill" — check the comma on the raw string, because
/// normalization itself strips commas.
fn surname_of(author: &Option<String>) -> String {
    use duet_common::text::normalize;
    author.as_deref().map(|n| {
        let base = n.split(',').next().unwrap_or(n);
        let norm = normalize(base);
        if n.contains(',') { norm.trim().to_string() } else { norm.rsplit(' ').next().unwrap_or("").to_string() }
    }).unwrap_or_default()
}

/// Ebook formats that are real books rather than comics.
fn is_prose(o: &SearchResult) -> bool {
    matches!(o.format.as_deref(), Some("epub" | "pdf" | "mobi" | "azw" | "azw3" | "fb2" | "djvu" | "txt"))
}

/// Ebook formats, best first: real book files before scans, comics last.
fn format_rank(o: &SearchResult) -> u8 {
    match o.format.as_deref() {
        Some("epub") => 0,
        Some("azw3" | "mobi" | "azw") => 1,
        Some("fb2" | "djvu" | "txt") => 2,
        Some("pdf") => 3,
        Some("cbz" | "cbr" | "cbt" | "cb7") => 5,
        _ => 4,
    }
}

/// Groups hits by book. Free sources give a title and author; indexer releases only have a
/// release name, so they join the book whose title appears in it.
/// Exact title matches for the query sort first; comics sort after prose.
fn group(results: Vec<SearchResult>, query: &str) -> Vec<BookGroup> {
    use duet_common::text::{normalize, title_key};
    let qkey = title_key(query);
    let mut groups: Vec<BookGroup> = Vec::new();
    let (named, releases): (Vec<_>, Vec<_>) = results.into_iter().partition(|r| r.author.is_some());
    for r in named {
        let key = format!("{}|{}", title_key(&r.title), surname_of(&r.author));
        match groups.iter_mut().find(|g| g.key == key) {
            Some(g) => g.options.push(r),
            None => groups.push(BookGroup { key, title: r.title.clone(), author: r.author.clone(), options: vec![r] }),
        }
    }
    for r in releases {
        let name = normalize(&r.title);
        // Join the book whose title this release starts with (word boundary),
        // so a 2-letter key like "it" can't substring-match "city" or "with".
        // Longer keys keep the old contains behavior for mid-string matches.
        let home = groups.iter_mut().find(|g| {
            let t = g.key.split('|').next().unwrap_or("");
            !t.is_empty() && (name == t || name.starts_with(&format!("{t} ")) || (t.len() >= 4 && name.contains(t)))
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
        // Ebooks first in the best format, then the best-seeded.
        g.options.sort_by_key(|o| (o.kind == AssetKind::Audiobook, format_rank(o), std::cmp::Reverse(o.seeders.unwrap_or(0))));
    }
    // Exact title match first (preferring attributed books over bare
    // filenames), then title-starts-with-query, then prose before comics,
    // then books available both ways, then option count.
    groups.sort_by_key(|g| {
        let t = title_key(&g.title);
        let exact = t == qkey;
        let named = g.author.as_ref().is_some_and(|a| !a.is_empty());
        let prefix = t.starts_with(&qkey) || qkey.starts_with(&t);
        let prose = g.options.iter().any(is_prose);
        let both = g.options.iter().any(|o| o.kind == AssetKind::Ebook) && g.options.iter().any(|o| o.kind == AssetKind::Audiobook);
        (!exact, !named, !prefix, !prose, !both, std::cmp::Reverse(g.options.len()))
    });
    groups
}

/// One book in the results: pick an ebook, an audiobook or both, then get them together.
#[component]
fn FoundBook(g: BookGroup, on_need_check: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
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
    let sending = RwSignal::new(false);
    let panel = crate::downloads_panel();
    // Books with a download on its way stay greyed out until the job
    // completes (imported into the library), fails, or 30 minutes pass.
    // Keyed by normalized title + kind.
    let pending = RwSignal::new(std::collections::HashMap::<(String, String), f64>::new());
    // Keys already announced, so one failed download pops the browser once.
    let announced = RwSignal::new(std::collections::HashSet::<(String, String)>::new());
    // Re-check pending jobs every few seconds, but only while any exist.
    if let Ok(h) = set_interval_with_handle(
        move || {
            if pending.with(|p| p.is_empty()) {
                return;
            }
            let mut ask_human = false;
            spawn_local(async move {
                let lib = api::get::<Vec<duet_common::WorkSummary>>("/api/library").await.unwrap_or_default();
                let dls = api::get::<Vec<duet_common::Download>>("/api/downloads").await.unwrap_or_default();
                let now = js_sys::Date::now() / 1000.0;
                pending.update(|p| {
                    p.retain(|(t, k), started| {
                        if now - *started > 1800.0 {
                            return false;
                        }
                        use duet_common::text::title_key;
                        let done = lib.iter().any(|w| {
                            title_key(&w.title) == title_key(t)
                                && ((k == "ebook" && w.has_ebook) || (k == "audiobook" && w.has_audiobook))
                        });
                        if done {
                            return false;
                        }
                        let failed = dls.iter().find(|d| {
                            title_key(&d.name) == title_key(t) && d.kind.as_str() == k && d.state == "failed"
                        });
                        if let Some(f) = failed {
                            // A check-walled failure needs hands: click into the
                            // record in the browser, solve, then Get again.
                            if f.error.as_deref().is_some_and(|e| e.to_lowercase().contains("bot check"))
                                && announced.with(|a| !a.contains(&(t.clone(), k.clone())))
                            {
                                announced.update(|a| {
                                    a.insert((t.clone(), k.clone()));
                                });
                                ask_human = true;
                            }
                            return false;
                        }
                        true
                    });
                });
                if ask_human {
                    toast().show("That download needs the bot check solved — click the blinking Anna's Archive badge to open the browser, solve it, then Get again.".to_string());
                    on_need_check();
                }
            });
        },
        std::time::Duration::from_secs(5),
    ) {
        on_cleanup(move || h.clear());
    }
    let busy_key = duet_common::text::title_key(&g.title);
    let busy_key2 = busy_key.clone();
    // Book stays greyed out while a download for it is on its way.
    // (One closure per use site: closures can't be shared by two owners.)
    let is_busy = move || {
        pending.with(|p| {
            p.keys().any(|(t, k)| {
                duet_common::text::title_key(t) == busy_key
                    && ((k == "ebook" && pick_e.get().is_some()) || (k == "audiobook" && pick_a.get().is_some()))
            })
        })
    };
    let is_busy2 = move || {
        pending.with(|p| {
            p.keys().any(|(t, k)| {
                duet_common::text::title_key(t) == busy_key2
                    && ((k == "ebook" && pick_e.get().is_some()) || (k == "audiobook" && pick_a.get().is_some()))
            })
        })
    };
    let get = move |_| {
        if sending.get_untracked() {
            return;
        }
        sending.set(true);
        let reqs: Vec<NewDownload> = [pick_e.get_untracked().map(|i| e2[i].clone()), pick_a.get_untracked().map(|i| a2[i].clone())]
            .into_iter()
            .flatten()
            .map(|opt| {
                let r = as_this_book(opt);
                NewDownload { kind: r.kind, title: r.title.clone(), target: r.download.clone(), author: r.author.clone(), aacid: r.aacid.clone(), torrent_url: r.torrent_url.clone() }
            })
            .collect();
        spawn_local(async move {
            let now = js_sys::Date::now() / 1000.0;
            for req in reqs {
                match api::post::<_, ()>("/api/downloads", &req).await {
                    Ok(()) => {
                        panel.opened();
                        pending.update(|p| {
                            p.insert((req.title.clone(), req.kind.as_str().to_string()), now);
                        });
                    }
                    Err(e) => crate::toast().show(e.0),
                }
            }
            sending.set(false);
        });
    };
    let label = move || {
        if sending.get() || is_busy() {
            return if is_busy() && !sending.get() { "Getting…" } else { "Adding…" };
        }
        match (pick_e.get().is_some(), pick_a.get().is_some()) {
            (true, true) => "Get ebook and audiobook",
            (true, false) => "Get ebook",
            (false, true) => "Get audiobook",
            (false, false) => "Pick an ebook or audiobook",
        }
    };
    view! {
        <article class="found-book">
            <header>
                <div>
                    <h2>{g.title.clone()}</h2>
                    {g.author.clone().map(|a| view! { <p class="author">{a}</p> })}
                </div>
                <button class="primary" disabled=move || sending.get() || is_busy2() || pick_e.get().is_none() && pick_a.get().is_none() on:click=get>
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
                                {res.aacid.clone().map(|a| view! { <code class="aacid" title="Anna's Archive content id — the file name in its bulk torrent">{a}</code> })}
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
const SOURCES: [(&str, &str); 8] = [
    ("gutenberg", "Project Gutenberg"),
    ("standard_ebooks", "Standard Ebooks"),
    ("internet_archive", "Internet Archive"),
    ("annas_archive", "Anna's Archive"),
    ("libgen", "LibGen"),
    ("librivox", "LibriVox"),
    ("audiobookbay", "Audiobook Bay"),
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
    // Anna's Archive bot check: the badge blinks yellow instead of popping
    // the browser immediately — the user opens it by clicking the badge.
    let aa_attention = RwSignal::new(false);
    let show_check = RwSignal::new(false);
    let frame_url = RwSignal::new(String::new());
    // VPN renewal: a solved check that still fails usually means the exit
    // IP is banned. Gated on configuration; offered on the page and in the
    // bot-check modal.
    let vpn_ready = RwSignal::new(false);
    let vpn_msg = RwSignal::new(None::<String>);
    let exit_ip = RwSignal::new(None::<String>);
    #[derive(serde::Deserialize)]
    struct VpnIp {
        #[serde(default)]
        ip: Option<String>,
    }
    let renew_vpn = move |_| {
        vpn_msg.set(Some("Starting VPN renewal…".into()));
        spawn_local(async move {
            api::renew_vpn_ip(move |m| vpn_msg.set(Some(m))).await;
            if let Ok(v) = api::get::<VpnIp>("/api/vpn/ip").await {
                exit_ip.set(v.ip);
            }
        });
    };
    let restart_browser = move |_| {
        vpn_msg.set(Some("Restarting browser for duet driving…".into()));
        spawn_local(async move {
            #[derive(serde::Deserialize)]
            struct RestartResult {
                #[serde(default)]
                marionette: bool,
            }
            match api::post::<_, RestartResult>("/api/browser/restart", &serde_json::json!({ "marionette": true })).await {
                Ok(_) => vpn_msg.set(Some("Browser back — duet can drive searches again.".into())),
                Err(e) => vpn_msg.set(Some(format!("Restart failed: {}", e.0))),
            }
        });
    };
    let ensure_frame_url = move || {
        if frame_url.get_untracked().is_empty() {
            // The modal is always about the current query: load that search
            // in the remote browser so the check triggers on the search page
            // itself, not the mirror homepage.
            let q = query.get_untracked();
            spawn_local(async move {
                let s = api::get::<duet_common::Settings>("/api/settings").await.ok();
                vpn_ready.set(s.as_ref().is_some_and(|s| s.vpn.enabled && !s.vpn.compose_dir.trim().is_empty()));
                let configured = s.as_ref().map(|s| if s.annas_archive.viewer_url.trim().is_empty() { s.browser_url.clone() } else { s.annas_archive.viewer_url.clone() }).unwrap_or_default();
                let url = if configured.trim().is_empty() {
                    // Same host as duet. Plain HTTP keeps the sidecar's port;
                    // HTTPS must use the TLS proxy's viewer port, otherwise
                    // the browser blocks the frame as mixed content.
                    let loc = web_sys::window().and_then(|w| w.location().href().ok()).unwrap_or_default();
                    let (proto, host) = match loc.split_once("://") {
                        Some((p, rest)) => (p, rest.split('/').next().unwrap_or("localhost").split(':').next().unwrap_or("localhost")),
                        None => ("http", "localhost"),
                    };
                    if proto == "https" {
                        format!("https://{host}:4443/")
                    } else {
                        format!("{proto}://{host}:3001/")
                    }
                } else {
                    configured
                };
                frame_url.set(url);
            });
        }
    };

    // Merges one source's answer into the results; shared by first pass,
    // retry, and the second-pass audiobook lookup.
    let handle_answer = move |i: usize, answer: crate::api::ApiResult<SearchResponse>, this: u32| {
        if generation.get_untracked() != this {
            return;
        }
        let is_aa = SOURCES.get(i).is_some_and(|(id, _)| *id == "annas_archive");
        let outcome = match answer {
            Ok(r) => {
                let n = r.results.len();
                let err = r.errors.first().cloned();
                if r.need_check {
                    aa_attention.set(true);
                    ensure_frame_url();
                } else if is_aa {
                    // A clean AA answer (even "nothing found") proves the
                    // session works — the badge must not stick around.
                    aa_attention.set(false);
                }
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
        status.update(|s| {
            if s.len() == SOURCES.len() {
                s[i].1 = Some(outcome);
            }
        });
    };

    // Every source is asked separately, so results show up as each one answers.
    // Second pass: after everything settles, if the top exact ebook match
    // still lacks an audiobook, Audiobook Bay gets ONE extra query with
    // "title author" — its engine needs the author for short titles like "It".
    let audio_tried = RwSignal::new(0u32);
    let fetch_source = move |i: usize, id: &'static str, q: String, this: u32| {        let url = format!("/api/search?q={}&source={id}", js_sys::encode_uri_component(&q));
        spawn_local(async move {
            let answer = api::get::<SearchResponse>(&url).await;
            handle_answer(i, answer, this);
            // Maybe fire the one second-pass audiobook lookup.
            if audio_tried.get_untracked() != this
                && status.with(|s| s.len() == SOURCES.len() && s.iter().all(|(_, st)| st.is_some()))
            {
                let lookup = results.with(|all| {
                    use duet_common::text::title_key;
                    let all = all.as_ref()?;
                    let qk = title_key(&q);
                    let ebook = all.results.iter().find(|r| {
                        r.kind == AssetKind::Ebook && r.author.as_ref().is_some_and(|a| !a.is_empty()) && title_key(&r.title) == qk
                    })?;
                    let author = ebook.author.clone()?;
                    if all.results.iter().any(|r| {
                        r.kind == AssetKind::Audiobook
                            && title_key(&r.title) == title_key(&ebook.title)
                            && surname_of(&r.author) == surname_of(&ebook.author)
                    }) {
                        return None;
                    }
                    Some(format!("{} {}", ebook.title, author))
                });
                if let Some(aq) = lookup {
                    audio_tried.set(this);
                    if let Some(j) = SOURCES.iter().position(|(id, _)| *id == "audiobookbay") {
                        status.update(|s| {
                            if s.len() == SOURCES.len() {
                                s[j].1 = None;
                            }
                        });
                        let url = format!("/api/search?q={}&source=audiobookbay", js_sys::encode_uri_component(&aq));
                        spawn_local(async move {
                            let answer = api::get::<SearchResponse>(&url).await;
                            handle_answer(j, answer, this);
                        });
                    }
                }
            }
        });
    };

    let search = move || {
        let q = query.get_untracked();
        if q.trim().is_empty() {
            return;
        }
        let this = generation.get_untracked() + 1;
        generation.set(this);
        aa_attention.set(false);
        show_check.set(false);
        results.set(Some(SearchResponse::default()));
        status.set(SOURCES.iter().map(|(_, label)| (*label, None)).collect());
        for (i, (id, _)) in SOURCES.iter().enumerate() {
            fetch_source(i, id, q.clone(), this);
        }
    };

    // Retry Anna's Archive alone (after the user solves its check):
    // other sources already answered and must not be re-hammered.
    // The AA answer merges into the current results; its chip updates.
    let retry_aa = move || {
        let q = query.get_untracked();
        if q.trim().is_empty() {
            return;
        }
        show_check.set(false);
        // New attempt: show searching, not the old verdict. handle_answer
        // re-sets attention only if the check is still up.
        aa_attention.set(false);
        let this = generation.get_untracked();
        let Some(i) = SOURCES.iter().position(|(id, _)| *id == "annas_archive") else { return };
        status.update(|s| {
            if s.len() == SOURCES.len() {
                s[i].1 = None;
            }
        });
        fetch_source(i, "annas_archive", q, this);
    };

    // Whether the renewal button can work (Settings → VPN configured).
    {
        let vpn_ready = vpn_ready;
        spawn_local(async move {
            if let Ok(s) = api::get::<duet_common::Settings>("/api/settings").await {
                let ready = s.vpn.enabled && !s.vpn.compose_dir.trim().is_empty();
                vpn_ready.set(ready);
                if ready {
                    if let Ok(v) = api::get::<VpnIp>("/api/vpn/ip").await {
                        exit_ip.set(v.ip);
                    }
                }
            }
        });
    }

    // Opened as #/discover/<query> (a book's "Find another edition"): search for it straight away.
    {
        let hash = web_sys::window().and_then(|w| w.location().hash().ok()).unwrap_or_default();
        let wanted = hash.trim_start_matches('#').trim_start_matches('/').strip_prefix("discover/").map(|q| {
            js_sys::decode_uri_component(q).ok().and_then(|s| s.as_string()).unwrap_or_else(|| q.to_string())
        });
        if let Some(q) = wanted.filter(|q| !q.trim().is_empty()) {
            // From partway down a book's page: start at the top.
            if let Some(w) = web_sys::window() {
                w.scroll_to_with_x_and_y(0.0, 0.0);
            }
            query.set(q);
            search();
        }
    }

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
                        if typed.get_untracked() == n && query.get_untracked().trim().chars().count() >= 2 {
                            search();
                        }
                    });
                }/>
            <Show when=searching>
                <span class="spinner"></span>
            </Show>
        </form>
        <p class="hint">"Free sources plus Anna's Archive and Audiobook Bay. If Anna's Archive shows its bot check, its badge blinks yellow — click it to open the remote browser, solve the check, then retry."</p>
        <Show when=move || vpn_ready.get()>
            <p class="hint">"Exit IP: " {move || exit_ip.get().unwrap_or_else(|| "…".to_string())} " "
                <button class="ghost" on:click=renew_vpn>"Get new VPN IP"</button>
                {move || vpn_msg.get().map(|m| view! { <span>{m}</span> })}
            </p>
        </Show>

        <Show when=move || !status.with(Vec::is_empty)>
            <div class="source-status">
                {move || status.get().into_iter().map(|(label, st)| {
                    // The bot check gets a blinking "needs attention" badge
                    // instead of a popup or an "unavailable" chip; clicking
                    // it opens the remote browser.
                    let attention = label == "Anna's Archive" && aa_attention.get();
                    let (class, text) = match &st {
                        _ if attention => ("attention", format!("{label}: needs attention")),
                        None => ("searching", format!("{label}")),
                        Some(Ok(0)) => ("none", format!("{label}: nothing")),
                        Some(Ok(n)) => ("found", format!("{label}: {n}")),
                        Some(Err(_)) => ("failed", format!("{label}: unavailable")),
                    };
                    let cls = if attention { "source-chip attention".to_string() } else { format!("source-chip {class}") };
                    let title = match st { Some(Err(e)) => e, _ => String::new() };
                    view! {
                        <span class=cls title=title role=if attention { "button" } else { "" } aria-label=if attention { Some("Anna's Archive needs a human — open the remote browser") } else { None }
                            on:click=move |_| { if attention { show_check.set(true); } }>
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
                {group(r.results, &query.get()).into_iter().map(|g| view! { <FoundBook g on_need_check=move || { aa_attention.set(true); ensure_frame_url(); }/> }).collect_view()}
            </div>
        })}
        <Show when=move || show_check.get()>
            <div class="check-modal-backdrop" on:click=move |_| show_check.set(false)>
                <div class="check-modal" on:click=|ev| ev.stop_propagation()>
                    <h2>"Anna's Archive wants a human"</h2>
                    <p class="muted">"The browser below opens straight at Anna's Archive. Search for your book there and pass the check, then press Retry. duet picks up the solved session itself and searches again."</p>
                    <p class="muted">"Tip: if you get the drag-the-segments puzzle and dragging misbehaves, turn on the viewer's game / locked-window mode — it sends raw mouse motion with no screen edges to hit — do the drag, then turn it back off. Or hit the puzzle's ↻ until you get the click-the-checkbox variant."</p>
                    {move || (!frame_url.get().is_empty()).then(|| view! {
                        <iframe class="check-frame" src=frame_url.get() title="Remote Firefox" allow="clipboard-read; clipboard-write; pointer-lock; fullscreen" allowfullscreen></iframe>
                    })}
                    <p class="muted">"Still failing after solving? Renew the exit IP below, wait for the new one, then Retry."</p>
                    <div class="row">
                        <button class="primary" on:click=move |_| retry_aa()>"I've solved it — retry Anna's Archive"</button>
                        <button class="ghost" on:click=move |_| show_check.set(false)>"Close"</button>
                        <Show when=move || vpn_ready.get()>
                            <button class="ghost" on:click=renew_vpn>"Get new VPN IP"</button>
                        </Show>
                        <button class="ghost" on:click=restart_browser title="Restarts the remote Firefox when it's stuck (fullscreen trap, blank page). Same profile and cookies.">"Restart browser"</button>
                        {move || (!frame_url.get().is_empty()).then(|| view! {
                            <a class="ghost" href=frame_url.get() target="_blank" rel="noopener">"Open in new tab"</a>
                        })}
                    </div>
                    {move || vpn_msg.get().map(|m| view! { <p class="muted">{m}</p> })}
                </div>
            </div>
        </Show>
    }
}

#[component]
fn ActivityNow() -> impl IntoView {
    let list = RwSignal::new(Vec::<crate::Activity>::new());
    let refresh = move || {
        spawn_local(async move {
            if let Ok(v) = api::get::<Vec<crate::Activity>>("/api/activity").await {
                list.set(v);
            }
        });
    };
    refresh();
    poll(2000, refresh);
    view! {
        <Show when=move || !list.with(Vec::is_empty)>
            <section class="activity-now">
                <h3>"In the background now"</h3>
                <crate::ActivityList list=list.into()/>
            </section>
        </Show>
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
        <ActivityNow/>
        {move || {
            let all = jobs.get().and_then(|r| r.ok()).unwrap_or_default();
            // A failure that a later run of the same job has replaced is history, not news.
            let list: Vec<Job> = all
                .iter()
                .enumerate()
                .filter(|(i, j)| j.status != "failed" || !all[..*i].iter().any(|n| n.kind == j.kind && n.work_id == j.work_id))
                .map(|(_, j)| j.clone())
                .collect();
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
