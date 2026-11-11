//! The daily briefing: today's AI-made podcast from your feeds, with the stories and their
//! sources, a player that keeps going with the screen off, and the microphone for asking about
//! what you're hearing. Its sources and hosts are set up here too.

use duet_common::{
    ChatTurn,
    podcast::{CATALOG, Episode, EpisodeSummary, HostVoice, PodcastAnswer, PodcastAsk, PodcastSettings, PodcastSource, strip_directions},
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;

use crate::{api, icon, icons, toast};

#[wasm_bindgen(inline_js = r#"
let audio = null, spare = null, lines = [], idx = 0, want = false, cbIndex = null, cbState = null, rate = 1, answer = null;
function state(s) { if (cbState) cbState(s); }
// Two players: the line playing, and the next one already loaded, so one line runs straight
// into the next without stopping to fetch and decode it.
function wire(el) {
  el.preload = "auto";
  el.addEventListener("ended", () => { if (el === audio) { idx += 1; advance(); } });
  el.addEventListener("playing", () => { if (el === audio) state("playing"); });
  el.addEventListener("pause", () => { if (el === audio && !el.ended) state("paused"); });
  el.addEventListener("waiting", () => { if (el === audio) state("waiting"); });
  return el;
}
function has(el, url) { return el && el.src && el.src.endsWith(url); }
// Gets the line after this one loading in the spare player.
function preload() {
  const n = lines[idx + 1];
  if (n && n.ready && !has(spare, n.url)) { spare.src = n.url; spare.load(); }
}
// On to the next line: the spare has it loaded already, so it just starts.
function advance() {
  const l = lines[idx];
  if (l && l.ready && has(spare, l.url)) {
    const done = audio;
    audio = spare; spare = done;
    audio.playbackRate = rate;
    if (cbIndex) cbIndex(idx);
    if (want) audio.play().catch(() => state("paused"));
    done.pause();
    preload();
    return;
  }
  load();
}
function load() {
  const l = lines[idx];
  if (!l) { want = false; state("ended"); return; }
  if (!l.ready) { state("waiting"); return; }
  if (!has(audio, l.url)) audio.src = l.url;
  audio.playbackRate = rate;
  if (cbIndex) cbIndex(idx);
  if (want) audio.play().catch(() => state("paused"));
  preload();
}
export function podInit(onIndex, onState) {
  if (!audio) {
    audio = wire(new Audio());
    spare = wire(new Audio());
  }
  cbIndex = onIndex; cbState = onState;
  if ("mediaSession" in navigator) {
    const ms = navigator.mediaSession;
    ms.setActionHandler("play", () => podResume());
    ms.setActionHandler("pause", () => podPause());
    ms.setActionHandler("nexttrack", () => podJump(1));
    ms.setActionHandler("previoustrack", () => podJump(-1));
    try { ms.setActionHandler("seekforward", () => podStep(1)); ms.setActionHandler("seekbackward", () => podStep(-1)); } catch (_) {}
  }
}
export function podLines(json) {
  const was = lines[idx];
  lines = JSON.parse(json);
  if (want && was && !was.ready && lines[idx] && lines[idx].ready) load();
  else if (audio) preload();
}
export function podPlayFrom(i) { idx = Math.max(0, i); want = true; load(); }
export function podPause() { want = false; if (audio) audio.pause(); state("paused"); }
export function podResume() { want = true; if (audio && audio.src && lines[idx] && audio.src.endsWith(lines[idx].url)) audio.play().catch(() => {}); else load(); }
export function podPlaying() { return want; }
export function podStep(d) { idx = Math.min(Math.max(0, idx + d), lines.length - 1); load(); }
// The start of the next (or this / the previous) story.
export function podJump(d) {
  const seg = lines[idx] ? lines[idx].seg : 0;
  let target = idx;
  if (d > 0) { target = lines.findIndex((l) => l.seg > seg); if (target < 0) return; }
  else {
    const start = lines.findIndex((l) => l.seg === seg);
    const into = audio && audio.currentTime > 3;
    if (idx === start && !into) { const prev = lines.findIndex((l) => l.seg === seg - 1); target = prev < 0 ? 0 : prev; }
    else target = start;
  }
  idx = target; load();
}
export function podRate(r) { rate = r; if (audio) audio.playbackRate = r; if (spare) spare.playbackRate = r; }
export function podStop() { want = false; for (const el of [audio, spare]) if (el) { el.pause(); el.removeAttribute("src"); } lines = []; idx = 0; if (answer) answer.pause(); }
export function podMeta(title, artist) {
  if ("mediaSession" in navigator) navigator.mediaSession.metadata = new MediaMetadata({ title, artist, album: "Daily briefing" });
}
export function podSay(url, onEnd) {
  if (answer) answer.pause();
  answer = new Audio(url);
  answer.addEventListener("ended", () => onEnd());
  answer.play().catch(() => onEnd());
}
// Hands-free: the microphone stays open while the show plays, with echo cancellation so the
// host's own voice doesn't count. Talking over it calls onSpeech; once armed (you're being
// recorded), a pause after you've spoken calls onDone.
let ears = null;
export async function podListen(onSpeech, onDone) {
  if (ears) { ears.onSpeech = onSpeech; ears.onDone = onDone; return; }
  const stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
  const ctx = new AudioContext();
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 1024;
  ctx.createMediaStreamSource(stream).connect(analyser);
  const buf = new Float32Array(analyser.fftSize);
  ears = { stream, ctx, onSpeech, onDone, floor: 0.01, loudSince: 0, quietSince: 0, armed: false, spoke: false, armedAt: 0, stopped: false };
  const e = ears;
  const tick = () => {
    if (e.stopped) return;
    analyser.getFloatTimeDomainData(buf);
    let sum = 0;
    for (const v of buf) sum += v * v;
    const rms = Math.sqrt(sum / buf.length);
    const now = performance.now();
    // The room's own level, followed slowly, so a fan or traffic doesn't count as talking.
    if (rms < e.floor * 2) e.floor = e.floor * 0.98 + rms * 0.02;
    const loud = rms > Math.max(0.02, e.floor * 4);
    if (loud) { e.quietSince = 0; if (!e.loudSince) e.loudSince = now; } else { e.loudSince = 0; if (!e.quietSince) e.quietSince = now; }
    if (!e.armed) {
      if (e.loudSince && now - e.loudSince > 300 && e.onSpeech) { e.loudSince = 0; e.onSpeech(); }
    } else {
      if (loud && now - e.armedAt > 200) e.spoke = true;
      // Done: a pause after speaking, or nothing said at all for a while.
      if ((e.spoke && e.quietSince && now - e.quietSince > 1300) || (!e.spoke && now - e.armedAt > 7000)) {
        e.armed = false; e.spoke = false;
        if (e.onDone) e.onDone();
      }
    }
    e.timer = setTimeout(tick, 50);
  };
  tick();
}
export function podListenArm(on) { if (ears) { ears.armed = on; ears.spoke = on; ears.armedAt = performance.now(); ears.quietSince = 0; } }
export function podListenStop() {
  if (!ears) return;
  ears.stopped = true; clearTimeout(ears.timer);
  ears.stream.getTracks().forEach((t) => t.stop());
  ears.ctx.close().catch(() => {});
  ears = null;
}
export function podAnswering() { return !!(answer && !answer.paused && !answer.ended); }
export function podStopAnswer() { if (answer) answer.pause(); }
// "Wednesday" / "November 11" from an episode's start time.
export function podDate(iso, part) {
  const d = new Date(iso);
  if (isNaN(d)) return "";
  return part === "weekday" ? d.toLocaleDateString(undefined, { weekday: "long" }) : d.toLocaleDateString(undefined, { month: "long", day: "numeric" });
}
export function podSaved(key) { try { return parseInt(localStorage.getItem("duet-brief-" + key) || "0", 10) || 0; } catch (_) { return 0; } }
export function podSave(key, i) { try { localStorage.setItem("duet-brief-" + key, String(i)); } catch (_) {} }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = podInit)]
    fn pod_init(on_index: &js_sys::Function, on_state: &js_sys::Function);
    #[wasm_bindgen(js_name = podLines)]
    fn pod_lines(json: &str);
    #[wasm_bindgen(js_name = podPlayFrom)]
    fn pod_play_from(i: usize);
    #[wasm_bindgen(js_name = podPause)]
    fn pod_pause();
    #[wasm_bindgen(js_name = podResume)]
    fn pod_resume();
    #[wasm_bindgen(js_name = podPlaying)]
    fn pod_playing() -> bool;
    #[wasm_bindgen(js_name = podStep)]
    fn pod_step(d: i32);
    #[wasm_bindgen(js_name = podJump)]
    fn pod_jump(d: i32);
    #[wasm_bindgen(js_name = podRate)]
    fn pod_rate(r: f64);
    #[wasm_bindgen(js_name = podStop)]
    fn pod_stop();
    #[wasm_bindgen(js_name = podMeta)]
    fn pod_meta(title: &str, artist: &str);
    #[wasm_bindgen(js_name = podSay)]
    fn pod_say(url: &str, on_end: &js_sys::Function);
    #[wasm_bindgen(js_name = podListen, catch)]
    async fn pod_listen(on_speech: &js_sys::Function, on_done: &js_sys::Function) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = podListenArm)]
    fn pod_listen_arm(on: bool);
    #[wasm_bindgen(js_name = podListenStop)]
    fn pod_listen_stop();
    #[wasm_bindgen(js_name = podAnswering)]
    fn pod_answering() -> bool;
    #[wasm_bindgen(js_name = podStopAnswer)]
    fn pod_stop_answer();
    #[wasm_bindgen(js_name = podDate)]
    fn pod_date(iso: &str, part: &str) -> String;
    #[wasm_bindgen(js_name = podSaved)]
    fn pod_saved(key: &str) -> usize;
    #[wasm_bindgen(js_name = podSave)]
    fn pod_save(key: &str, i: usize);
}

/// Every line in order, as the player plays them: (segment, line).
fn flat(ep: &Episode) -> Vec<(usize, usize)> {
    ep.segments.iter().enumerate().flat_map(|(s, seg)| (0..seg.lines.len()).map(move |l| (s, l))).collect()
}

fn lines_json(ep: &Episode) -> String {
    let v: Vec<serde_json::Value> = ep
        .segments
        .iter()
        .enumerate()
        .flat_map(|(s, seg)| {
            let id = ep.id.clone();
            let key = seg.key.clone();
            seg.lines.iter().enumerate().map(move |(l, line)| {
                serde_json::json!({ "seg": s, "ready": line.ready, "url": format!("/api/podcast/episodes/{id}/audio/{key}/{l}") })
            })
        })
        .collect();
    serde_json::to_string(&v).unwrap_or_default()
}

fn stage_text(ep: &Episode) -> String {
    if !ep.doing.is_empty() {
        return ep.doing.clone();
    }
    match ep.stage.as_str() {
        "gathering" => "Checking your sources",
        "choosing" => "Choosing the stories",
        "reading" => "Reading the stories",
        "writing" => "Writing the script",
        "recording" => "Recording",
        _ => "",
    }
    .into()
}

#[derive(Clone, PartialEq)]
struct Exchange {
    question: String,
    answer: Option<PodcastAnswer>,
}

#[component]
pub fn Briefing() -> impl IntoView {
    let toast = toast();
    let tab = RwSignal::new("listen");
    let episodes = RwSignal::new(Vec::<EpisodeSummary>::new());
    let current = RwSignal::new(None::<Episode>);
    let settings = RwSignal::new(None::<PodcastSettings>);
    let starting = RwSignal::new(false);
    // Where the player is (an index into `flat`) and what it's doing.
    let index = RwSignal::new(0usize);
    let pstate = RwSignal::new(String::from("paused"));
    let rate = RwSignal::new(1.0f64);

    let on_index = Closure::<dyn Fn(f64)>::new(move |i: f64| {
        let i = i as usize;
        index.set(i);
        if let Some(ep) = current.get_untracked() {
            pod_save(&ep.id, i);
            if let Some((s, _)) = flat(&ep).get(i) {
                pod_meta(&ep.segments[*s].title, &format!("Daily briefing · {}", ep.title));
            }
        }
    })
    .into_js_value();
    let on_state = Closure::<dyn Fn(String)>::new(move |s: String| pstate.set(s)).into_js_value();
    pod_init(on_index.unchecked_ref(), on_state.unchecked_ref());
    on_cleanup(pod_stop);
    // The voices loaded and kept loaded while the briefing's open.
    spawn_local(async {
        let _ = api::post::<_, serde_json::Value>("/api/podcast/warm", &()).await;
    });

    let open_episode = move |id: String| {
        spawn_local(async move {
            match api::get::<Episode>(&format!("/api/podcast/episodes/{id}")).await {
                Ok(ep) => {
                    pod_stop();
                    pod_lines(&lines_json(&ep));
                    let saved = pod_saved(&ep.id).min(flat(&ep).len().saturating_sub(1));
                    index.set(saved);
                    current.set(Some(ep));
                }
                Err(e) => toast.show(e.0),
            }
        });
    };
    let refresh_list = move |open_latest: bool| {
        spawn_local(async move {
            if let Ok(list) = api::get::<Vec<EpisodeSummary>>("/api/podcast/episodes").await {
                if open_latest {
                    if let Some(first) = list.first() {
                        open_episode(first.id.clone());
                    }
                }
                episodes.set(list);
            }
        });
    };
    refresh_list(true);
    spawn_local(async move {
        if let Ok(s) = api::get::<PodcastSettings>("/api/podcast/settings").await {
            settings.set(Some(s));
        }
    });

    // While an episode is being made, it's checked every couple of seconds.
    let alive = StoredValue::new(true);
    on_cleanup(move || alive.set_value(false));
    spawn_local(async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(2000).await;
            if !alive.try_get_value().unwrap_or(false) {
                break;
            }
            let Some(ep) = current.get_untracked() else { continue };
            if ep.finished() {
                continue;
            }
            if let Ok(new) = api::get::<Episode>(&format!("/api/podcast/episodes/{}", ep.id)).await {
                if new != ep {
                    pod_lines(&lines_json(&new));
                    let done = new.finished();
                    current.set(Some(new));
                    if done {
                        refresh_list(false);
                    }
                }
            }
        }
    });

    let make = move |_| {
        if starting.get_untracked() {
            return;
        }
        starting.set(true);
        spawn_local(async move {
            match api::post::<_, Episode>("/api/podcast/episodes", &()).await {
                Ok(ep) => {
                    pod_stop();
                    pod_lines(&lines_json(&ep));
                    index.set(0);
                    current.set(Some(ep));
                    refresh_list(false);
                }
                Err(e) => toast.show(e.0),
            }
            starting.set(false);
        });
    };

    // Settings live in their own pane, out of the player's way.
    let show_settings = RwSignal::new(false);
    let show_episodes = RwSignal::new(false);
    let _ = tab;

    view! {
        <div class="briefing">
            <header class="brief-head">
                <h1>"Daily briefing " {crate::ai_tag()}</h1>
                <div class="brief-head-actions">
                    <button class="ghost" title="Make a new episode now" on:click=make disabled=move || starting.get()>
                        {move || if starting.get() { view! { <span class="spinner small"></span> }.into_any() } else { icon(icons::SPARKLE).into_any() }}
                        <span class="label">"New episode"</span>
                    </button>
                    <button class="icon-button" title="Episodes" aria-label="Episodes" class:active=move || show_episodes.get()
                        on:click=move |_| show_episodes.update(|v| *v = !*v)>
                        <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M4 6h16M4 12h16M4 18h10"/></svg>
                    </button>
                    <button class="icon-button" title="Sources, hosts and logins" aria-label="Briefing settings" on:click=move |_| show_settings.set(true)>
                        {icon(icons::SETTINGS)}
                    </button>
                </div>
            </header>
            <div class="brief-stage">
                {move || match current.get() {
                    None => view! {
                        <div class="brief-empty">
                            <div class="brief-art empty"><span class="art-show">"Daily briefing"</span></div>
                            <p>"No episodes yet. Your briefing is deep dives on what you care about, from your feeds, read by an AI host."</p>
                            <div class="brief-empty-actions">
                                <button class="primary" on:click=make disabled=move || starting.get()>{icon(icons::SPARKLE)}"Make today's briefing"</button>
                                <button class="ghost" on:click=move |_| show_settings.set(true)>"Choose topics and sources"</button>
                            </div>
                        </div>
                    }.into_any(),
                    Some(_) => view! { <Player current index pstate rate/> }.into_any(),
                }}
            </div>
            <Show when=move || show_episodes.get()>
                <leptos::portal::Portal>
                <div class="brief-scrim" on:click=move |_| show_episodes.set(false)></div>
                <aside class="brief-pane brief-episodes" role="dialog" aria-label="Episodes">
                    <div class="pane-head">
                        <h2>"Episodes"</h2>
                        <button class="icon-button" aria-label="Close" on:click=move |_| show_episodes.set(false)>"×"</button>
                    </div>
                    <For each=move || episodes.get() key=|e| (e.id.clone(), e.stage.clone()) let:e>
                        {
                            let id = e.id.clone();
                            let id2 = e.id.clone();
                            let making = !matches!(e.stage.as_str(), "ready" | "failed");
                            let when = format!("{} · {}", pod_date(&e.created, "weekday"), pod_date(&e.created, "day"));
                            view! {
                                <button class="brief-ep" class:active=move || current.with(|c| c.as_ref().is_some_and(|c| c.id == id2))
                                    on:click=move |_| { open_episode(id.clone()); show_episodes.set(false) }>
                                    <span class="ep-when">{when}</span>
                                    <span class="ep-title">{episode_name(&e.title)}</span>
                                    <small>{if making { "Being made…".to_string() } else if e.stage == "failed" { "Didn't finish".into() } else { format!("{} stories", e.stories) }}</small>
                                </button>
                            }
                        }
                    </For>
                </aside>
                </leptos::portal::Portal>
            </Show>
            <Show when=move || show_settings.get()>
                <leptos::portal::Portal>
                <div class="brief-scrim" on:click=move |_| show_settings.set(false)></div>
                <aside class="brief-pane brief-settings-pane" role="dialog" aria-label="Briefing settings">
                    <div class="pane-head">
                        <h2>"Briefing settings"</h2>
                        <button class="icon-button" aria-label="Close" on:click=move |_| show_settings.set(false)>"×"</button>
                    </div>
                    <BriefingSettings settings/>
                </aside>
                </leptos::portal::Portal>
            </Show>
        </div>
    }
}

/// An episode's own name, without the date the server puts in front ("Briefing, Wed… · Name").
fn episode_name(title: &str) -> String {
    match title.split_once(" · ") {
        Some((_, name)) if !name.trim().is_empty() => name.trim().to_string(),
        _ => title.to_string(),
    }
}

#[component]
fn Player(current: RwSignal<Option<Episode>>, index: RwSignal<usize>, pstate: RwSignal<String>, rate: RwSignal<f64>) -> impl IntoView {
    let toast = toast();
    let ep = move || current.get().unwrap_or_default();
    let here = move || current.with(|c| c.as_ref().and_then(|e| flat(e).get(index.get()).copied()));
    let playing = move || matches!(pstate.get().as_str(), "playing" | "waiting");

    // Questions.
    let exchanges = RwSignal::new(Vec::<Exchange>::new());
    let input = RwSignal::new(String::new());
    let recording = RwSignal::new(false);
    let mic_starting = RwSignal::new(false);
    let mic_level = RwSignal::new(0.0f64);
    let thinking = RwSignal::new(false);
    let resume_after = RwSignal::new(true);
    let was_playing = StoredValue::new(false);

    let ask = move |question: String| {
        let question = question.trim().to_string();
        if question.is_empty() {
            return;
        }
        let Some(e) = current.get_untracked() else { return };
        let (segment, line) = flat(&e).get(index.get_untracked()).copied().unwrap_or((0, 0));
        let history: Vec<ChatTurn> = exchanges
            .get_untracked()
            .iter()
            .rev()
            .take(6)
            .rev()
            .flat_map(|x| {
                let mut t = vec![ChatTurn { role: "user".into(), content: x.question.clone() }];
                if let Some(a) = &x.answer {
                    t.push(ChatTurn { role: "assistant".into(), content: a.spoken.clone() });
                }
                t
            })
            .collect();
        exchanges.update(|x| x.push(Exchange { question: question.clone(), answer: None }));
        input.set(String::new());
        thinking.set(true);
        let req_id = e.id.clone();
        let req = PodcastAsk { episode: e.id.clone(), segment, line, question, history };
        spawn_local(async move {
            match api::post::<_, PodcastAnswer>("/api/podcast/ask", &req).await {
                Ok(a) => {
                    let audio = a.audio.clone();
                    if a.detour.is_some() {
                        // The detour's in the episode now: show it, and keep checking on it.
                        if let Ok(new) = api::get::<Episode>(&format!("/api/podcast/episodes/{}", req_id)).await {
                            pod_lines(&lines_json(&new));
                            current.set(Some(new));
                        }
                    }
                    exchanges.update(|x| {
                        if let Some(last) = x.last_mut() {
                            last.answer = Some(a);
                        }
                    });
                    thinking.set(false);
                    let resume = move || {
                        if resume_after.get_untracked() && was_playing.get_value() {
                            pod_resume();
                        }
                    };
                    match audio {
                        Some(url) => {
                            let done = Closure::once_into_js(resume);
                            pod_say(&url, done.unchecked_ref());
                        }
                        None => resume(),
                    }
                }
                Err(err) => {
                    toast.show(err.0);
                    exchanges.update(|x| {
                        x.pop();
                    });
                    thinking.set(false);
                }
            }
        });
    };

    // Asked by talking over the show (hands-free), rather than with the button.
    let by_voice = StoredValue::new(false);
    let mic_toggle = move || {
        if recording.get_untracked() {
            recording.set(false);
            pod_listen_arm(false);
            let voice = by_voice.get_value();
            by_voice.set_value(false);
            spawn_local(async move {
                match crate::stop_recording().await {
                    Ok(text) => {
                        let text = text.as_string().unwrap_or_default();
                        if text.trim().is_empty() {
                            // (Hands-free mistakes a cough or a car for talking now and then: just carry on.)
                            if !voice {
                                toast.show("Didn't catch any words. Is the right microphone selected?");
                            }
                            if resume_after.get_untracked() && was_playing.get_value() {
                                pod_resume();
                            }
                        } else {
                            ask(text);
                        }
                    }
                    Err(e) => toast.show(format!("Couldn't transcribe: {e:?}")),
                }
            });
            return;
        }
        if mic_starting.get_untracked() {
            return;
        }
        // Stop the show (and any answer being given) while you ask.
        if pod_answering() {
            pod_stop_answer();
        } else {
            was_playing.set_value(pod_playing());
        }
        pod_pause();
        mic_starting.set(true);
        input.set(String::new());
        let on_partial = Closure::<dyn Fn(JsValue)>::new(move |t: JsValue| {
            if recording.get_untracked() {
                input.set(t.as_string().unwrap_or_default());
            }
        })
        .into_js_value();
        let on_level = Closure::<dyn Fn(f64)>::new(move |l: f64| mic_level.set(l)).into_js_value();
        spawn_local(async move {
            let started = crate::start_recording(on_partial.unchecked_ref(), on_level.unchecked_ref()).await;
            mic_starting.set(false);
            match started {
                Ok(_) => recording.set(true),
                Err(e) => toast.show(format!("Microphone unavailable: {e:?}")),
            }
        });
    };
    let mic = move |_| mic_toggle();

    // Hands-free: listening while it talks, so you can just start speaking to interrupt.
    let hands_free = RwSignal::new(pod_saved("handsfree-off") == 0);
    let listening = RwSignal::new(false);
    let on_speech = Closure::<dyn Fn()>::new(move || {
        if recording.get_untracked() || mic_starting.get_untracked() || thinking.get_untracked() {
            return;
        }
        if !(pod_playing() || pod_answering()) {
            return;
        }
        by_voice.set_value(true);
        mic_toggle();
        pod_listen_arm(true);
    })
    .into_js_value();
    let on_done = Closure::<dyn Fn()>::new(move || {
        if recording.get_untracked() && by_voice.get_value() {
            mic_toggle();
        }
    })
    .into_js_value();
    let ears = StoredValue::new((on_speech, on_done));
    Effect::new(move |_| {
        let want = hands_free.get() && crate::mic_supported() && matches!(pstate.get().as_str(), "playing" | "waiting");
        if want && !listening.get_untracked() {
            listening.set(true);
            let (speech, done) = ears.get_value();
            spawn_local(async move {
                if let Err(e) = pod_listen(speech.unchecked_ref(), done.unchecked_ref()).await {
                    listening.set(false);
                    hands_free.set(false);
                    toast.show(format!("Hands-free needs the microphone: {e:?}"));
                }
            });
        } else if !hands_free.get() && listening.get_untracked() {
            listening.set(false);
            pod_listen_stop();
        }
    });
    on_cleanup(pod_listen_stop);

    let play_pause = move |_| {
        if playing() {
            pod_pause();
        } else if pstate.get_untracked() == "ended" {
            pod_play_from(0);
        } else {
            pod_play_from(index.get_untracked());
        }
    };
    let record_again = move |_| {
        let Some(e) = current.get_untracked() else { return };
        spawn_local(async move {
            match api::post::<_, Episode>(&format!("/api/podcast/episodes/{}/record", e.id), &()).await {
                Ok(ep) => current.set(Some(ep)),
                Err(err) => toast.show(err.0),
            }
        });
    };

    let progress = move || {
        current.with(|c| {
            let Some(e) = c else { return (0, 0, 0) };
            let all = flat(e);
            let ready = e.segments.iter().flat_map(|s| &s.lines).filter(|l| l.ready).count();
            (index.get().min(all.len()), ready, all.len())
        })
    };

    // The cover: the picture from the story playing (or the first story with one).
    let art = move || {
        current.with(|c| {
            let e = c.as_ref()?;
            let now = flat(e).get(index.get()).map(|(s, _)| *s).unwrap_or(0);
            let pic = |seg: &duet_common::podcast::Segment| seg.sources.iter().find_map(|x| x.image.clone());
            e.segments.get(now).and_then(pic).or_else(|| e.segments.iter().find_map(pic))
        })
    };

    let numbered = move || exchanges.get().into_iter().enumerate().collect::<Vec<(usize, Exchange)>>();

    view! {
        <section class="brief-player">
            <div class="brief-now">
                <div class="brief-art" class:playing=playing
                    style:background-image=move || art().map(|src| format!("url(\"{}\")", src.replace('"', "%22"))).unwrap_or_default()>
                    <div class="art-text">
                        <span class="art-show">"Daily briefing"</span>
                        <span class="art-day">{move || pod_date(&ep().created, "weekday")}</span>
                        <span class="art-date">{move || pod_date(&ep().created, "day")}</span>
                    </div>
                </div>
                <div class="brief-title">
                    <small>{move || episode_name(&ep().title)}</small>
                    <h2>{move || here().and_then(|(s, _)| current.with(|c| c.as_ref().map(|e| e.segments[s].title.clone()))).unwrap_or_else(|| "Getting today's stories".into())}</h2>
                </div>
                <Show when=move || !ep().finished()>
                    <p class="brief-making"><span class="spinner small"></span>{move || stage_text(&ep())}</p>
                </Show>
                <Show when=move || ep().error.is_some()>
                    <p class="brief-error">
                        {move || ep().error.unwrap_or_default()}
                        <Show when=move || ep().segments.iter().any(|s| s.lines.iter().any(|l| !l.ready))>
                            " "<button class="link" on:click=record_again>"Record the rest"</button>
                        </Show>
                    </p>
                </Show>
                <div class="brief-caption" aria-live="polite">
                    {move || here().and_then(|(s, l)| current.with(|c| c.as_ref().map(|e| e.segments[s].lines[l].clone()))).map(|line| view! {
                        <span class="speaker">{line.speaker.clone()}</span>
                        <p>{strip_directions(&line.text)}</p>
                    })}
                    <Show when=move || pstate.get() == "waiting">
                        <p class="hint"><span class="spinner small"></span>"Waiting for the hosts to record this…"</p>
                    </Show>
                </div>
                <div class="brief-bar" title=move || { let (_, ready, total) = progress(); format!("{ready} of {total} lines recorded") }>
                    <div class="recorded" style:width=move || { let (_, r, t) = progress(); format!("{:.1}%", if t == 0 { 0.0 } else { r as f64 * 100.0 / t as f64 }) }></div>
                    <div class="played" style:width=move || { let (i, _, t) = progress(); format!("{:.1}%", if t == 0 { 0.0 } else { i as f64 * 100.0 / t as f64 }) }></div>
                </div>
                <div class="brief-controls">
                    <button class="icon-button" title="Previous story" aria-label="Previous story" on:click=move |_| pod_jump(-1)>
                        <svg viewBox="0 0 24 24" class="icon" aria-hidden="true"><path d="M6 5v14M18 5 9 12l9 7z" fill="currentColor" stroke="none"/></svg>
                    </button>
                    <button class="icon-button" title="Back a line" aria-label="Back a line" on:click=move |_| pod_step(-1)>
                        <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M4 12a8 8 0 1 0 2.4-5.7L4 8.5M4 4v4.5h4.5"/></svg>
                    </button>
                    <button class="brief-play primary" aria-label=move || if playing() { "Pause" } else { "Play" } on:click=play_pause>
                        {move || if playing() {
                            view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 5h3.5v14H7zM13.5 5H17v14h-3.5z" fill="currentColor"/></svg> }.into_any()
                        } else {
                            view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5v14l11-7z" fill="currentColor"/></svg> }.into_any()
                        }}
                    </button>
                    <button class="icon-button" title="Forward a line" aria-label="Forward a line" on:click=move |_| pod_step(1)>
                        <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M20 12a8 8 0 1 1-2.4-5.7L20 8.5M20 4v4.5h-4.5"/></svg>
                    </button>
                    <button class="icon-button" title="Next story" aria-label="Next story" on:click=move |_| pod_jump(1)>
                        <svg viewBox="0 0 24 24" class="icon" aria-hidden="true"><path d="M18 5v14M6 5l9 7-9 7z" fill="currentColor" stroke="none"/></svg>
                    </button>
                    <select class="brief-rate" title="Speed" on:change=move |ev| {
                        let r = event_target_value(&ev).parse().unwrap_or(1.0);
                        rate.set(r);
                        pod_rate(r);
                    }>
                        {[0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0].map(|r| view! { <option value=r.to_string() selected=move || rate.get() == r>{format!("{r}×")}</option> })}
                    </select>
                </div>
            </div>

            <div class="brief-ask">
                <div class="brief-exchanges">
                    <For each=numbered key=|(i, x)| (*i, x.answer.is_some()) let:item>
                        <div class="message mine"><small>"You"</small><p>{item.1.question.clone()}</p></div>
                        {item.1.answer.clone().map(|a| {
                            let detour = a.detour.clone();
                            view! {
                                <div class="message">
                                    <small>{a.speaker.clone()}</small>
                                    <p>{a.text.clone()}</p>
                                    {(!a.links.is_empty()).then(|| view! {
                                        <ul class="answer-links">
                                            {a.links.iter().map(|l| view! {
                                                <li><a href=l.url.clone() target="_blank" rel="noopener noreferrer">
                                                    {(!l.site.is_empty()).then(|| view! { <span class="site">{l.site.clone()}</span> })}
                                                    {if l.title.is_empty() { l.url.clone() } else { l.title.clone() }}
                                                </a></li>
                                            }).collect_view()}
                                        </ul>
                                    })}
                                    {detour.map(|key| {
                                        let k2 = key.clone();
                                        let key = StoredValue::new(key);
                                        let seg = move || key.with_value(|key| current.with(|c| c.as_ref().and_then(|e| e.segments.iter().position(|x| &x.key == key).map(|i| (i, e.segments[i].clone())))));
                                        view! {
                                            <div class="detour-note">
                                                {icon(icons::SPARKLE)}
                                                <span>{move || match seg() {
                                                    Some((_, x)) if x.writing => format!("Digging into “{}”…", x.title),
                                                    Some((_, x)) => format!("“{}” plays after this part", x.title),
                                                    None => "That detour didn't work out".into(),
                                                }}</span>
                                                <button class="link" disabled=move || !seg().is_some_and(|(_, x)| x.lines.first().is_some_and(|l| l.ready))
                                                    on:click=move |_| {
                                                        let Some(e) = current.get_untracked() else { return };
                                                        if let Some(s) = e.segments.iter().position(|x| x.key == k2) {
                                                            if let Some(i) = flat(&e).iter().position(|(x, _)| *x == s) {
                                                                pod_play_from(i);
                                                            }
                                                        }
                                                    }>"Play it now"</button>
                                            </div>
                                        }
                                    })}
                                </div>
                            }
                        })}
                    </For>
                    <Show when=move || thinking.get()><p class="hint thinking"><span class="spinner small"></span>"Looking into it…"</p></Show>
                </div>
                <form on:submit=move |ev| { ev.prevent_default(); was_playing.set_value(pod_playing()); pod_pause(); ask(input.get_untracked()) }>
                    <Show when=crate::mic_supported>
                        <button type="button" class="mic big" class:recording=move || recording.get() class:starting=move || mic_starting.get() on:click=mic
                            style:--level=move || format!("{:.2}", if recording.get() { mic_level.get() } else { 0.0 })
                            title=move || if recording.get() { "Stop and ask" } else { "Pause and ask a question" } aria-label="Ask a question">
                            {move || if recording.get() {
                                view! { <svg viewBox="0 0 24 24" aria-hidden="true" class="mic-icon"><rect x="7" y="7" width="10" height="10" rx="2.5" class="fill"/></svg> }.into_any()
                            } else {
                                view! { <svg viewBox="0 0 24 24" aria-hidden="true" class="mic-icon"><rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21M8.5 21h7"/></svg> }.into_any()
                            }}
                        </button>
                    </Show>
                    <input type="text" prop:value=move || input.get()
                        placeholder=move || if mic_starting.get() { "Waiting for the microphone…" } else if recording.get() { "Listening…" } else { "Ask, or say \"tell me more about…\"" }
                        on:input=move |ev| input.set(event_target_value(&ev))/>
                </form>
                <div class="brief-ask-options">
                    <Show when=crate::mic_supported>
                        <label class="check" title="The microphone stays on while the show plays (echo-cancelled), so you can just start talking to ask something">
                            <input type="checkbox" prop:checked=move || hands_free.get() on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                pod_save("handsfree-off", if on { 0 } else { 1 });
                                hands_free.set(on);
                            }/>
                            "Hands-free: just talk to interrupt"
                            <Show when=move || listening.get()><span class="listening-dot" aria-label="Listening"></span></Show>
                        </label>
                    </Show>
                    <label class="check"><input type="checkbox" prop:checked=move || resume_after.get() on:change=move |ev| resume_after.set(event_target_checked(&ev))/>"Carry on after the answer"</label>
                </div>
            </div>

            <h3 class="brief-chapters-head">"In this episode"</h3>
            <ol class="brief-stories">
                {move || {
                    let e = ep();
                    let all = flat(&e);
                    e.segments.iter().enumerate().map(|(s, seg)| {
                        let start = all.iter().position(|(x, _)| *x == s).unwrap_or(0);
                        let ready = seg.lines.iter().filter(|l| l.ready).count();
                        let total = seg.lines.len();
                        let state = if seg.writing { "Being written…".to_string() } else if total > 0 && ready < total { format!("Recording {ready}/{total}") } else { String::new() };
                        let sources = seg.sources.clone();
                        view! {
                            <li class="brief-story" class:now=move || here().is_some_and(|(x, _)| x == s) class:intro=seg.kind == "intro" || seg.kind == "outro" class:detour=seg.kind == "detour">
                                <button class="story-head" disabled=total == 0 on:click=move |_| pod_play_from(start)>
                                    {sources.iter().find_map(|x| x.image.clone()).map(|src| view! { <img src=src alt="" loading="lazy" referrerpolicy="no-referrer"/> })}
                                    <span class="story-title">{(seg.kind == "detour").then(|| view! { <span class="detour-tag">"Your detour"</span> })}{seg.title.clone()}</span>
                                    <small>{state}</small>
                                </button>
                                {(!sources.is_empty()).then(|| view! {
                                    <ul class="story-sources">
                                        {sources.into_iter().map(|x| view! {
                                            <li><a href=x.url.clone() target="_blank" rel="noopener noreferrer"><span class="site">{x.site.clone()}</span>{x.title.clone()}</a></li>
                                        }).collect_view()}
                                    </ul>
                                })}
                            </li>
                        }
                    }).collect_view()
                }}
            </ol>
        </section>
    }
}

/// Sources, interests, length, the morning time, and the hosts. Saved as you change them.
#[component]
fn BriefingSettings(settings: RwSignal<Option<PodcastSettings>>) -> impl IntoView {
    let toast = toast();
    let voices = RwSignal::new(Vec::<HostVoice>::new());
    let recasting = RwSignal::new(None::<String>);
    let load_voices = move || {
        spawn_local(async move {
            if let Ok(v) = api::get::<Vec<HostVoice>>("/api/podcast/voices").await {
                voices.set(v);
            }
        })
    };
    load_voices();
    let save = move |f: Box<dyn FnOnce(&mut PodcastSettings)>| {
        let Some(mut s) = settings.get_untracked() else { return };
        f(&mut s);
        settings.set(Some(s.clone()));
        spawn_local(async move {
            match api::put::<_, PodcastSettings>("/api/podcast/settings", &s).await {
                Ok(_) => toast.show("Saved"),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let new_kind = RwSignal::new(String::from("reddit"));
    let new_value = RwSignal::new(String::new());
    let add = move || {
        let value = new_value.get_untracked().trim().to_string();
        if value.is_empty() {
            return;
        }
        let kind = new_kind.get_untracked();
        save(Box::new(move |s| s.sources.push(PodcastSource { kind, value, enabled: true })));
        new_value.set(String::new());
    };
    let s = move || settings.get().unwrap_or_default();
    let has = move |kind: &str, value: &str| s().sources.iter().any(|x| x.kind == kind && x.value == value);

    view! {
        <Show when=move || settings.get().is_some() fallback=|| view! { <p class="hint"><span class="spinner small"></span>"Loading…"</p> }>
            <div class="settings brief-settings">
                <section>
                    <h3>"What you care about"</h3>
                    <label class="field">
                        <span>"Interests"</span>
                        <textarea rows="4" placeholder="e.g. AI and open source, Rust, space, the Linux desktop, US and UK politics, climate; skip sports and celebrity gossip"
                            prop:value=move || s().interests
                            on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| s.interests = v)) }></textarea>
                        <small>"The producer uses this to pick stories from everything in your sources, and the hosts to decide how deep to go."</small>
                    </label>
                    <div class="brief-row">
                        <label class="field">
                            <span>"Length (minutes)"</span>
                            <input type="number" min="5" max="60" step="5" prop:value=move || s().minutes.to_string()
                                on:change=move |ev| { let v = event_target_value(&ev).parse().unwrap_or(20); save(Box::new(move |s| s.minutes = v)) }/>
                        </label>
                        <label class="field">
                            <span>"Ready every morning at"</span>
                            <input type="time" prop:value=move || s().daily_at
                                on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| s.daily_at = v)) }/>
                            <small>"Leave empty to only make one when you ask."</small>
                        </label>
                    </div>
                    <label class="check"><input type="checkbox" prop:checked=move || s().watch_videos
                        on:change=move |ev| { let v = event_target_checked(&ev); save(Box::new(move |s| s.watch_videos = v)) }/>
                        "Listen to videos and reels (needs yt-dlp), so the hosts can cover what's said in them"</label>
                </section>

                <section>
                    <h3>"Your sources"</h3>
                    <ul class="brief-sources">
                        {move || s().sources.into_iter().enumerate().map(|(i, src)| {
                            let label = source_label(&src);
                            view! {
                                <li>
                                    <label class="check"><input type="checkbox" prop:checked=src.enabled
                                        on:change=move |ev| { let v = event_target_checked(&ev); save(Box::new(move |s| if let Some(x) = s.sources.get_mut(i) { x.enabled = v })) }/>
                                        <span class="kind">{kind_name(&src.kind)}</span>{label}</label>
                                    <button class="icon-button" aria-label="Remove" title="Remove" on:click=move |_| save(Box::new(move |s| { if i < s.sources.len() { s.sources.remove(i); } }))>"×"</button>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                    <form class="brief-add" on:submit=move |ev| { ev.prevent_default(); add() }>
                        <select on:change=move |ev| new_kind.set(event_target_value(&ev)) prop:value=move || new_kind.get()>
                            {KINDS.map(|(k, name, _)| view! { <option value=k>{name}</option> })}
                        </select>
                        <input type="text" prop:value=move || new_value.get() on:input=move |ev| new_value.set(event_target_value(&ev))
                            placeholder=move || KINDS.iter().find(|k| k.0 == new_kind.get()).map(|k| k.2).unwrap_or("")/>
                        <button type="submit">"Add"</button>
                    </form>
                    <p class="hint">{move || match new_kind.get().as_str() {
                        "reddit" => "A subreddit's name, or your private front-page feed address from reddit.com/prefs/feeds (the RSS link) for everything you follow.",
                        "x" => "\"home\" for your timeline, an account, or list/<id>. Read in the remote browser: log in to X there first (below).",
                        "instagram" => "\"home\" for your feed, or an account. Read in the remote browser: log in to Instagram there first (below). Reels are listened to.",
                        "bluesky" => "A handle, like someone.bsky.social.",
                        "mastodon" => "@name@instance, or #tag@instance for a hashtag.",
                        "youtube" => "A channel's @handle, address or id. Videos are listened to.",
                        "lobsters" => "hottest, newest, or a tag like rust.",
                        "hackernews" => "best, frontpage, newest, ask or show.",
                        _ => "Any RSS or Atom feed address: blogs, newsletters, news sites.",
                    }}</p>
                    <Logins settings=settings save/>
                </section>

                <section>
                    <h3>"More sources"</h3>
                    <p class="hint">"Major news, tech and science sites and the aggregators. Switch on as many as you like: the producer picks from all of them by your interests."</p>
                    {["Social", "Aggregators", "News", "Tech", "Science"].map(|cat| view! {
                        <h4>{cat}</h4>
                        <div class="chips">
                            {CATALOG.iter().filter(|c| c.category == cat).map(|c| {
                                let (kind, value) = (c.kind, c.value);
                                view! {
                                    <button class="chip-button" class:active=move || has(kind, value) on:click=move |_| {
                                        save(Box::new(move |s| {
                                            if let Some(i) = s.sources.iter().position(|x| x.kind == kind && x.value == value) {
                                                s.sources.remove(i);
                                            } else {
                                                s.sources.push(PodcastSource { kind: kind.into(), value: value.into(), enabled: true });
                                            }
                                        }))
                                    }>{c.name}</button>
                                }
                            }).collect_view()}
                        </div>
                    })}
                </section>

                <section>
                    <h3>"Hosts"</h3>
                    <div class="segmented">
                        <button class:active=move || s().hosts.len() <= 1 on:click=move |_| save(Box::new(|s| s.hosts.truncate(1)))>"One host"</button>
                        <button class:active=move || { s().hosts.len() > 1 } on:click=move |_| save(Box::new(|s| if s.hosts.len() < 2 { s.hosts.push(duet_common::podcast::second_host()) }))>"Two hosts"</button>
                    </div>
                    <p class="hint">"AI voices, made from these descriptions, or cloned from a real recording (10–30 seconds of one person talking clearly: your own voice, or a public-domain narrator), which sounds the most natural. One host talks straight to you; two talk with each other."</p>
                    {move || s().hosts.into_iter().enumerate().map(|(i, h)| {
                        let name = h.name.clone();
                        let name2 = h.name.clone();
                        let name3 = h.name.clone();
                        let name4 = h.name.clone();
                        let clip = move || voices.get().iter().any(|v| v.name == name && v.clip);
                        let engine = h.engine.clone();
                        let sample = h.sample.clone();
                        let uploaded = !sample.is_empty() && !sample.contains("://");
                        // A new recording (or none): saved, then the voice made from it.
                        let set_sample = {
                            let name = h.name.clone();
                            move |v: String| {
                                let Some(mut st) = settings.get_untracked() else { return };
                                if let Some(x) = st.hosts.get_mut(i) {
                                    x.sample = v.trim().to_string();
                                }
                                settings.set(Some(st.clone()));
                                let name = name.clone();
                                recasting.set(Some(name.clone()));
                                spawn_local(async move {
                                    let r = match api::put::<_, PodcastSettings>("/api/podcast/settings", &st).await {
                                        Ok(_) => api::post::<_, ()>("/api/podcast/voices/recast", &serde_json::json!({ "name": name })).await,
                                        Err(e) => Err(e),
                                    };
                                    match r {
                                        Ok(()) => toast.show("New voice made"),
                                        Err(e) => toast.show(e.0),
                                    }
                                    recasting.set(None);
                                    load_voices();
                                });
                            }
                        };
                        let set_sample2 = set_sample.clone();
                        let upload = {
                            let name = h.name.clone();
                            move |ev: leptos::ev::Event| {
                                let input: web_sys::HtmlInputElement = event_target(&ev);
                                let Some(file) = input.files().and_then(|f| f.get(0)) else { return };
                                input.set_value("");
                                let name = name.clone();
                                recasting.set(Some(name.clone()));
                                spawn_local(async move {
                                    match api::upload::<PodcastSettings>(&format!("/api/podcast/voices/{}/sample", js_sys::encode_uri_component(&name)), file).await {
                                        Ok(st) => {
                                            settings.set(Some(st));
                                            toast.show("New voice made from your recording");
                                        }
                                        Err(e) => toast.show(e.0),
                                    }
                                    recasting.set(None);
                                    load_voices();
                                });
                            }
                        };
                        view! {
                            <div class="brief-host">
                                <input type="text" class="host-name" prop:value=h.name.clone() aria-label="Name"
                                    on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| if let Some(x) = s.hosts.get_mut(i) { x.name = v })) }/>
                                <textarea rows="2" prop:value=h.voice.clone() aria-label="How the voice sounds"
                                    on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| if let Some(x) = s.hosts.get_mut(i) { x.voice = v })) }></textarea>
                                <div class="host-voice">
                                    <label class="field">
                                        <span>"Engine"</span>
                                        <select on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| if let Some(x) = s.hosts.get_mut(i) { x.engine = v })) }>
                                            {duet_common::podcast::ENGINES.iter().map(|(v, label)| view! { <option value=*v selected=engine == *v>{*label}</option> }).collect_view()}
                                        </select>
                                    </label>
                                    <div class="field">
                                        <span>"Voice sample"</span>
                                        <div class="sample-input">
                                            {if uploaded {
                                                view! {
                                                    <span class="sample-chip">"Your recording"</span>
                                                    <button class="link" disabled=move || recasting.get().is_some() on:click=move |_| set_sample2(String::new())>"Remove"</button>
                                                }.into_any()
                                            } else {
                                                view! {
                                                    <input type="url" placeholder="Paste a link to a recording…" prop:value=sample.clone()
                                                        on:change=move |ev| set_sample(event_target_value(&ev))/>
                                                }.into_any()
                                            }}
                                            <label class="button upload" class:disabled=move || recasting.get().is_some()>
                                                "Upload"
                                                <input type="file" accept="audio/*,video/*" hidden on:change=upload/>
                                            </label>
                                        </div>
                                    </div>
                                </div>
                                <div class="host-actions">
                                    <Show when=clip.clone() fallback=|| view! { <small class="hint">"Voice not made yet (it's made with the first episode)"</small> }>
                                        <audio controls preload="none" src=format!("/api/podcast/voices/{}/clip", name2)></audio>
                                    </Show>
                                    <button disabled=move || recasting.get().is_some() on:click={
                                        let name = name3.clone();
                                        move |_| {
                                            let name = name.clone();
                                            recasting.set(Some(name.clone()));
                                            spawn_local(async move {
                                                match api::post::<_, ()>("/api/podcast/voices/recast", &serde_json::json!({ "name": name })).await {
                                                    Ok(()) => toast.show("New voice made"),
                                                    Err(e) => toast.show(e.0),
                                                }
                                                recasting.set(None);
                                                load_voices();
                                            });
                                        }
                                    }>{move || if recasting.get().as_deref() == Some(name4.as_str()) { "Making the voice…" } else { "Make the voice again" }}</button>
                                    <Show when=move || { s().hosts.len() > 1 }>
                                        <button class="link" on:click=move |_| save(Box::new(move |s| { if i < s.hosts.len() { s.hosts.remove(i); } }))>"Remove"</button>
                                    </Show>
                                </div>
                            </div>
                        }
                    }).collect_view()}
                </section>
            </div>
        </Show>
    }
}

const KINDS: [(&str, &str, &str); 9] = [
    ("reddit", "Reddit", "subreddit, or your front-page feed address"),
    ("x", "X", "home, an account, or list/<id>"),
    ("instagram", "Instagram", "home, or an account"),
    ("bluesky", "Bluesky", "handle.bsky.social"),
    ("mastodon", "Mastodon", "@name@instance or #tag@instance"),
    ("youtube", "YouTube", "@channel"),
    ("hackernews", "Hacker News", "best"),
    ("lobsters", "Lobsters", "hottest, or a tag"),
    ("feed", "RSS / website", "https://example.com/feed.xml"),
];

fn kind_name(kind: &str) -> &'static str {
    KINDS.iter().find(|k| k.0 == kind).map(|k| k.1).unwrap_or("Feed")
}

fn source_label(s: &PodcastSource) -> String {
    if let Some(c) = CATALOG.iter().find(|c| c.kind == s.kind && c.value == s.value) {
        return c.name.to_string();
    }
    s.value.clone()
}

fn site_name(site: &str) -> &'static str {
    match site {
        "x" => "X",
        "instagram" => "Instagram",
        "reddit" => "Reddit",
        _ => "the site",
    }
}

/// Logging in to the sites read through the remote browser: the login page opens in a tab of
/// the remote Firefox, shown here, and duet checks it worked.
#[component]
fn Logins(settings: RwSignal<Option<PodcastSettings>>, save: impl Fn(Box<dyn FnOnce(&mut PodcastSettings)>) + Copy + Send + Sync + 'static) -> impl IntoView {
    let toast = toast();
    let open = RwSignal::new(None::<&'static str>);
    let status = RwSignal::new(std::collections::HashMap::<&'static str, Option<bool>>::new());
    let busy = RwSignal::new(false);
    // The duet-wide browser screen address (Settings → Free sources) is
    // the shared default, so this viewer agrees with Discover's; a podcast
    // setting here overrides it for this section only.
    let shared_viewer = RwSignal::new(String::new());
    spawn_local(async move {
        if let Ok(s) = api::get::<duet_common::Settings>("/api/settings").await {
            shared_viewer.set(s.annas_archive.viewer_url);
        }
    });
    let viewer = move || {
        let configured = settings.get().map(|s| s.viewer_url).unwrap_or_default();
        if !configured.trim().is_empty() {
            return configured;
        }
        let shared = shared_viewer.get();
        if !shared.trim().is_empty() {
            return shared;
        }
        let loc = web_sys::window().and_then(|w| w.location().href().ok()).unwrap_or_default();
        let (proto, host) = match loc.split_once("://") {
            Some((p, rest)) => (p.to_string(), rest.split('/').next().unwrap_or("localhost").split(':').next().unwrap_or("localhost").to_string()),
            None => ("http".into(), "localhost".into()),
        };
        format!("{proto}://{host}:3001/")
    };
    let check = move |site: &'static str| {
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, bool>("/api/podcast/browser/check", &serde_json::json!({ "site": site })).await {
                Ok(ok) => {
                    status.update(|m| {
                        m.insert(site, Some(ok));
                    });
                    let name = site_name(site);
                    toast.show(if ok { format!("Logged in to {name}") } else { format!("{name} login not found — try again") });
                }
                Err(e) => toast.show(e.0),
            }
            busy.set(false);
        });
    };
    let login = move |site: &'static str| {
        busy.set(true);
        open.set(Some(site));
        spawn_local(async move {
            if let Err(e) = api::post::<_, ()>("/api/podcast/browser/login", &serde_json::json!({ "site": site })).await {
                toast.show(e.0);
            }
            busy.set(false);
        });
    };
    view! {
        <div class="brief-logins">
            <h4>"Logins (for X and Instagram)"</h4>
            <p class="hint">"These are read in your remote Firefox (on the VPN), logged in as you. Log in once; duet opens its own tab to read your feed each morning and closes it after."</p>
            <div class="login-row">
                {[("x", "X"), ("instagram", "Instagram"), ("reddit", "Reddit")].map(|(site, name)| view! {
                    <div class="login-site">
                        <span>{name}</span>
                        <small>{move || match status.get().get(site).copied().flatten() { Some(true) => "Logged in", Some(false) => "Not logged in", None => "" }}</small>
                        <button disabled=move || busy.get() on:click=move |_| login(site)>{format!("Log in to {name}")}</button>
                        <button class="link" disabled=move || busy.get() on:click=move |_| check(site)>"Check"</button>
                    </div>
                })}
            </div>
            <Show when=move || open.get().is_some()>
                <div class="login-viewer">
                    <div class="login-bar">
                        <span>{move || format!("Log in to {} below, then press Done.", open.get().map(site_name).unwrap_or("the site"))}</span>
                        // Done hides the browser at once; checking means switching the browser
                        // back to driven mode first (a restart), so it runs behind.
                        <button class="primary" on:click=move |_| {
                            if let Some(s) = open.get_untracked() {
                                open.set(None);
                                toast.show(format!("Checking your {} login…", site_name(s)));
                                check(s);
                            }
                        }>"Done"</button>
                        <button class="icon-button" aria-label="Close" on:click=move |_| open.set(None)>"×"</button>
                    </div>
                    <iframe src=viewer title="Remote Firefox" allow="clipboard-read; clipboard-write"></iframe>
                </div>
            </Show>
            <details class="brief-advanced">
                <summary>"Advanced"</summary>
                <label class="field">
                    <span>"Remote browser WebDriver"</span>
                    <input type="url" placeholder="http://127.0.0.1:4444" prop:value=move || settings.get().map(|s| s.webdriver_url).unwrap_or_default()
                        on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| s.webdriver_url = v)) }/>
                    <small>"geckodriver attached to the remote Firefox. Empty: don't use the browser."</small>
                </label>
                <label class="field">
                    <span>"Remote browser viewer"</span>
                    <input type="url" placeholder="(Discover's browser screen address)" prop:value=move || settings.get().map(|s| s.viewer_url).unwrap_or_default()
                        on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| s.viewer_url = v)) }/>
                    <small>"Empty: use Discover's browser screen (the aa.* address), which works on phones too."</small>
                </label>
                <label class="field">
                    <span>"RSSHub (optional fallback)"</span>
                    <input type="url" placeholder="http://localhost:1200" prop:value=move || settings.get().map(|s| s.rsshub_url).unwrap_or_default()
                        on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| s.rsshub_url = v)) }/>
                    <small>"Used for X and Instagram if the browser can't read them."</small>
                </label>
            </details>
        </div>
    }
}
