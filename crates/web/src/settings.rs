//! The settings page: library folders, downloads (qBittorrent, indexers, free sources), AI
//! services and reading preferences.

use duet_common::{ChatSetup, ChatTest, DetectedServer, FolderListing, Indexer, Settings, VoiceSetup};
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

/// The settings, loaded from the server and saved automatically shortly after each change
/// (with a "Saved" toast), and on leaving the page.
pub fn settings_store() -> (RwSignal<Settings>, RwSignal<bool>) {
    let s = RwSignal::new(Settings::default());
    let loaded = RwSignal::new(false);
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

    let edits = RwSignal::new(0u64);
    let saved = RwSignal::new(0u64);
    let save_now = move || {
        let n = edits.get_untracked();
        if n == saved.get_untracked() {
            return;
        }
        saved.set(n);
        let v = s.get_untracked();
        spawn_local(async move {
            match api::put::<_, Settings>("/api/settings", &v).await {
                Ok(_) => toast.show("Saved"),
                Err(e) => toast.show(format!("Couldn't save: {}", e.0)),
            }
        });
    };
    Effect::new(move |seen: Option<bool>| {
        s.track();
        let ready = loaded.get();
        // The first run after loading is the loaded settings themselves, not an edit.
        if ready && seen == Some(true) {
            let n = edits.get_untracked() + 1;
            edits.set(n);
            spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(700).await;
                if edits.get_untracked() == n {
                    save_now();
                }
            });
        }
        ready
    });
    on_cleanup(save_now);
    (s, loaded)
}

#[component]
pub fn SettingsPage() -> impl IntoView {
    let (s, _) = settings_store();
    let toast = toast();
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
            <span class="hint">"Changes save automatically."</span>
            <a class="button" href="#/setup">"Setup guide"</a>
        </header>
        <div class="settings">
            <VoicesSetup s/>
            <section>
                <h2>"Library"</h2>
                <div class="field">
                    <span>"Book folders"</span>
                    <FolderPicker s/>
                    <small>"Ebooks and audiobooks are matched into one book by title and author."</small>
                </div>
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
pub fn VoicesSetup(s: RwSignal<Settings>) -> impl IntoView {
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
    view! {
        <section class="ai">
            <h2>"AI"</h2>
            <p class="hint">"Everything runs on this computer. A chat model answers your questions, directs the full-cast recordings and tidies up transcripts. Voices and speech recognition come from the Voices section above."</p>

            <ChatModelSetup s/>

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
                {text(s, "Chat model server address", "Filled in when you pick a server above.", |x| x.ai.llm_url.clone(), |x, v| x.ai.llm_url = v)}
                {text(s, "Chat model name", "", |x| x.ai.llm_model.clone(), |x, v| x.ai.llm_model = v)}
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

/// Models worth suggesting, best first for a 24 GB card that also runs the voices.
const SUGGESTED: [(&str, &str, f64, &str); 3] = [
    ("qwen3.5:9b", "Recommended", 6.6, "Good, quick answers and a capable casting director. Leaves room on the graphics card for the voices."),
    ("qwen3.5:27b", "Smartest", 17.0, "Noticeably better at characters and long books. Uses most of a 24 GB card, so it runs slower while the voice server is on."),
    ("qwen3.5:4b", "Lightest", 3.4, "For slower computers. Fine for definitions and quick questions; weaker at casting."),
];

/// Walks through getting a chat model running: a model server, a model, and a test question.
#[component]
pub fn ChatModelSetup(s: RwSignal<Settings>) -> impl IntoView {
    let setup = RwSignal::new(None::<ChatSetup>);
    let scanning = RwSignal::new(false);
    let testing = RwSignal::new(false);
    let test = RwSignal::new(None::<Result<ChatTest, String>>);
    let toast = toast();

    let scan = move || {
        scanning.set(true);
        spawn_local(async move {
            if let Ok(v) = api::get::<ChatSetup>("/api/ai/setup").await {
                // Pick a running server and a sensible model if nothing usable is chosen yet.
                let url = s.with_untracked(|x| same_host(&x.ai.llm_url));
                let current = v.servers.iter().find(|sv| sv.running && same_host(&sv.url) == url);
                let chosen = current.or_else(|| v.servers.iter().find(|sv| sv.running)).cloned();
                if let Some(sv) = chosen {
                    let names: Vec<String> = sv.models.iter().map(|m| m.name.clone()).collect();
                    let model = s.with_untracked(|x| x.ai.llm_model.clone());
                    if same_host(&sv.url) != url || !names.contains(&model) {
                        s.update(|x| {
                            x.ai.llm_url = sv.url.clone();
                            if !names.contains(&x.ai.llm_model) {
                                if let Some(best) = best_chat_model(&names) {
                                    x.ai.llm_model = best;
                                }
                            }
                        });
                    }
                }
                setup.set(Some(v));
            }
            scanning.set(false);
        });
    };
    scan();
    // Follow a model download until it finishes.
    if let Ok(h) = set_interval_with_handle(
        move || {
            if setup.get_untracked().and_then(|v| v.pull).is_some_and(|p| !p.done) {
                scan();
            }
        },
        std::time::Duration::from_secs(2),
    ) {
        on_cleanup(move || h.clear());
    }

    let selected = move || -> Option<DetectedServer> {
        let url = s.with(|x| same_host(&x.ai.llm_url));
        setup.get()?.servers.into_iter().find(|sv| sv.running && same_host(&sv.url) == url)
    };
    let download = move |model: &'static str| {
        let Some(sv) = selected() else { return };
        spawn_local(async move {
            match api::post::<_, ChatSetup>("/api/ai/pull", &serde_json::json!({ "url": sv.url, "model": model })).await {
                Ok(v) => setup.set(Some(v)),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let run_test = move |_| {
        testing.set(true);
        test.set(None);
        spawn_local(async move {
            // Make sure the latest choice is saved before the server uses it.
            let _ = api::put::<_, Settings>("/api/settings", &s.get_untracked()).await;
            test.set(Some(api::post::<_, ChatTest>("/api/ai/test", &()).await.map_err(|e| e.0)));
            testing.set(false);
        });
    };

    view! {
        <div class="wizard">
            <h3>"Chat model"</h3>

            <div class="wizard-step">
                <div class="step-title"><span class="num">"1"</span>"A program that runs the model"</div>
                {move || match setup.get() {
                    None => view! { <p class="muted">"Looking for model servers on this computer…"</p> }.into_any(),
                    Some(v) => {
                        let running: Vec<DetectedServer> = v.servers.iter().filter(|sv| sv.running).cloned().collect();
                        if running.is_empty() {
                            let package = match v.gpu.as_deref() {
                                Some(g) if g.starts_with("NVIDIA") => "pkgs.ollama-cuda",
                                Some(g) if g.starts_with("AMD") => "pkgs.ollama-rocm",
                                _ => "pkgs.ollama",
                            };
                            view! {
                                <p>"duet needs a model server running on this computer. We recommend "<strong>"Ollama"</strong>": it's free, uses your graphics card, and duet can download models into it for you."</p>
                                {if v.nixos {
                                    view! {
                                        <p>"On NixOS, add this to your configuration and run "<code>"sudo nixos-rebuild switch"</code>":"</p>
                                        <pre class="code">{format!("services.ollama = {{\n  enable = true;\n  package = {package};\n}};")}</pre>
                                    }.into_any()
                                } else {
                                    view! {
                                        <p>"Install it from "<a href="https://ollama.com/download" target="_blank" rel="noopener">"ollama.com/download"</a>" and start it."</p>
                                    }.into_any()
                                }}
                                <p class="hint">"Already use LM Studio, llama.cpp or vLLM? Start its server and press Check again; you can also enter its address under Advanced."</p>
                            }.into_any()
                        } else {
                            view! {
                                <div class="choices">
                                    {running.into_iter().map(|sv| {
                                        let url = sv.url.clone();
                                        let key = same_host(&url);
                                        let is_on = move || s.with(|x| same_host(&x.ai.llm_url) == key);
                                        let is_on2 = is_on.clone();
                                        view! {
                                            <label class="choice" class:on=is_on>
                                                <input type="radio" name="llm-server" prop:checked=is_on2
                                                    on:change=move |_| s.update(|x| x.ai.llm_url = url.clone())/>
                                                <span><strong>{sv.name.clone()}</strong>" is running"</span>
                                                <span class="muted">{format!("{} model{}", sv.models.len(), if sv.models.len() == 1 { "" } else { "s" })}</span>
                                            </label>
                                        }
                                    }).collect_view()}
                                </div>
                            }.into_any()
                        }
                    }
                }}
                <button class="small" disabled=move || scanning.get() on:click=move |_| scan()>
                    {move || if scanning.get() { "Checking…" } else { "Check again" }}
                </button>
            </div>

            <Show when=move || selected().is_some()>
                <div class="wizard-step">
                    <div class="step-title"><span class="num">"2"</span>"Choose a model"</div>
                    {move || {
                        let sv = selected()?;
                        let v = setup.get()?;
                        let installed: Vec<String> = sv.models.iter().map(|m| m.name.clone()).collect();
                        let can_download = sv.kind == "ollama";
                        let vram = v.gpu_vram_gb.unwrap_or(0) as f64;
                        let pull = v.pull.clone();
                        Some(view! {
                            {v.gpu.clone().map(|g| view! { <p class="muted">{format!("Your graphics card: {g}. The voices need most of it while they're running, so a model that leaves room is faster.")}</p> })}
                            <div class="model-cards">
                                {SUGGESTED.map(|(name, tag, size, about)| {
                                    let have = installed.iter().any(|m| m == name);
                                    let too_big = vram > 0.0 && size > vram * 0.9;
                                    let pulling = pull.as_ref().filter(|p| p.model == name).cloned();
                                    let is_on = move || s.with(|x| x.ai.llm_model == name);
                                    view! {
                                        <div class="model-card" class:on=is_on>
                                            <div class="model-head">
                                                <strong>{name}</strong>
                                                <span class="badge">{tag}</span>
                                                <span class="muted">{format!("about {size} GB")}</span>
                                            </div>
                                            <p>{about}</p>
                                            {too_big.then(|| view! { <p class="warn">"Bigger than your graphics card; it would run mostly on the processor and be slow."</p> })}
                                            {match pulling {
                                                Some(p) if !p.done => view! {
                                                    <div class="install-progress">
                                                        <div class="progress"><div style=format!("width:{:.0}%", p.progress * 100.0)></div></div>
                                                        <span>{format!("{} ({:.0}%)", p.status, p.progress * 100.0)}</span>
                                                    </div>
                                                }.into_any(),
                                                Some(p) if p.error.is_some() => view! { <p class="warn">{format!("Download failed: {}", p.error.unwrap_or_default())}</p> }.into_any(),
                                                _ if have => view! {
                                                    <button class:primary=move || !is_on() on:click=move |_| s.update(|x| x.ai.llm_model = name.to_string())>
                                                        {move || if is_on() { "✓ In use" } else { "Use this model" }}
                                                    </button>
                                                }.into_any(),
                                                _ if can_download => view! {
                                                    <button on:click=move |_| download(name)>{format!("Download ({size} GB)")}</button>
                                                }.into_any(),
                                                _ => view! { <p class="muted">"Download it in your model server, then press Check again."</p> }.into_any(),
                                            }}
                                        </div>
                                    }
                                })}
                            </div>
                            {(!installed.is_empty()).then(|| view! {
                                <label class="field">
                                    <span>"Or use another installed model"</span>
                                    <select on:change=move |ev| s.update(|x| x.ai.llm_model = event_target_value(&ev))>
                                        {installed.iter().map(|m| {
                                            let (value, label, current) = (m.clone(), m.clone(), m.clone());
                                            view! { <option value=value selected=move || s.with(|x| x.ai.llm_model == current)>{label}</option> }
                                        }).collect_view()}
                                    </select>
                                </label>
                            })}
                        })
                    }}
                </div>

                <div class="wizard-step">
                    <div class="step-title"><span class="num">"3"</span>"Try it"</div>
                    <p class="muted">{move || format!("Asks {} a quick question to check it works and how fast it is. The first answer takes longer while the model loads.", s.with(|x| x.ai.llm_model.clone()))}</p>
                    <button class="primary" disabled=move || testing.get() on:click=run_test>
                        {move || if testing.get() { "Waiting for an answer…" } else { "Ask a test question" }}
                    </button>
                    {move || test.get().map(|r| match r {
                        Ok(t) => {
                            // A token is roughly three quarters of a word.
                            let verdict = match t.tokens_per_second.map(|tps| tps * 0.75) {
                                Some(wps) if wps >= 10.0 => format!("About {wps:.0} words a second: quick enough for conversation."),
                                Some(wps) => format!("About {wps:.0} words a second: usable, but a smaller model would feel snappier."),
                                None => format!("Answered in {:.1} s.", t.seconds),
                            };
                            view! {
                                <div class="test-result ok-box">
                                    <p>"✓ "{t.answer}</p>
                                    <small>{verdict}</small>
                                </div>
                            }.into_any()
                        }
                        Err(e) => view! { <div class="test-result warn">{format!("That didn't work: {e}")}</div> }.into_any(),
                    })}
                </div>
            </Show>
        </div>
    }
}

/// The chosen book folders, plus a browser for adding more without typing paths.
#[component]
pub fn FolderPicker(s: RwSignal<Settings>) -> impl IntoView {
    let browsing = RwSignal::new(false);
    let listing = RwSignal::new(None::<FolderListing>);
    let toast = toast();
    let open = move |path: Option<String>| {
        spawn_local(async move {
            let url = match &path {
                Some(p) => format!("/api/folders?path={}", js_sys::encode_uri_component(p)),
                None => "/api/folders".into(),
            };
            match api::get::<FolderListing>(&url).await {
                Ok(l) => listing.set(Some(l)),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let add = move |path: String| {
        s.update(|x| {
            if !x.library_dirs.contains(&path) {
                x.library_dirs.push(path);
            }
        });
        browsing.set(false);
    };
    let books = |n: usize| match n {
        0 => String::new(),
        n if n >= 2000 => "2000+ books here".into(),
        1 => "1 book here".into(),
        n => format!("{n} books here"),
    };

    view! {
        <div class="folders">
            {move || {
                let dirs = s.with(|x| x.library_dirs.clone());
                if dirs.is_empty() {
                    view! { <p class="muted">"No folders yet."</p> }.into_any()
                } else {
                    view! {
                        <ul class="folder-list">
                            {dirs.into_iter().map(|d| {
                                let d2 = d.clone();
                                view! {
                                    <li>
                                        <span class="path">{d}</span>
                                        <button class="small" on:click=move |_| s.update(|x| x.library_dirs.retain(|p| *p != d2))>"Remove"</button>
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    }.into_any()
                }
            }}
            <Show when=move || !browsing.get()>
                <button on:click=move |_| { browsing.set(true); open(None) }>"Add a folder…"</button>
            </Show>
            <Show when=move || browsing.get()>
                <div class="browser">
                    {move || listing.get().map(|l| {
                        let here = l.path.clone();
                        view! {
                            {(!l.suggestions.is_empty()).then(|| view! {
                                <div class="suggestions">
                                    <span class="muted">"Found on this computer:"</span>
                                    {l.suggestions.iter().map(|f| {
                                        let p = f.path.clone();
                                        view! { <button class="small" on:click=move |_| add(p.clone())>{format!("+ {}", f.name)}</button> }
                                    }).collect_view()}
                                </div>
                            })}
                            <div class="browser-head">
                                {l.parent.clone().map(|p| view! { <button class="small" on:click=move |_| open(Some(p.clone()))>"↑ Up"</button> })}
                                <span class="path">{l.path.clone()}</span>
                            </div>
                            <ul class="browser-list">
                                {l.folders.iter().map(|f| {
                                    let p = f.path.clone();
                                    view! {
                                        <li>
                                            <a href="#" on:click=move |ev| { ev.prevent_default(); open(Some(p.clone())) }>{format!("📁 {}", f.name)}</a>
                                            <span class="muted">{books(f.books)}</span>
                                        </li>
                                    }
                                }).collect_view()}
                                {l.folders.is_empty().then(|| view! { <li class="muted">"No folders inside."</li> })}
                            </ul>
                            <div class="row">
                                <button class="primary" on:click=move |_| add(here.clone())>"Use this folder"</button>
                                <button on:click=move |_| browsing.set(false)>"Cancel"</button>
                            </div>
                        }
                    })}
                </div>
            </Show>
        </div>
    }
}
