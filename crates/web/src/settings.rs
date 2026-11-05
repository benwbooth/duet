//! The settings page: library folders, downloads (qBittorrent, indexers, free sources), AI
//! services and reading preferences.

use duet_common::{Indexer, Settings, VoiceSetup};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::{api, toast};

type Getter<T> = fn(&Settings) -> T;
type Setter<T> = fn(&mut Settings, T);

fn text(s: RwSignal<Settings>, label: &'static str, hint: &'static str, get: Getter<String>, set: Setter<String>) -> impl IntoView {
    view! {
        <label class="field">
            <span>{label}</span>
            <input type="text" prop:value=move || s.with(get) on:input=move |ev| s.update(|x| set(x, event_target_value(&ev)))/>
            {(!hint.is_empty()).then(|| view! { <small>{hint}</small> })}
        </label>
    }
}

fn secret(s: RwSignal<Settings>, label: &'static str, get: Getter<String>, set: Setter<String>) -> impl IntoView {
    view! {
        <label class="field">
            <span>{label}</span>
            <input type="password" autocomplete="off" prop:value=move || s.with(get)
                on:input=move |ev| s.update(|x| set(x, event_target_value(&ev)))/>
        </label>
    }
}

fn toggle(s: RwSignal<Settings>, label: &'static str, get: Getter<bool>, set: Setter<bool>) -> impl IntoView {
    view! {
        <label class="check">
            <input type="checkbox" prop:checked=move || s.with(get) on:change=move |ev| s.update(|x| set(x, event_target_checked(&ev)))/>
            {label}
        </label>
    }
}

fn lines(v: &[String]) -> String {
    v.join("\n")
}

#[component]
pub fn SettingsPage() -> impl IntoView {
    let s = RwSignal::new(Settings::default());
    let loaded = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let toast = toast();
    spawn_local(async move {
        match api::get::<Settings>("/api/settings").await {
            Ok(v) => {
                s.set(v);
                loaded.set(true);
            }
            Err(e) => toast.show(e.0),
        }
    });
    let save = move |_| {
        saving.set(true);
        let v = s.get_untracked();
        spawn_local(async move {
            match api::put::<_, Settings>("/api/settings", &v).await {
                Ok(_) => toast.show("Settings saved"),
                Err(e) => toast.show(e.0),
            }
            saving.set(false);
        });
    };
    let test_qbit = move |_| {
        let q = s.get_untracked().qbittorrent;
        spawn_local(async move {
            match api::post::<_, String>("/api/settings/test-qbittorrent", &q).await {
                Ok(m) => toast.show(m),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let add_indexer = move |_| {
        s.update(|x| {
            x.indexers.push(Indexer {
                name: "Prowlarr".into(),
                enabled: true,
                url: "http://localhost:9696/1/api".into(),
                ebook_categories: "7000,7020".into(),
                audiobook_categories: "3030".into(),
                ..Default::default()
            })
        })
    };

    view! {
        <header class="page-head">
            <h1>"Settings"</h1>
            <button class="primary" on:click=save disabled=move || saving.get() || !loaded.get()>
                {move || if saving.get() { "Saving…" } else { "Save" }}
            </button>
        </header>
        <div class="settings">
            <VoicesSetup s/>
            <section>
                <h2>"Library"</h2>
                <label class="field">
                    <span>"Book folders"</span>
                    <textarea rows="3" placeholder="/home/you/Books\n/home/you/Audiobooks"
                        prop:value=move || s.with(|x| lines(&x.library_dirs))
                        on:change=move |ev| s.update(|x| x.library_dirs = event_target_value(&ev).lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())>
                    </textarea>
                    <small>"One per line. Ebooks and audiobooks are matched into one book by title and author."</small>
                </label>
                {text(s, "Import folder", "Downloads and generated audiobooks/ebooks go here. Empty: the data directory.",
                    |x| x.import_dir.clone(), |x, v| x.import_dir = v)}
            </section>

            <section>
                <h2>"Free sources"</h2>
                <p class="hint">"Public-domain and freely licensed books, searchable from Discover."</p>
                {toggle(s, "Project Gutenberg", |x| x.free_sources.gutenberg, |x, v| x.free_sources.gutenberg = v)}
                {toggle(s, "Standard Ebooks", |x| x.free_sources.standard_ebooks, |x, v| x.free_sources.standard_ebooks = v)}
                {toggle(s, "Internet Archive (public domain texts)", |x| x.free_sources.internet_archive, |x, v| x.free_sources.internet_archive = v)}
                {toggle(s, "LibriVox audiobooks", |x| x.free_sources.librivox, |x, v| x.free_sources.librivox = v)}
            </section>

            <section>
                <h2>"qBittorrent"</h2>
                {toggle(s, "Use qBittorrent for torrent downloads", |x| x.qbittorrent.enabled, |x, v| x.qbittorrent.enabled = v)}
                {text(s, "Web UI address", "", |x| x.qbittorrent.url.clone(), |x, v| x.qbittorrent.url = v)}
                {text(s, "Username", "", |x| x.qbittorrent.username.clone(), |x, v| x.qbittorrent.username = v)}
                {secret(s, "Password", |x| x.qbittorrent.password.clone(), |x, v| x.qbittorrent.password = v)}
                {text(s, "Category for ebooks", "", |x| x.qbittorrent.category_ebooks.clone(), |x, v| x.qbittorrent.category_ebooks = v)}
                {text(s, "Category for audiobooks", "", |x| x.qbittorrent.category_audiobooks.clone(), |x, v| x.qbittorrent.category_audiobooks = v)}
                {toggle(s, "Import finished downloads automatically", |x| x.qbittorrent.auto_import, |x, v| x.qbittorrent.auto_import = v)}
                <button on:click=test_qbit>"Test connection"</button>
            </section>

            <section>
                <h2>"Indexers"</h2>
                <p class="hint">"Torznab indexers, e.g. from Prowlarr or Jackett. Results appear in Discover and download through qBittorrent."</p>
                {move || (0..s.with(|x| x.indexers.len())).map(|i| {
                    let field = move |label: &'static str, get: fn(&Indexer) -> String, set: fn(&mut Indexer, String), password: bool| view! {
                        <label class="field">
                            <span>{label}</span>
                            <input type=if password { "password" } else { "text" }
                                prop:value=move || s.with(|x| x.indexers.get(i).map(get).unwrap_or_default())
                                on:change=move |ev| s.update(|x| if let Some(ix) = x.indexers.get_mut(i) { set(ix, event_target_value(&ev)) })/>
                        </label>
                    };
                    view! {
                        <div class="indexer">
                            <label class="check">
                                <input type="checkbox" prop:checked=move || s.with(|x| x.indexers.get(i).is_some_and(|ix| ix.enabled))
                                    on:change=move |ev| s.update(|x| if let Some(ix) = x.indexers.get_mut(i) { ix.enabled = event_target_checked(&ev) })/>
                                "Enabled"
                            </label>
                            {field("Name", |ix| ix.name.clone(), |ix, v| ix.name = v, false)}
                            {field("Torznab URL", |ix| ix.url.clone(), |ix, v| ix.url = v, false)}
                            {field("API key", |ix| ix.api_key.clone(), |ix, v| ix.api_key = v, true)}
                            {field("Ebook categories", |ix| ix.ebook_categories.clone(), |ix, v| ix.ebook_categories = v, false)}
                            {field("Audiobook categories", |ix| ix.audiobook_categories.clone(), |ix, v| ix.audiobook_categories = v, false)}
                            <button class="danger" on:click=move |_| s.update(|x| { if i < x.indexers.len() { x.indexers.remove(i); } })>"Remove"</button>
                        </div>
                    }
                }).collect_view()}
                <button on:click=add_indexer>"Add indexer"</button>
            </section>

            <AiSection s/>

            <section>
                <h2>"Reading"</h2>
                <label class="field">
                    <span>"Theme"</span>
                    <select on:change=move |ev| s.update(|x| x.reader.theme = event_target_value(&ev))>
                        {["auto", "light", "sepia", "dark"].map(|t| view! {
                            <option value=t selected=move || s.with(|x| x.reader.theme == t)>{t}</option>
                        })}
                    </select>
                </label>
                {text(s, "Font", "", |x| x.reader.font.clone(), |x, v| x.reader.font = v)}
                <label class="field">
                    <span>"Text size"</span>
                    <input type="number" min="12" max="32" prop:value=move || s.with(|x| x.reader.font_size.to_string())
                        on:change=move |ev| s.update(|x| if let Ok(v) = event_target_value(&ev).parse() { x.reader.font_size = v })/>
                </label>
                <label class="field">
                    <span>"Default speed"</span>
                    <input type="number" min="0.5" max="3" step="0.05" prop:value=move || s.with(|x| x.reader.playback_rate.to_string())
                        on:change=move |ev| s.update(|x| if let Ok(v) = event_target_value(&ev).parse() { x.reader.playback_rate = v })/>
                </label>
                {toggle(s, "Follow the audio (scroll and turn chapters)", |x| x.reader.follow_audio, |x, v| x.reader.follow_audio = v)}
            </section>
        </div>
    }
}

/// Installs and runs the local voice stack: Fish Audio S2-Pro for acting, Qwen3-TTS for casting.
#[component]
fn VoicesSetup(s: RwSignal<Settings>) -> impl IntoView {
    let status = RwSignal::new(None::<VoiceSetup>);
    let busy = RwSignal::new(false);
    let show_log = RwSignal::new(false);
    let server_log = RwSignal::new(Vec::<String>::new());
    let toast = toast();
    let refresh = move || {
        spawn_local(async move {
            if let Ok(v) = api::get::<VoiceSetup>("/api/setup/voices").await {
                status.set(Some(v));
            }
            if show_log.get_untracked() {
                if let Ok(l) = api::get::<Vec<String>>("/api/setup/voices/log").await {
                    server_log.set(l);
                }
            }
        })
    };
    refresh();
    if let Ok(h) = set_interval_with_handle(refresh, std::time::Duration::from_secs(2)) {
        on_cleanup(move || h.clear());
    }
    let act = move |path: &'static str| {
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, VoiceSetup>(path, &()).await {
                Ok(v) => status.set(Some(v)),
                Err(e) => toast.show(e.0),
            }
            busy.set(false);
        });
    };
    let item = |done: bool, label: &'static str| view! {
        <li class:done=done><span class="mark">{if done { "✓" } else { "○" }}</span>{label}</li>
    };

    view! {
        <section class="voices">
            <h2>"Voices"</h2>
            <p class="hint">"Character voices run on this computer: Fish Audio S2-Pro performs every line, and Qwen3-TTS VoiceDesign creates each character's voice from its description."</p>
            {move || status.get().map(|v| {
                let ready = v.ready();
                let small_gpu = v.gpu_vram_gb.is_some_and(|g| g < 24);
                view! {
                    <p class="gpu">{v.gpu.clone().map(|g| format!("Graphics card: {g}")).unwrap_or_else(|| "No graphics card found".into())}</p>
                    {small_gpu.then(|| view! {
                        <p class="warn">"S2-Pro needs about 24 GB of graphics memory; this card may be too small."</p>
                    })}
                    <ul class="setup-steps">
                        {item(v.fish_installed, "Fish Audio speech engine")}
                        {item(v.fish_weights, "S2-Pro acting model")}
                        {item(v.workers_installed, "duet workers (speech recognition, casting)")}
                        {item(v.casting_weights, "Qwen3-TTS voice design models")}
                    </ul>
                    {v.installing.then(|| view! {
                        <div class="install-progress">
                            <div class="progress"><div style=format!("width:{:.0}%", v.progress * 100.0)></div></div>
                            <span>{v.step.clone()}</span>
                        </div>
                    })}
                    {v.error.clone().map(|e| view! { <p class="warn">{e}</p> })}
                    {(v.installing || v.error.is_some()).then(|| view! {
                        <pre class="log">{v.log.join("\n")}</pre>
                    })}
                    <div class="row">
                        {(!ready && !v.installing).then(|| view! {
                            <button class="primary" disabled=move || busy.get() on:click=move |_| act("/api/setup/voices/install")>
                                {if v.fish_installed || v.workers_installed { "Finish installing" } else { "Install voices (downloads tens of GB)" }}
                            </button>
                        })}
                        {(ready && !v.installing).then(|| if v.server_running {
                            view! {
                                <span class="badge sync">"Voice server running"</span>
                                <button disabled=move || busy.get() on:click=move |_| act("/api/setup/voices/stop")>"Stop"</button>
                            }.into_any()
                        } else {
                            view! {
                                <span class="badge">"Voice server stopped"</span>
                                <button class="primary" disabled=move || busy.get() on:click=move |_| act("/api/setup/voices/start")>"Start"</button>
                            }.into_any()
                        })}
                        {ready.then(|| view! {
                            <button on:click=move |_| { show_log.update(|s| *s = !*s); refresh() }>
                                {move || if show_log.get() { "Hide log" } else { "Show log" }}
                            </button>
                        })}
                    </div>
                    <Show when=move || show_log.get()>
                        <pre class="log">{move || server_log.get().join("\n")}</pre>
                    </Show>
                }
            })}
            {toggle(s, "Start the voice server with duet", |x| x.ai.auto_start_voices, |x, v| x.ai.auto_start_voices = v)}
        </section>
    }
}

const CHAT_PRESETS: [(&str, &str); 4] = [
    ("Ollama", "http://127.0.0.1:11434/v1"),
    ("LM Studio", "http://127.0.0.1:1234/v1"),
    ("llama.cpp server", "http://127.0.0.1:8080/v1"),
    ("vLLM", "http://127.0.0.1:8000/v1"),
];

const WHISPER_MODELS: [(&str, &str); 4] = [
    ("large-v3-turbo", "Large v3 Turbo: accurate and reasonably quick (recommended)"),
    ("large-v3", "Large v3: most accurate, slowest"),
    ("medium", "Medium: quicker, a little less accurate"),
    ("small", "Small: fastest, for slow computers"),
];

/// "http://localhost:11434/v1/" and "http://127.0.0.1:11434/v1" are the same server.
fn same_host(url: &str) -> String {
    url.trim().trim_end_matches('/').replace("://localhost", "://127.0.0.1")
}

/// The likeliest general chat model in a server's list: not an OCR, embedding, translation or
/// speech model, and the biggest of the rest ("qwen3.5:9b" over "qwen3.5:4b").
fn best_chat_model(models: &[String]) -> Option<String> {
    const SPECIALISED: [&str; 7] = ["ocr", "embed", "translate", "whisper", "tts", "rerank", "guard"];
    let size = |m: &str| -> f64 {
        m.to_lowercase()
            .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'b'))
            .filter_map(|p| p.strip_suffix('b')?.parse::<f64>().ok())
            .fold(0.0, f64::max)
    };
    models
        .iter()
        .filter(|m| !SPECIALISED.iter().any(|w| m.to_lowercase().contains(w)))
        .max_by(|a, b| size(a).total_cmp(&size(b)))
        .or_else(|| models.first())
        .cloned()
}

/// The chat model, speech recognition and the companion's voice, in plain terms; the plumbing
/// lives under Advanced.
#[component]
fn AiSection(s: RwSignal<Settings>) -> impl IntoView {
    let models = RwSignal::new(Vec::<String>::new());
    let check = RwSignal::new(None::<Result<usize, String>>);
    let checking = RwSignal::new(false);
    let probe = move || {
        let (url, key) = s.with_untracked(|x| (x.ai.llm_url.clone(), x.ai.llm_api_key.clone()));
        if url.trim().is_empty() {
            return;
        }
        checking.set(true);
        spawn_local(async move {
            match api::post::<_, Vec<String>>("/api/ai/models", &serde_json::json!({ "url": url, "key": key })).await {
                Ok(list) => {
                    check.set(Some(Ok(list.len())));
                    // Pick something sensible when the saved model isn't there.
                    if !list.contains(&s.with_untracked(|x| x.ai.llm_model.clone())) {
                        if let Some(best) = best_chat_model(&list) {
                            s.update(|x| x.ai.llm_model = best);
                        }
                    }
                    models.set(list);
                }
                Err(e) => {
                    models.set(Vec::new());
                    check.set(Some(Err(e.0)));
                }
            }
            checking.set(false);
        });
    };
    // Check once the saved settings have loaded, and whenever the address changes.
    Effect::new(move |prev: Option<String>| {
        let url = s.with(|x| x.ai.llm_url.clone());
        if prev.as_deref() != Some(url.as_str()) {
            probe();
        }
        url
    });
    let preset = move || {
        let url = same_host(&s.with(|x| x.ai.llm_url.clone()));
        CHAT_PRESETS.iter().find(|(_, u)| same_host(u) == url).map(|(n, _)| n.to_string()).unwrap_or_else(|| "other".into())
    };

    view! {
        <section class="ai">
            <h2>"AI"</h2>
            <p class="hint">"Everything runs on this computer. A chat model answers your questions, directs the full-cast recordings and tidies up transcripts. Voices and speech recognition come from the Voices section above."</p>

            <h3>"Chat model"</h3>
            <label class="field">
                <span>"Runs in"</span>
                <select on:change=move |ev| {
                    let v = event_target_value(&ev);
                    if let Some((_, url)) = CHAT_PRESETS.iter().find(|(n, _)| *n == v) {
                        s.update(|x| x.ai.llm_url = url.to_string());
                    }
                }>
                    {CHAT_PRESETS.map(|(name, _)| view! { <option value=name selected=move || preset() == name>{name}</option> })}
                    <option value="other" selected=move || preset() == "other">"Somewhere else"</option>
                </select>
            </label>
            <label class="field">
                <span>"Address"</span>
                <input type="text" prop:value=move || s.with(|x| x.ai.llm_url.clone())
                    on:change=move |ev| s.update(|x| x.ai.llm_url = event_target_value(&ev))/>
            </label>
            <div class="field">
                <span>"Model"</span>
                {move || {
                    let list = models.get();
                    if list.is_empty() {
                        view! {
                            <input type="text" placeholder="e.g. qwen3:14b" prop:value=move || s.with(|x| x.ai.llm_model.clone())
                                on:input=move |ev| s.update(|x| x.ai.llm_model = event_target_value(&ev))/>
                        }.into_any()
                    } else {
                        view! {
                            <select on:change=move |ev| s.update(|x| x.ai.llm_model = event_target_value(&ev))>
                                {list.into_iter().map(|m| {
                                    let (value, label, current) = (m.clone(), m.clone(), m);
                                    view! { <option value=value selected=move || s.with(|x| x.ai.llm_model == current)>{label}</option> }
                                }).collect_view()}
                            </select>
                        }.into_any()
                    }
                }}
                <div class="status-line">
                    {move || match check.get() {
                        _ if checking.get() => view! { <span class="muted">"Checking…"</span> }.into_any(),
                        Some(Ok(n)) => view! { <span class="ok">{format!("✓ Connected, {n} model{} available", if n == 1 { "" } else { "s" })}</span> }.into_any(),
                        Some(Err(e)) => view! { <span class="warn">{format!("Not reachable: {e}. Is it running?")}</span> }.into_any(),
                        None => view! { <span></span> }.into_any(),
                    }}
                    <button class="small" on:click=move |_| probe()>"Check again"</button>
                </div>
                <small>"A model around 14B parameters is a good fit next to the voices on a 24 GB card."</small>
            </div>

            <h3>"Speech recognition"</h3>
            <label class="field">
                <span>"Accuracy"</span>
                <select on:change=move |ev| s.update(|x| x.ai.stt_model = event_target_value(&ev))>
                    {WHISPER_MODELS.map(|(id, label)| view! {
                        <option value=id selected=move || s.with(|x| x.ai.stt_model == id)>{label}</option>
                    })}
                </select>
                <small>"Used for the microphone, for syncing books with their audiobooks, and for turning audiobooks into ebooks."</small>
            </label>

            <h3>"Reading companion's voice"</h3>
            <label class="field">
                <span>"Describe it"</span>
                <textarea rows="2" placeholder="e.g. A calm Scottish woman in her forties, warm and a little wry"
                    prop:value=move || s.with(|x| x.ai.assistant_voice.clone())
                    on:input=move |ev| s.update(|x| x.ai.assistant_voice = event_target_value(&ev))></textarea>
                <small>"The voice is created from this description the first time it speaks. Characters get their own voices from the book."</small>
            </label>

            <details class="advanced">
                <summary>"Advanced"</summary>
                {secret(s, "Chat model API key (only if your server needs one)", |x| x.ai.llm_api_key.clone(), |x, v| x.ai.llm_api_key = v)}
                {text(s, "Speech recognition address", "", |x| x.ai.stt_url.clone(), |x, v| x.ai.stt_url = v)}
                {text(s, "Voice server address", "", |x| x.ai.tts_url.clone(), |x, v| x.ai.tts_url = v)}
                {text(s, "Voice name for the companion", "Change it to recast the voice from a new description.", |x| x.ai.narrator_voice.clone(), |x, v| x.ai.narrator_voice = v)}
                {text(s, "Python for background jobs", "Set automatically when the voices are installed.", |x| x.ai.python.clone(), |x, v| x.ai.python = v)}
                {text(s, "Workers folder", "", |x| x.ai.workers_dir.clone(), |x, v| x.ai.workers_dir = v)}
                {text(s, "Device for jobs", "auto, cuda or cpu", |x| x.ai.device.clone(), |x, v| x.ai.device = v)}
            </details>
        </section>
    }
}
