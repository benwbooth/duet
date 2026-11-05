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
            <input type="text" prop:value=move || s.with(get) on:change=move |ev| s.update(|x| set(x, event_target_value(&ev)))/>
            {(!hint.is_empty()).then(|| view! { <small>{hint}</small> })}
        </label>
    }
}

fn secret(s: RwSignal<Settings>, label: &'static str, get: Getter<String>, set: Setter<String>) -> impl IntoView {
    view! {
        <label class="field">
            <span>{label}</span>
            <input type="password" autocomplete="off" prop:value=move || s.with(get)
                on:change=move |ev| s.update(|x| set(x, event_target_value(&ev)))/>
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

            <section>
                <h2>"AI"</h2>
                <p class="hint">"Any OpenAI-compatible servers: llama.cpp, vLLM or Ollama for chat; speaches/faster-whisper-server for speech to text; workers/tts_server.py (Fish Audio acting, Qwen3-TTS voice design) for speech."</p>
                {text(s, "Chat model URL", "", |x| x.ai.llm_url.clone(), |x, v| x.ai.llm_url = v)}
                {text(s, "Chat model", "", |x| x.ai.llm_model.clone(), |x, v| x.ai.llm_model = v)}
                {secret(s, "API key (if needed)", |x| x.ai.llm_api_key.clone(), |x, v| x.ai.llm_api_key = v)}
                {text(s, "Speech-to-text URL", "", |x| x.ai.stt_url.clone(), |x, v| x.ai.stt_url = v)}
                {text(s, "Speech-to-text model", "Also used by the sync and transcription jobs.", |x| x.ai.stt_model.clone(), |x, v| x.ai.stt_model = v)}
                {text(s, "Text-to-speech URL", "", |x| x.ai.tts_url.clone(), |x, v| x.ai.tts_url = v)}
                {text(s, "Text-to-speech model", "", |x| x.ai.tts_model.clone(), |x, v| x.ai.tts_model = v)}
                {text(s, "Narrator voice", "Used for the plain assistant.", |x| x.ai.narrator_voice.clone(), |x, v| x.ai.narrator_voice = v)}
                {text(s, "Python for workers", "A venv with workers/requirements.txt installed.", |x| x.ai.python.clone(), |x, v| x.ai.python = v)}
                {text(s, "Workers folder", "", |x| x.ai.workers_dir.clone(), |x, v| x.ai.workers_dir = v)}
                {text(s, "Device", "auto, cuda or cpu", |x| x.ai.device.clone(), |x, v| x.ai.device = v)}
            </section>

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
