//! The first-run guide: book folders, the chat model, voices and downloads, one step at a time.

use duet_common::{ScanReport, Settings};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::{api, go, settings, toast};

const STEPS: [(&str, &str); 5] = [
    ("Welcome", "What duet does"),
    ("Your books", "Where your ebooks and audiobooks are"),
    ("Chat model", "Answers questions and directs recordings"),
    ("Voices", "Character voices and speech recognition"),
    ("Downloads", "Optional: qBittorrent"),
];

#[component]
pub fn SetupPage() -> impl IntoView {
    let (s, loaded) = settings::settings_store();
    let step = RwSignal::new(0usize);
    let scan = RwSignal::new(None::<ScanReport>);
    let scanning = RwSignal::new(false);
    let toast = toast();

    let run_scan = move |_| {
        scanning.set(true);
        spawn_local(async move {
            // Save the folders first; the scan reads them from the server's settings.
            let _ = api::put::<_, Settings>("/api/settings", &s.get_untracked()).await;
            match api::post::<_, ScanReport>("/api/library/scan", &()).await {
                Ok(r) => scan.set(Some(r)),
                Err(e) => toast.show(e.0),
            }
            scanning.set(false);
        });
    };
    let finish = move || {
        s.update(|x| x.setup_done = true);
        let v = s.get_untracked();
        spawn_local(async move {
            let _ = api::put::<_, Settings>("/api/settings", &v).await;
            go("/");
        });
    };
    let next = move |_| {
        if step.get_untracked() + 1 >= STEPS.len() {
            finish();
        } else {
            step.update(|n| *n += 1);
            let _ = window().scroll_to_with_x_and_y(0.0, 0.0);
        }
    };

    view! {
        <div class="setup">
            <aside class="setup-steps-nav">
                <h1>"Get started"</h1>
                <ol>
                    {STEPS.iter().enumerate().map(|(i, (title, sub))| view! {
                        <li class:current=move || step.get() == i class:past=move || i < step.get()
                            on:click=move |_| step.set(i)>
                            <span class="num">{move || if i < step.get() { "✓".to_string() } else { (i + 1).to_string() }}</span>
                            <span><strong>{*title}</strong><small>{*sub}</small></span>
                        </li>
                    }).collect_view()}
                </ol>
                <button class="link" on:click=move |_| finish()>"Skip setup for now"</button>
            </aside>

            <div class="setup-body">
                <Show when=move || loaded.get() fallback=|| view! { <p class="muted">"Loading…"</p> }>
                    {move || match step.get() {
                        0 => view! {
                            <h2>"Welcome to duet"</h2>
                            <p>"duet keeps your ebooks and audiobooks together and plays them in sync: the sentence being read is highlighted, and tapping a sentence jumps the audio there."</p>
                            <p>"It can also make a full-cast audiobook from any ebook, make an ebook from an audiobook, and answer questions about what you're reading, or let you talk to the characters in their own voices."</p>
                            <p>"Everything runs on this computer. This guide takes a few minutes; you can skip any step and come back from Settings."</p>
                        }.into_any(),
                        1 => view! {
                            <h2>"Where are your books?"</h2>
                            <p>"Add the folders that hold your ebooks (EPUB, PDF, MOBI…) and audiobooks (a folder of tracks, or M4B files). They can be the same folder; duet pairs up an ebook and its audiobook by title and author."</p>
                            <settings::FolderPicker s/>
                            <div class="row">
                                <button class="primary" disabled=move || scanning.get() || s.with(|x| x.library_dirs.is_empty()) on:click=run_scan>
                                    {move || if scanning.get() { "Looking for books…" } else { "Find my books" }}
                                </button>
                            </div>
                            {move || scan.get().map(|r| view! {
                                <p class="ok">{format!("✓ Found {} books: {} ebooks and {} audiobooks.", r.works, r.ebooks, r.audiobooks)}</p>
                                {(!r.errors.is_empty()).then(|| view! { <p class="warn">{format!("{} files couldn't be read.", r.errors.len())}</p> })}
                            })}
                            <p class="hint">"No books yet? That's fine: Discover finds free classics from Project Gutenberg, Standard Ebooks and LibriVox."</p>
                        }.into_any(),
                        2 => view! {
                            <h2>"The chat model"</h2>
                            <p>"A language model running on this computer powers the reading companion (questions, definitions, talking to characters), directs full-cast recordings, and tidies up transcripts."</p>
                            <settings::ChatModelSetup s/>
                        }.into_any(),
                        3 => view! {
                            <h2>"Voices"</h2>
                            <p>"Voices are needed for full-cast audiobooks and for hearing the companion and characters speak. They also handle speech recognition, which syncing books with audiobooks and the microphone use. It's a large download, so it can run while you carry on."</p>
                            <settings::VoicesSetup s/>
                        }.into_any(),
                        _ => view! {
                            <h2>"Downloads (optional)"</h2>
                            <p>"Free books from Project Gutenberg, Standard Ebooks, Internet Archive and LibriVox download directly; nothing to set up. If you use qBittorrent with your own indexers, connect it in Settings → qBittorrent and Indexers."</p>
                            <p class="hint">"You can change anything later in Settings."</p>
                        }.into_any(),
                    }}
                    <div class="setup-nav row">
                        <Show when=move || 0 < step.get()>
                            <button on:click=move |_| step.update(|n| *n -= 1)>"Back"</button>
                        </Show>
                        <button class="primary" on:click=next>
                            {move || match step.get() {
                                0 => "Let's start",
                                n if n + 1 >= STEPS.len() => "Finish",
                                _ => "Next",
                            }}
                        </button>
                    </div>
                </Show>
            </div>
        </div>
    }
}
