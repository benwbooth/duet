//! Podcasts: the shows you follow, finding new ones, and a player that keeps playing as you move
//! around duet (a bar along the bottom that opens into a full now-playing screen) and skips the
//! ads the server has found.

use duet_common::shows::{AdScan, Chart, GENRES, Outline, Show, ShowEpisode, ShowHit, ShowPage, ShowPosition, ShowSettings, ShowsHome};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;

use crate::{api, format_time, go, hue_of, icon, icons, toast};

#[wasm_bindgen(inline_js = r#"
let el = null, ads = [], skipping = true, allowed = new Set(), ours = false, sleepAt = 0, sleepEnd = false;
let cb = { tick: null, state: null, skip: null };
function st(s) { if (cb.state) cb.state(s); }
function inAd(t) {
  for (let i = 0; i < ads.length; i++) {
    const a = ads[i];
    if (!allowed.has(a.start) && t >= a.start - 0.25 && t < a.end - 0.75) return i;
  }
  return -1;
}
function ms() {
  if (!("mediaSession" in navigator) || !el || !isFinite(el.duration)) return;
  try { navigator.mediaSession.setPositionState({ duration: el.duration, playbackRate: el.playbackRate, position: Math.min(el.currentTime, el.duration) }); } catch (_) {}
}
export function spInit(onTick, onState, onSkip) {
  cb = { tick: onTick, state: onState, skip: onSkip };
  if (el) return;
  el = new Audio();
  el.preload = "auto";
  el.addEventListener("timeupdate", () => {
    const t = el.currentTime;
    if (sleepAt && Date.now() >= sleepAt) { sleepAt = 0; el.pause(); }
    if (skipping) {
      const i = inAd(t);
      if (i >= 0) {
        ours = true;
        el.currentTime = ads[i].end;
        if (cb.skip) cb.skip(i, ads[i].end - Math.max(t, ads[i].start));
      }
    }
    if (cb.tick) cb.tick(el.currentTime, isFinite(el.duration) ? el.duration : 0);
  });
  // (Seeking into an ad skips it too, with Undo; only "play this ad" lets one play.)
  el.addEventListener("seeking", () => { ours = false; });
  el.addEventListener("playing", () => { st("playing"); ms(); });
  el.addEventListener("pause", () => { if (!el.ended) st("paused"); });
  el.addEventListener("waiting", () => st("waiting"));
  el.addEventListener("ended", () => { sleepAt = 0; sleepEnd = false; st("ended"); });
  el.addEventListener("loadedmetadata", () => { if (cb.tick) cb.tick(el.currentTime, el.duration); ms(); });
  el.addEventListener("error", () => st("error"));
  if ("mediaSession" in navigator) {
    const m = navigator.mediaSession;
    m.setActionHandler("play", () => el.play().catch(() => {}));
    m.setActionHandler("pause", () => el.pause());
    try {
      m.setActionHandler("seekbackward", (d) => spNudge(-(d.seekOffset || 15)));
      m.setActionHandler("seekforward", (d) => spNudge(d.seekOffset || 30));
      m.setActionHandler("seekto", (d) => spSeek(d.seekTime));
      m.setActionHandler("previoustrack", () => spNudge(-15));
      m.setActionHandler("nexttrack", () => spNudge(30));
    } catch (_) {}
  }
}
export function spLoad(src, start, rate, play) {
  allowed = new Set();
  ads = [];
  el.src = src;
  el.playbackRate = rate;
  const go = () => { if (start > 0) { ours = true; el.currentTime = start; } if (play) el.play().catch(() => st("paused")); };
  if (el.readyState >= 1) go(); else el.addEventListener("loadedmetadata", go, { once: true });
  st(play ? "waiting" : "paused");
}
export function spPlay() { if (el && el.src) el.play().catch(() => st("paused")); }
export function spPause() { if (el) el.pause(); }
export function spPlaying() { return !!(el && el.src && !el.paused); }
export function spSeek(t) {
  if (!el) return;
  const d = isFinite(el.duration) ? el.duration : t;
  el.currentTime = Math.max(0, Math.min(t, d));
  ms();
}
export function spNudge(d) { if (el) spSeek(el.currentTime + d); }
export function spRate(r) { if (el) { el.playbackRate = r; ms(); } }
export function spTime() { return el ? el.currentTime : 0; }
export function spAds(json, on) { ads = JSON.parse(json); skipping = on; }
// Undo a skip: back to where the ad started, and it plays this time.
export function spAllow(i) { if (!ads[i]) return; allowed.add(ads[i].start); ours = true; el.currentTime = ads[i].start; }
export function spStop() { if (el) { el.pause(); el.removeAttribute("src"); el.load(); } }
// Stop after `mins` minutes (0: off).
export function spSleep(mins) { sleepAt = mins > 0 ? Date.now() + mins * 60000 : 0; }
export function spSleepLeft() { return sleepAt ? Math.max(0, (sleepAt - Date.now()) / 1000) : 0; }
export function spMeta(title, artist, art) {
  if (!("mediaSession" in navigator)) return;
  navigator.mediaSession.metadata = new MediaMetadata({ title, artist, album: artist, artwork: art ? [{ src: art, sizes: "600x600" }] : [] });
}
// "Today", "Yesterday", "Tuesday", "Nov 3", "Nov 3, 2023".
export function spDate(iso) {
  const d = new Date(iso);
  if (!iso || isNaN(d)) return "";
  const now = new Date();
  const days = Math.floor((new Date(now.getFullYear(), now.getMonth(), now.getDate()) - new Date(d.getFullYear(), d.getMonth(), d.getDate())) / 864e5);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return d.toLocaleDateString(undefined, { weekday: "long" });
  return d.toLocaleDateString(undefined, d.getFullYear() === now.getFullYear() ? { month: "short", day: "numeric" } : { month: "short", day: "numeric", year: "numeric" });
}
// Show notes from a feed, made safe to show: only plain formatting and web links survive, and
// times ("12:34") become links that jump there.
export function spCleanNotes(html) {
  const doc = new DOMParser().parseFromString("<div>" + (html || "") + "</div>", "text/html");
  const keep = new Set(["A", "P", "BR", "UL", "OL", "LI", "B", "STRONG", "I", "EM", "H1", "H2", "H3", "H4", "BLOCKQUOTE"]);
  const walk = (node) => {
    for (const c of [...node.childNodes]) {
      if (c.nodeType === 1) {
        if (["SCRIPT", "STYLE", "IFRAME", "OBJECT", "NOSCRIPT"].includes(c.tagName)) { c.remove(); continue; }
        walk(c);
        if (!keep.has(c.tagName)) { c.replaceWith(...c.childNodes); continue; }
        for (const a of [...c.attributes]) if (!(c.tagName === "A" && a.name === "href")) c.removeAttribute(a.name);
        if (c.tagName === "A") {
          const h = c.getAttribute("href") || "";
          if (!/^https?:/i.test(h)) c.removeAttribute("href");
          else { c.setAttribute("target", "_blank"); c.setAttribute("rel", "noopener noreferrer"); }
        }
      } else if (c.nodeType === 3) {
        const parts = c.textContent.split(/(\b(?:\d{1,2}:)?\d{1,2}:\d{2}\b)/);
        if (parts.length > 1 && !(c.parentNode && c.parentNode.tagName === "A")) {
          const frag = document.createDocumentFragment();
          parts.forEach((p, i) => {
            if (i % 2) {
              const a = document.createElement("a");
              a.textContent = p;
              a.className = "ts";
              a.dataset.t = p.split(":").reduce((s, x) => s * 60 + parseInt(x, 10), 0);
              frag.append(a);
            } else if (p) frag.append(p);
          });
          c.replaceWith(frag);
        }
      } else c.remove();
    }
  };
  const root = doc.body.firstChild;
  walk(root);
  return root.innerHTML;
}
// A timestamp link clicked in the notes: its time in seconds, or -1.
export function spTsAt(ev) {
  const a = ev.target && ev.target.closest ? ev.target.closest("a.ts") : null;
  if (!a) return -1;
  ev.preventDefault();
  return parseFloat(a.dataset.t) || 0;
}
export function spDownload(url) { const a = document.createElement("a"); a.href = url; a.download = ""; document.body.append(a); a.click(); a.remove(); }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = spInit)]
    fn sp_init(on_tick: &js_sys::Function, on_state: &js_sys::Function, on_skip: &js_sys::Function);
    #[wasm_bindgen(js_name = spLoad)]
    fn sp_load(src: &str, start: f64, rate: f64, play: bool);
    #[wasm_bindgen(js_name = spPlay)]
    fn sp_play();
    #[wasm_bindgen(js_name = spPause)]
    fn sp_pause();
    #[wasm_bindgen(js_name = spPlaying)]
    fn sp_playing() -> bool;
    #[wasm_bindgen(js_name = spSeek)]
    fn sp_seek(t: f64);
    #[wasm_bindgen(js_name = spNudge)]
    fn sp_nudge(d: f64);
    #[wasm_bindgen(js_name = spRate)]
    fn sp_rate(r: f64);
    #[wasm_bindgen(js_name = spTime)]
    fn sp_time() -> f64;
    #[wasm_bindgen(js_name = spAds)]
    fn sp_ads(json: &str, on: bool);
    #[wasm_bindgen(js_name = spAllow)]
    fn sp_allow(i: usize);
    #[wasm_bindgen(js_name = spStop)]
    fn sp_stop();
    #[wasm_bindgen(js_name = spSleep)]
    fn sp_sleep(mins: f64);
    #[wasm_bindgen(js_name = spSleepLeft)]
    fn sp_sleep_left() -> f64;
    #[wasm_bindgen(js_name = spMeta)]
    fn sp_meta(title: &str, artist: &str, art: &str);
    #[wasm_bindgen(js_name = spDate)]
    fn sp_date(iso: &str) -> String;
    #[wasm_bindgen(js_name = spCleanNotes)]
    fn sp_clean_notes(html: &str) -> String;
    #[wasm_bindgen(js_name = spTsAt)]
    fn sp_ts_at(ev: &web_sys::MouseEvent) -> f64;
    #[wasm_bindgen(js_name = spDownload)]
    fn sp_download(url: &str);
}

// ---- The player, shared by every page ----

/// What's playing, kept in the app so it carries on while you look at other pages.
#[derive(Clone, Copy)]
pub struct Player {
    pub ep: RwSignal<Option<ShowEpisode>>,
    pub time: RwSignal<f64>,
    pub duration: RwSignal<f64>,
    /// "playing", "paused", "waiting", "ended", "error", or "" (nothing loaded).
    pub state: RwSignal<String>,
    /// The full now-playing screen is open.
    pub open: RwSignal<bool>,
    pub prefs: RwSignal<ShowSettings>,
    /// Getting the episode ready before it plays (downloading it, so its ads can be skipped).
    pub preparing: RwSignal<Option<AdScan>>,
    /// The ad just skipped (its index and length), for the "Skipped · Undo" note.
    pub skipped: RwSignal<Option<(usize, f64)>>,
    /// Seconds of ads skipped in this episode.
    pub saved: RwSignal<f64>,
    /// Which of the episode's ads have been skipped so far (their indexes).
    pub skipped_ads: RwSignal<Vec<usize>>,
    /// Sleep timer: minutes chosen, or -1 for "end of episode".
    pub sleep: RwSignal<i32>,
    pub rate: RwSignal<f64>,
    /// Bumped whenever an episode's played state changes, so lists reload.
    pub changed: RwSignal<u32>,
}

pub fn player() -> Player {
    expect_context::<Player>()
}

impl Player {
    pub fn active(&self) -> bool {
        self.ep.with(Option::is_some)
    }

    fn playing(&self) -> bool {
        matches!(self.state.get().as_str(), "playing" | "waiting")
    }

    /// Ads are skipped in this episode (on in settings, and for its show).
    fn skipping(&self) -> bool {
        self.prefs.with(|p| p.skip_ads) && self.ep.with(|e| e.as_ref().is_some_and(|e| e.show_skip_ads && e.id != 0))
    }

    fn send_ads(&self) {
        let (ads, on) = (self.ep.with_untracked(|e| e.as_ref().map(|e| e.ads.ads.clone()).unwrap_or_default()), self.skipping_untracked());
        sp_ads(&serde_json::to_string(&ads).unwrap_or_else(|_| "[]".into()), on);
    }

    fn skipping_untracked(&self) -> bool {
        self.prefs.with_untracked(|p| p.skip_ads) && self.ep.with_untracked(|e| e.as_ref().is_some_and(|e| e.show_skip_ads && e.id != 0))
    }

    /// Saves where you are (and whether you finished).
    fn save(&self, finished: bool) {
        let Some(id) = self.ep.with_untracked(|e| e.as_ref().map(|e| e.id)).filter(|id| *id != 0) else { return };
        let pos = ShowPosition { position: sp_time(), duration: self.duration.get_untracked(), finished };
        let changed = self.changed;
        spawn_local(async move {
            let _ = api::put::<_, ()>(&format!("/api/shows/episodes/{id}/position"), &pos).await;
            if finished {
                changed.update(|n| *n += 1);
            }
        });
    }

    /// Plays an episode: from where you left it, ad-free when its ads can be found.
    pub fn play(&self, ep: ShowEpisode) {
        let p = *self;
        if p.ep.with_untracked(|e| e.as_ref().is_some_and(|e| e.id == ep.id && e.audio_url == ep.audio_url)) && p.preparing.get_untracked().is_none() {
            if sp_playing() {
                sp_pause();
            } else {
                sp_play();
            }
            return;
        }
        if p.active() {
            p.save(false);
        }
        sp_pause();
        p.skipped.set(None);
        p.saved.set(0.0);
        p.skipped_ads.set(Vec::new());
        p.preparing.set(None);
        p.time.set(ep.position);
        p.duration.set(ep.duration);
        p.ep.set(Some(ep.clone()));
        p.state.set("waiting".into());
        sp_meta(&ep.title, &ep.show_title, ep.image.as_deref().unwrap_or(""));
        spawn_local(async move {
            // (Not followed: straight from the show's server, nothing saved.)
            if ep.id == 0 {
                if ep.needs_download {
                    toast().show("Follow this channel to listen (its sound is pulled from YouTube first)");
                    p.ep.set(None);
                    p.state.set(String::new());
                    return;
                }
                sp_load(&ep.audio_url, 0.0, p.rate.get_untracked(), true);
                return;
            }
            let ep = api::get::<ShowEpisode>(&format!("/api/shows/episodes/{}", ep.id)).await.unwrap_or(ep);
            let id = ep.id;
            let start = if ep.finished { 0.0 } else { ep.position };
            p.ep.set(Some(ep.clone()));
            let skip = p.skipping_untracked();
            if (skip && ep.ads.state != "done") || (ep.needs_download && !ep.downloaded) {
                let _ = api::post::<_, AdScan>(&format!("/api/shows/episodes/{id}/prepare"), &serde_json::json!({ "now": true })).await;
            }
            if (skip || ep.needs_download) && !ep.downloaded {
                // Its ads are found in the downloaded copy, so that's the one that plays.
                p.preparing.set(Some(ep.ads.clone()));
                loop {
                    gloo_timers::future::TimeoutFuture::new(700).await;
                    if p.ep.with_untracked(|e| e.as_ref().map(|e| e.id)) != Some(id) || p.preparing.get_untracked().is_none() {
                        return;
                    }
                    let Ok(now) = api::get::<ShowEpisode>(&format!("/api/shows/episodes/{id}")).await else { continue };
                    if now.downloaded {
                        p.ep.set(Some(now));
                        break;
                    }
                    if now.ads.state == "failed" && now.needs_download {
                        toast().show(format!("Couldn't get it from YouTube: {}", now.ads.error.clone().unwrap_or_default()));
                        p.preparing.set(None);
                        p.state.set("paused".into());
                        return;
                    }
                    if now.ads.state == "failed" {
                        toast().show(format!("Couldn't download it: {}. Playing it from the show's site.", now.ads.error.clone().unwrap_or_default()));
                        p.preparing.set(None);
                        sp_load(&now.audio_url, start, p.rate.get_untracked(), true);
                        return;
                    }
                    p.preparing.set(Some(now.ads));
                }
                p.preparing.set(None);
            }
            sp_load(&format!("/api/shows/episodes/{id}/audio"), start, p.rate.get_untracked(), true);
            p.send_ads();
        });
    }

    /// Stop waiting for the download: play it now from the show's site, ads and all.
    fn play_without_skipping(&self) {
        let Some(ep) = self.ep.get_untracked() else { return };
        self.preparing.set(None);
        sp_load(&ep.audio_url, if ep.finished { 0.0 } else { ep.position }, self.rate.get_untracked(), true);
        sp_ads("[]", false);
    }

    pub fn toggle(&self) {
        if self.preparing.get_untracked().is_some() {
            return;
        }
        if sp_playing() {
            sp_pause();
        } else {
            sp_play();
        }
    }

    fn close(&self) {
        self.save(false);
        sp_stop();
        self.ep.set(None);
        self.state.set(String::new());
        self.open.set(false);
        self.preparing.set(None);
    }
}

/// Sets up the player (once, in the app).
pub fn provide_player() -> Player {
    let p = Player {
        ep: RwSignal::new(None),
        time: RwSignal::new(0.0),
        duration: RwSignal::new(0.0),
        state: RwSignal::new(String::new()),
        open: RwSignal::new(false),
        prefs: RwSignal::new(ShowSettings::default()),
        preparing: RwSignal::new(None),
        skipped: RwSignal::new(None),
        saved: RwSignal::new(0.0),
        skipped_ads: RwSignal::new(Vec::new()),
        sleep: RwSignal::new(0),
        rate: RwSignal::new(1.0),
        changed: RwSignal::new(0),
    };
    provide_context(p);
    let last_saved = StoredValue::new(0.0f64);
    let on_tick = Closure::<dyn Fn(f64, f64)>::new(move |t: f64, d: f64| {
        p.time.set(t);
        if d > 0.0 {
            p.duration.set(d);
        }
        if (t - last_saved.get_value()).abs() > 15.0 {
            last_saved.set_value(t);
            p.save(false);
        }
    })
    .into_js_value();
    let on_state = Closure::<dyn Fn(String)>::new(move |s: String| {
        let was = p.state.get_untracked();
        if s == "ended" {
            p.save(true);
            if p.sleep.get_untracked() == -1 {
                p.sleep.set(0);
            }
            p.ep.update(|e| {
                if let Some(e) = e {
                    e.finished = true;
                }
            });
        } else if s == "paused" && was == "playing" {
            p.save(false);
        } else if s == "error" && p.ep.with_untracked(Option::is_some) {
            toast().show("This episode won't play. The show's server may be down.");
        }
        p.state.set(s);
    })
    .into_js_value();
    let skip_note = StoredValue::new(0u32);
    let on_skip = Closure::<dyn Fn(f64, f64)>::new(move |i: f64, secs: f64| {
        p.skipped.set(Some((i as usize, secs)));
        p.saved.update(|s| *s += secs);
        p.skipped_ads.update(|v| {
            if !v.contains(&(i as usize)) {
                v.push(i as usize);
            }
        });
        skip_note.update_value(|n| *n += 1);
        let n = skip_note.get_value();
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(8000).await;
            if skip_note.get_value() == n {
                p.skipped.set(None);
            }
        });
    })
    .into_js_value();
    sp_init(on_tick.unchecked_ref(), on_state.unchecked_ref(), on_skip.unchecked_ref());
    spawn_local(async move {
        if let Ok(s) = api::get::<ShowSettings>("/api/shows/settings").await {
            p.rate.set(s.speed);
            p.prefs.set(s);
        }
    });
    // While ads are still being found in what's playing, the new ones are picked up.
    spawn_local(async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(3000).await;
            let Some(ep) = p.ep.get_untracked() else { continue };
            if ep.id == 0 || p.preparing.get_untracked().is_some() || !(ep.ads.busy() || ep.ads.state.is_empty() && p.skipping_untracked()) {
                continue;
            }
            if let Ok(scan) = api::get::<AdScan>(&format!("/api/shows/episodes/{}/ads", ep.id)).await {
                if scan != ep.ads && p.ep.with_untracked(|e| e.as_ref().map(|e| e.id)) == Some(ep.id) {
                    p.ep.update(|e| {
                        if let Some(e) = e {
                            e.ads = scan;
                        }
                    });
                    p.send_ads();
                }
            }
        }
    });
    // Skipping switched on or off.
    Effect::new(move |_| {
        let _ = p.prefs.with(|x| x.skip_ads);
        p.send_ads();
    });
    p
}

// ---- Bits ----

fn art(url: Option<String>, title: &str, class: &'static str) -> impl IntoView + use<> {
    match url {
        Some(u) => view! { <img class=format!("pod-art {class}") src=u alt="" loading="lazy" decoding="async" referrerpolicy="no-referrer"/> }.into_any(),
        None => {
            let hue = hue_of(title);
            let initial: String = title.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_default();
            view! { <div class=format!("pod-art placeholder {class}") style=format!("--hue:{hue}")><span>{initial}</span></div> }.into_any()
        }
    }
}

/// "1 hr 5 min", "42 min", or "12 min left".
fn length(secs: f64) -> String {
    let m = (secs / 60.0).round() as i64;
    if m >= 60 { format!("{} hr {} min", m / 60, m % 60) } else { format!("{} min", m.max(1)) }
}

fn ad_summary(scan: &AdScan) -> Option<String> {
    match scan.state.as_str() {
        "done" if scan.ads.is_empty() => Some("No ads found".into()),
        "done" => Some(format!("Ad-free: {} ad{} ({}) will be skipped", scan.ads.len(), if scan.ads.len() == 1 { "" } else { "s" }, format_time(scan.ad_seconds()))),
        "queued" => Some("Waiting to check for ads".into()),
        "downloading" | "listening" => Some(if scan.doing.is_empty() { "Finding ads…".into() } else { scan.doing.clone() }),
        "failed" => Some("Couldn't check for ads".into()),
        _ => None,
    }
}

const PLAY_PATH: &str = "M8 5.5v13l10.5-6.5z";
const PAUSE_PATH: &str = "M7 5h3.5v14H7zM13.5 5H17v14h-3.5z";

fn back_icon(secs: u32) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" class="icon skip-icon" aria-hidden="true">
            <path d="M4.5 12a7.5 7.5 0 1 0 2.3-5.4" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>
            <path d="M4 3.5v4.4h4.4" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>
            <text x="12.4" y="15.6" text-anchor="middle">{secs}</text>
        </svg>
    }
}

fn forward_icon(secs: u32) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" class="icon skip-icon" aria-hidden="true">
            <path d="M19.5 12a7.5 7.5 0 1 1-2.3-5.4" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>
            <path d="M20 3.5v4.4h-4.4" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>
            <text x="11.6" y="15.6" text-anchor="middle">{secs}</text>
        </svg>
    }
}

fn play_glyph(playing: bool) -> impl IntoView {
    view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d=if playing { PAUSE_PATH } else { PLAY_PATH } fill="currentColor"/></svg> }
}

// ---- Pages ----

#[derive(Clone, Debug, PartialEq)]
pub enum PodRoute {
    Home,
    Show(i64),
    /// A show that isn't followed, by its feed address.
    Feed(String),
    /// The charts: all shows, or one category.
    Discover(Option<i64>),
}

impl PodRoute {
    pub fn parse(rest: &[&str]) -> PodRoute {
        match rest {
            ["show", id, ..] => id.parse().map(PodRoute::Show).unwrap_or(PodRoute::Home),
            ["discover", genre, ..] => PodRoute::Discover(genre.parse().ok()),
            ["discover"] => PodRoute::Discover(None),
            ["feed", url, ..] => PodRoute::Feed(js_sys::decode_uri_component(url).ok().and_then(|s| s.as_string()).unwrap_or_default()),
            _ => PodRoute::Home,
        }
    }
}

fn feed_link(url: &str) -> String {
    format!("#/podcasts/feed/{}", String::from(js_sys::encode_uri_component(url)))
}

#[component]
pub fn Podcasts(route: PodRoute) -> impl IntoView {
    match route {
        PodRoute::Home => view! { <Home/> }.into_any(),
        PodRoute::Show(id) => view! { <ShowView id=Some(id) feed=None/> }.into_any(),
        PodRoute::Discover(genre) => view! { <Discover genre/> }.into_any(),
        PodRoute::Feed(url) => view! { <ShowView id=None feed=Some(url)/> }.into_any(),
    }
}

#[component]
fn Home() -> impl IntoView {
    let toast = toast();
    let p = player();
    let home = RwSignal::new(None::<ShowsHome>);
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(None::<Vec<ShowHit>>);
    let searching = RwSignal::new(false);
    let top = RwSignal::new(Vec::<ShowHit>::new());
    let refreshing = RwSignal::new(false);
    let show_add = RwSignal::new(false);
    let show_settings = RwSignal::new(false);
    let all_new = RwSignal::new(false);

    let load = move || {
        spawn_local(async move {
            match api::get::<ShowsHome>("/api/shows").await {
                Ok(h) => home.set(Some(h)),
                Err(e) => toast.show(e.0),
            }
        });
    };
    load();
    Effect::new(move |prev: Option<u32>| {
        let n = p.changed.get();
        if prev.is_some() {
            load();
        }
        n
    });
    spawn_local(async move {
        if let Ok(t) = api::get::<Vec<ShowHit>>("/api/shows/top").await {
            top.set(t);
        }
    });
    // While anything's being checked for ads, the page keeps up.
    let alive = StoredValue::new(true);
    on_cleanup(move || alive.set_value(false));
    spawn_local(async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(4000).await;
            if !alive.try_get_value().unwrap_or(false) {
                break;
            }
            let busy = home.with_untracked(|h| h.as_ref().is_some_and(|h| h.latest.iter().chain(&h.in_progress).any(|e| e.ads.busy())));
            if busy {
                if let Ok(h) = api::get::<ShowsHome>("/api/shows").await {
                    home.set(Some(h));
                }
            }
        }
    });

    let run_search = move || {
        let q = query.get_untracked().trim().to_string();
        if q.is_empty() {
            results.set(None);
            return;
        }
        // A pasted feed, Apple Podcasts link, or YouTube channel (@handle) opens the show.
        if q.starts_with("http") || q.contains("podcasts.apple.com") || q.contains("youtube.com/") || (q.starts_with('@') && !q.contains(' ')) {
            go(&feed_link(&q));
            return;
        }
        searching.set(true);
        spawn_local(async move {
            let url = format!("/api/shows/search?q={}", String::from(js_sys::encode_uri_component(&q)));
            match api::get::<Vec<ShowHit>>(&url).await {
                Ok(r) => results.set(Some(r)),
                Err(e) => toast.show(e.0),
            }
            searching.set(false);
        });
    };
    // Searches as you pause typing.
    let typed = StoredValue::new(0u32);
    let on_input = move |ev| {
        query.set(event_target_value(&ev));
        typed.update_value(|n| *n += 1);
        let n = typed.get_value();
        spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(450).await;
            if typed.get_value() == n {
                run_search();
            }
        });
    };
    let refresh = move |_| {
        refreshing.set(true);
        spawn_local(async move {
            match api::post::<_, ShowsHome>("/api/shows/refresh", &()).await {
                Ok(h) => home.set(Some(h)),
                Err(e) => toast.show(e.0),
            }
            refreshing.set(false);
        });
    };

    view! {
        <div class="pods">
            <header class="pods-head">
                <h1>"Podcasts"</h1>
                <div class="pods-head-actions">
                    <a class="button ghost pods-discover-btn" href="#/podcasts/discover" title="Top shows by category">{icon(icons::DISCOVER)}<span>"Discover"</span></a>
                    <button class="icon-button" title="Check for new episodes" aria-label="Check for new episodes" on:click=refresh disabled=move || refreshing.get()>
                        {move || if refreshing.get() { view! { <span class="spinner small"></span> }.into_any() } else { icon(icons::SYNC).into_any() }}
                    </button>
                    <button class="icon-button" title="Add a show by its address, or import from another app" aria-label="Add a show" on:click=move |_| show_add.set(true)>
                        <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>
                    </button>
                    <button class="icon-button" title="Podcast settings" aria-label="Podcast settings" on:click=move |_| show_settings.set(true)>{icon(icons::SETTINGS)}</button>
                </div>
            </header>
            <form class="search-box pods-search" on:submit=move |ev| { ev.prevent_default(); run_search() }>
                {icon(icons::DISCOVER)}
                <input type="search" placeholder="Search podcasts, or paste a feed, YouTube channel or @handle" prop:value=move || query.get() on:input=on_input/>
                <Show when=move || searching.get()><span class="spinner small"></span></Show>
            </form>
            {move || match results.get() {
                Some(hits) => view! {
                    <section class="pods-section">
                        <div class="section-head"><h2>"Results"</h2><button class="link" on:click=move |_| { query.set(String::new()); results.set(None) }>"Clear"</button></div>
                        {if hits.is_empty() { view! { <p class="muted">"No podcasts found."</p> }.into_any() } else { view! { <HitGrid hits on_follow=Callback::new(move |_| load())/> }.into_any() }}
                    </section>
                }.into_any(),
                None => view! {
                    {move || home.get().map(|h| {
                        let empty = h.shows.is_empty();
                        view! {
                            {(!h.in_progress.is_empty()).then(|| view! {
                                <section class="pods-section">
                                    <div class="section-head"><h2>"Continue listening"</h2></div>
                                    <div class="pods-continue">
                                        {h.in_progress.iter().cloned().map(|e| view! { <ContinueCard ep=e/> }).collect_view()}
                                    </div>
                                </section>
                            })}
                            {(!empty).then(|| view! {
                                <section class="pods-section">
                                    <div class="section-head"><h2>"Your shows"</h2></div>
                                    <div class="pods-shows">
                                        {h.shows.iter().cloned().map(|s| view! {
                                            <a class="pods-show" href=format!("#/podcasts/show/{}", s.id) title=s.title.clone()>
                                                {art(s.image.clone(), &s.title, "")}
                                                {(s.unplayed > 0).then(|| view! { <span class="pods-badge">{s.unplayed.min(99)}</span> })}
                                                <span class="pods-show-title">{s.title.clone()}</span>
                                            </a>
                                        }).collect_view()}
                                    </div>
                                </section>
                            })}
                            {(!h.latest.is_empty()).then(|| view! {
                                <section class="pods-section">
                                    <div class="section-head"><h2>"New episodes"</h2>
                                        {(h.latest.len() > 8).then(|| view! { <button class="link" on:click=move |_| all_new.update(|v| *v = !*v)>{move || if all_new.get() { "Show fewer" } else { "Show all" }}</button> })}
                                    </div>
                                    <ul class="pod-episodes">
                                        {h.latest.iter().take(if all_new.get() { 100 } else { 8 }).cloned().map(|e| view! { <EpisodeRow ep=e show_name=true/> }).collect_view()}
                                    </ul>
                                </section>
                            })}
                            {empty.then(|| view! {
                                <div class="pods-empty">
                                    <h2>"Find something to listen to"</h2>
                                    <p>"Search above, pick from what's popular below, or bring your shows over from another app. "
                                       "Ads in the shows you follow are found by AI and skipped as you listen."</p>
                                    <button class="ghost" on:click=move |_| show_add.set(true)>"Import from another app"</button>
                                </div>
                            })}
                        }
                    })}
                    <Show when=move || !top.with(Vec::is_empty)>
                        <section class="pods-section">
                            <div class="section-head"><h2>"Popular right now"</h2><a href="#/podcasts/discover">"Top charts by category"</a></div>
                            <HitGrid hits=top.get_untracked() on_follow=Callback::new(move |_| load())/>
                        </section>
                    </Show>
                }.into_any(),
            }}
            <Show when=move || show_add.get()>
                <leptos::portal::Portal>
                    <div class="brief-scrim" on:click=move |_| show_add.set(false)></div>
                    <aside class="brief-pane pods-pane" role="dialog" aria-label="Add a show">
                        <div class="pane-head"><h2>"Add a show"</h2><button class="icon-button" aria-label="Close" on:click=move |_| show_add.set(false)>"×"</button></div>
                        <AddShow on_done=Callback::new(move |_| { show_add.set(false); load() })/>
                    </aside>
                </leptos::portal::Portal>
            </Show>
            <Show when=move || show_settings.get()>
                <leptos::portal::Portal>
                    <div class="brief-scrim" on:click=move |_| show_settings.set(false)></div>
                    <aside class="brief-pane pods-pane" role="dialog" aria-label="Podcast settings">
                        <div class="pane-head"><h2>"Podcast settings"</h2><button class="icon-button" aria-label="Close" on:click=move |_| show_settings.set(false)>"×"</button></div>
                        <PodSettings/>
                    </aside>
                </leptos::portal::Portal>
            </Show>
        </div>
    }
}

#[component]
fn HitGrid(hits: Vec<ShowHit>, on_follow: Callback<()>, #[prop(optional)] ranked: bool) -> impl IntoView {
    let toast = toast();
    view! {
        <div class="pods-hits" class:ranked=ranked>
            {hits.into_iter().map(|h| {
                let followed = RwSignal::new(h.followed);
                let busy = RwSignal::new(false);
                let feed = h.feed_url.clone();
                let link = move || match followed.get() { Some(id) => format!("#/podcasts/show/{id}"), None => feed_link(&feed) };
                let url = h.feed_url.clone();
                let follow = move |ev: web_sys::MouseEvent| {
                    ev.prevent_default();
                    ev.stop_propagation();
                    if followed.get_untracked().is_some() || busy.get_untracked() {
                        return;
                    }
                    busy.set(true);
                    let url = url.clone();
                    spawn_local(async move {
                        match api::post::<_, Show>("/api/shows", &serde_json::json!({ "url": url })).await {
                            Ok(s) => {
                                toast.show(format!("Following {}", s.title));
                                followed.set(Some(s.id));
                                on_follow.run(());
                            }
                            Err(e) => toast.show(e.0),
                        }
                        busy.set(false);
                    });
                };
                view! {
                    <a class="pods-hit" href=link>
                        {(ranked && h.rank > 0).then(|| view! { <span class="hit-rank">{h.rank}</span> })}
                        {art(h.image.clone(), &h.title, "")}
                        <span class="hit-text">
                            <strong>{h.title.clone()}</strong>
                            <small>{h.author.clone()}</small>
                            <small class="muted">
                                {if h.genre.is_empty() { String::new() } else { h.genre.clone() }}
                                {(h.source == "podcastindex").then(|| view! { <span class="hit-source" title="Found on Podcast Index">"Podcast Index"</span> })}
                            </small>
                        </span>
                        <button class="follow-btn" class:following=move || followed.get().is_some() disabled=move || busy.get() on:click=follow
                            aria-label=move || if followed.get().is_some() { "Following" } else { "Follow" }>
                            {move || if busy.get() {
                                view! { <span class="spinner small"></span> }.into_any()
                            } else if followed.get().is_some() {
                                icon(icons::CHECK).into_any()
                            } else {
                                view! { <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 6v12M6 12h12"/></svg> }.into_any()
                            }}
                        </button>
                    </a>
                }
            }).collect_view()}
        </div>
    }
}

#[component]
fn ContinueCard(ep: ShowEpisode) -> impl IntoView {
    let p = player();
    let pct = if ep.duration > 0.0 { (ep.position / ep.duration * 100.0).clamp(0.0, 100.0) } else { 0.0 };
    let left = (ep.duration - ep.position).max(0.0);
    let id = ep.id;
    let e2 = ep.clone();
    let now = move || p.ep.with(|e| e.as_ref().is_some_and(|e| e.id == id));
    view! {
        <button class="pods-card" class:now=now on:click=move |_| p.play(e2.clone())>
            {art(ep.image.clone(), &ep.show_title, "")}
            <span class="card-text">
                <small>{ep.show_title.clone()}</small>
                <strong>{ep.title.clone()}</strong>
                <span class="card-foot">
                    <span class="card-play">{move || play_glyph(now() && p.playing())}</span>
                    <span class="mini-progress"><span style:width=format!("{pct:.1}%")></span></span>
                    <small>{if ep.duration > 0.0 { format!("{} left", length(left)) } else { String::new() }}</small>
                </span>
            </span>
        </button>
    }
}

#[component]
fn EpisodeRow(ep: ShowEpisode, #[prop(optional)] show_name: bool) -> impl IntoView {
    let p = player();
    let toast = toast();
    let ep = RwSignal::new(ep);
    let id = ep.with_untracked(|e| e.id);
    let open_notes = RwSignal::new(false);
    let now = move || id != 0 && p.ep.with(|e| e.as_ref().is_some_and(|e| e.id == id));
    // The playing one shows the player's place.
    let live = move || if now() { p.ep.get().map(|mut e| { e.position = p.time.get(); e }).unwrap_or_else(|| ep.get()) } else { ep.get() };
    let mark = move |finished: bool| {
        spawn_local(async move {
            let pos = ShowPosition { position: 0.0, duration: ep.with_untracked(|e| e.duration), finished };
            match api::put::<_, ()>(&format!("/api/shows/episodes/{id}/position"), &pos).await {
                Ok(()) => ep.update(|e| {
                    e.finished = finished;
                    e.position = 0.0;
                }),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let prepare = move |_| {
        spawn_local(async move {
            match api::post::<_, AdScan>(&format!("/api/shows/episodes/{id}/prepare"), &serde_json::json!({ "now": false })).await {
                Ok(s) => ep.update(|e| e.ads = s),
                Err(e) => toast.show(e.0),
            }
        });
    };
    view! {
        <li class="pod-ep" class:finished=move || ep.with(|e| e.finished) class:now=now>
            <button class="ep-main" on:click=move |_| open_notes.set(true)>
                {move || ep.with(|e| art(e.image.clone(), &e.show_title, "ep-art"))}
                <span class="ep-text">
                    <small class="ep-meta">
                        {move || ep.with(|e| {
                            let mut bits = Vec::new();
                            if show_name { bits.push(e.show_title.clone()); }
                            if let Some(d) = &e.published { bits.push(sp_date(d)); }
                            bits.join(" · ")
                        })}
                    </small>
                    <strong class="ep-title">{move || ep.with(|e| e.title.clone())}</strong>
                    <span class="ep-desc">{move || ep.with(|e| e.description.clone())}</span>
                </span>
            </button>
            <div class="ep-side">
                <button class="ep-play" class:playing=move || now() && p.playing() on:click=move |_| p.play(ep.get_untracked())
                    aria-label=move || if now() && p.playing() { "Pause" } else { "Play" }>
                    {move || {
                        let e = live();
                        let frac = if e.duration > 0.0 && !e.finished { (e.position / e.duration).clamp(0.0, 1.0) } else { 0.0 };
                        let c = 2.0 * std::f64::consts::PI * 15.0;
                        view! {
                            <svg viewBox="0 0 36 36" class="ring" aria-hidden="true">
                                <circle cx="18" cy="18" r="15" class="ring-bg"/>
                                {(frac > 0.005).then(|| view! { <circle cx="18" cy="18" r="15" class="ring-fg" stroke-dasharray=format!("{:.2} {:.2}", c * frac, c)/> })}
                                <path d=if now() && p.playing() { "M13.5 12h3v12h-3zM19.5 12h3v12h-3z" } else { "M15 11.5v13l10-6.5z" } class="ring-glyph"/>
                            </svg>
                        }
                    }}
                </button>
                <small class="ep-len">{move || {
                    let e = live();
                    if e.finished { "Played".to_string() }
                    else if e.duration <= 0.0 { String::new() }
                    else if e.position > 5.0 { format!("{} left", length(e.duration - e.position)) }
                    else { length(e.duration) }
                }}</small>
            </div>
            {move || {
                let e = live();
                let skip = e.show_skip_ads && p.prefs.with(|x| x.skip_ads);
                (id != 0 && skip).then(|| ad_summary(&e.ads)).flatten().map(|s| view! {
                    <small class="ep-ads" class:busy=e.ads.busy() class:clean=e.ads.state == "done">
                        {if e.ads.busy() { view! { <span class="spinner tiny"></span> }.into_any() } else { icon(icons::SCAN).into_any() }}
                        {s}
                    </small>
                })
            }}
            <Show when=move || open_notes.get()>
                <leptos::portal::Portal>
                    <div class="brief-scrim" on:click=move |_| open_notes.set(false)></div>
                    <aside class="brief-pane pods-pane ep-pane" role="dialog" aria-label="Episode">
                        <div class="pane-head"><h2>{move || ep.with(|e| e.show_title.clone())}</h2><button class="icon-button" aria-label="Close" on:click=move |_| open_notes.set(false)>"×"</button></div>
                        <div class="ep-pane-top">
                            {move || ep.with(|e| art(e.image.clone(), &e.show_title, "ep-pane-art"))}
                            <div>
                                <small class="muted">{move || ep.with(|e| { let mut b = Vec::new(); if let Some(d) = &e.published { b.push(sp_date(d)); } if e.duration > 0.0 { b.push(length(e.duration)); } b.join(" · ") })}</small>
                                <h3>{move || ep.with(|e| e.title.clone())}</h3>
                                <div class="ep-pane-actions">
                                    <button class="primary" on:click=move |_| { p.play(ep.get_untracked()); open_notes.set(false); p.open.set(true) }>
                                        {move || play_glyph(false)}{move || if ep.with(|e| e.position > 5.0 && !e.finished) { "Resume" } else { "Play" }}
                                    </button>
                                    <Show when=move || id != 0>
                                        <button class="ghost" on:click=move |_| mark(!ep.with_untracked(|e| e.finished))>
                                            {move || if ep.with(|e| e.finished) { "Mark unplayed" } else { "Mark played" }}
                                        </button>
                                        <Show when=move || ep.with(|e| e.ads.state.is_empty() || e.ads.state == "failed")>
                                            <button class="ghost" on:click=prepare title="Download it and find the ads now, so it's ready">"Get ready"</button>
                                        </Show>
                                    </Show>
                                </div>
                                {move || ep.with(|e| e.ads.error.clone()).map(|err| view! { <p class="brief-error small">{err}</p> })}
                            </div>
                        </div>
                        <Notes id ep=ep.into()/>
                    </aside>
                </leptos::portal::Portal>
            </Show>
        </li>
    }
}

/// An episode's AI outline, fetched again when `key` (its id and how far its check has got)
/// changes, and every few seconds while it's being made.
fn outline_for(key: Signal<(i64, String)>) -> RwSignal<Outline> {
    let outline = RwSignal::new(Outline::default());
    let asked = StoredValue::new(0u32);
    Effect::new(move |_| {
        let (id, _) = key.get();
        asked.update_value(|n| *n += 1);
        let me = asked.get_value();
        if id == 0 {
            outline.set(Outline::default());
            return;
        }
        spawn_local(async move {
            loop {
                let Ok(o) = api::get::<Outline>(&format!("/api/shows/episodes/{id}/outline")).await else { return };
                if asked.get_value() != me {
                    return;
                }
                let more = o.state == "making";
                outline.set(o);
                if !more {
                    return;
                }
                gloo_timers::future::TimeoutFuture::new(4000).await;
                if asked.get_value() != me {
                    return;
                }
            }
        });
    });
    outline
}

/// Asks for the outline to be made (or made again).
fn remake_outline(id: i64, outline: RwSignal<Outline>) {
    let toast = toast();
    spawn_local(async move {
        match api::post::<_, Outline>(&format!("/api/shows/episodes/{id}/outline"), &()).await {
            Ok(o) => {
                let making = o.state == "making";
                outline.set(o);
                // (Watch it until it's done.)
                while making {
                    gloo_timers::future::TimeoutFuture::new(4000).await;
                    let Ok(o) = api::get::<Outline>(&format!("/api/shows/episodes/{id}/outline")).await else { return };
                    let still = o.state == "making";
                    outline.set(o);
                    if !still {
                        break;
                    }
                }
            }
            Err(e) => toast.show(e.0),
        }
    });
}

/// The AI's few sentences on what the episode's about.
#[component]
fn EpisodeSummary(outline: RwSignal<Outline>) -> impl IntoView {
    move || {
        outline.with(|o| match o.state.as_str() {
            "done" if !o.summary.is_empty() => view! {
                <div class="ep-summary">
                    <div class="ep-summary-head"><span>"Summary"</span><span class="ai-badge" title="Written by AI from the transcript">"AI"</span></div>
                    <p>{o.summary.clone()}</p>
                </div>
            }
            .into_any(),
            "making" => view! { <div class="ep-summary muted"><span class="spinner tiny"></span>" Writing a summary and outline…"</div> }.into_any(),
            _ => ().into_any(),
        })
    }
}

/// An episode's show notes (fetched whole when they weren't in the list).
#[component]
fn Notes(id: i64, ep: Signal<ShowEpisode>) -> impl IntoView {
    let p = player();
    let html = RwSignal::new(ep.with_untracked(|e| e.notes_html.clone()));
    if html.with_untracked(String::is_empty) && id != 0 {
        spawn_local(async move {
            if let Ok(e) = api::get::<ShowEpisode>(&format!("/api/shows/episodes/{id}")).await {
                html.set(e.notes_html);
            }
        });
    }
    let on_click = move |ev: web_sys::MouseEvent| {
        let t = sp_ts_at(&ev);
        if t >= 0.0 {
            if !p.ep.with_untracked(|e| e.as_ref().is_some_and(|e| e.id == id)) {
                let mut e = ep.get_untracked();
                e.position = t;
                e.finished = false;
                p.play(e);
            } else {
                sp_seek(t);
                sp_play();
            }
        }
    };
    let outline = outline_for(Signal::derive(move || ep.with(|e| (id, e.ads.state.clone()))));
    view! {
        <div class="pod-notes-wrap">
            <EpisodeSummary outline/>
            <div class="pod-notes" on:click=on_click inner_html=move || {
                let h = html.get();
                if h.trim().is_empty() { ep.with(|e| e.description.clone()) } else { sp_clean_notes(&h) }
            }></div>
        </div>
    }
}

#[component]
fn ShowView(id: Option<i64>, feed: Option<String>) -> impl IntoView {
    let toast = toast();
    let p = player();
    let page = RwSignal::new(None::<ShowPage>);
    let failed = RwSignal::new(None::<String>);
    let more = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let filter = RwSignal::new("all");
    let feed = StoredValue::new(feed);
    let load = move || {
        spawn_local(async move {
            let url = match (id, feed.get_value()) {
                (Some(id), _) => format!("/api/shows/{id}"),
                (None, Some(f)) => format!("/api/shows/preview?url={}", String::from(js_sys::encode_uri_component(&f))),
                _ => return,
            };
            match api::get::<ShowPage>(&url).await {
                Ok(pg) => {
                    // (A followed show opened by its address: its own page.)
                    if id.is_none() && pg.show.id != 0 {
                        go(&format!("#/podcasts/show/{}", pg.show.id));
                    }
                    page.set(Some(pg))
                }
                Err(e) => failed.set(Some(e.0)),
            }
        });
    };
    load();
    Effect::new(move |prev: Option<u32>| {
        let n = p.changed.get();
        if prev.is_some() && id.is_some() {
            load();
        }
        n
    });
    let alive = StoredValue::new(true);
    on_cleanup(move || alive.set_value(false));
    spawn_local(async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(4000).await;
            if !alive.try_get_value().unwrap_or(false) {
                break;
            }
            if id.is_some() && page.with_untracked(|pg| pg.as_ref().is_some_and(|pg| pg.episodes.iter().any(|e| e.ads.busy()))) {
                load();
            }
        }
    });

    let follow = move |_| {
        let Some(url) = page.with_untracked(|pg| pg.as_ref().map(|pg| pg.show.feed_url.clone())) else { return };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, Show>("/api/shows", &serde_json::json!({ "url": url })).await {
                Ok(s) => {
                    toast.show(format!("Following {}", s.title));
                    go(&format!("#/podcasts/show/{}", s.id));
                }
                Err(e) => toast.show(e.0),
            }
            busy.set(false);
        });
    };
    let unfollow = move |_| {
        let Some(id) = id else { return };
        spawn_local(async move {
            match api::remove(&format!("/api/shows/{id}")).await {
                Ok(()) => {
                    toast.show("Unfollowed");
                    go("#/podcasts");
                }
                Err(e) => toast.show(e.0),
            }
        });
    };
    let set_skip = move |on: bool| {
        let Some(id) = id else { return };
        spawn_local(async move {
            match api::send::<_, Show>("PATCH", &format!("/api/shows/{id}"), &serde_json::json!({ "skip_ads": on })).await {
                Ok(s) => {
                    toast.show(if s.skip_ads { "Ads will be skipped in this show" } else { "Ads will play in this show" });
                    page.update(|pg| {
                        if let Some(pg) = pg {
                            pg.show.skip_ads = s.skip_ads;
                            for e in &mut pg.episodes {
                                e.show_skip_ads = s.skip_ads;
                            }
                        }
                    });
                }
                Err(e) => toast.show(e.0),
            }
        });
    };
    let all_played = move |_| {
        let Some(id) = id else { return };
        spawn_local(async move {
            match api::post::<_, ()>(&format!("/api/shows/{id}/played"), &()).await {
                Ok(()) => {
                    toast.show("Marked all played");
                    load();
                }
                Err(e) => toast.show(e.0),
            }
        });
    };

    view! {
        <div class="pods pod-show">
            <a class="back-link" href="#/podcasts">
                <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14.5 6l-6 6 6 6"/></svg>
                "Podcasts"
            </a>
            {move || failed.get().map(|e| view! { <p class="brief-error">{e}</p> })}
            {move || match page.get() {
                None if failed.with(Option::is_none) => view! { <div class="pods-loading"><span class="spinner"></span></div> }.into_any(),
                None => ().into_any(),
                Some(pg) => {
                    let s = pg.show.clone();
                    let followed = s.id != 0;
                    let bg = s.image.clone().map(|u| format!("url(\"{}\")", u.replace('"', "%22"))).unwrap_or_default();
                    let eps = pg.episodes.clone();
                    let count = eps.len();
                    view! {
                        <section class="show-hero" style:--hero=bg style:--hue=hue_of(&s.title).to_string()>
                            {art(s.image.clone(), &s.title, "hero-art")}
                            <div class="hero-text">
                                <h1>{s.title.clone()}</h1>
                                <p class="hero-author">{s.author.clone()}{(s.kind == "youtube").then(|| view! { <span class="yt-badge">"YouTube"</span> })}</p>
                                <p class="hero-desc" class:open=move || more.get() on:click=move |_| more.update(|m| *m = !*m)>{s.description.clone()}</p>
                                <div class="hero-actions">
                                    {if followed {
                                        view! {
                                            <button class="ghost following" on:click=unfollow title="Unfollow">{icon(icons::CHECK)}"Following"</button>
                                            <label class="chip-toggle" title="Find and skip the ads in this show's episodes">
                                                <input type="checkbox" prop:checked=s.skip_ads on:change=move |ev| set_skip(event_target_checked(&ev))/>
                                                <span>"Skip ads"</span>
                                            </label>
                                            <button class="ghost" on:click=all_played>"Mark all played"</button>
                                        }.into_any()
                                    } else {
                                        view! {
                                            <button class="primary" on:click=follow disabled=move || busy.get()>
                                                {move || if busy.get() { view! { <span class="spinner small"></span> }.into_any() } else { ().into_any() }}
                                                "Follow"
                                            </button>
                                            <small class="muted">"Follow to have ads found and skipped"</small>
                                        }.into_any()
                                    }}
                                    {(!s.link.is_empty()).then(|| view! { <a class="button ghost" href=s.link.clone() target="_blank" rel="noopener noreferrer">"Website"</a> })}
                                </div>
                            </div>
                        </section>
                        <div class="show-eps-head">
                            <h2>{format!("{count} episode{}", if count == 1 { "" } else { "s" })}</h2>
                            {followed.then(|| view! {
                                <div class="seg">
                                    <button class:on=move || filter.get() == "all" on:click=move |_| filter.set("all")>"All"</button>
                                    <button class:on=move || filter.get() == "unplayed" on:click=move |_| filter.set("unplayed")>"Unplayed"</button>
                                </div>
                            })}
                        </div>
                        <ul class="pod-episodes">
                            {move || eps.iter().filter(|e| filter.get() == "all" || !e.finished).cloned().map(|e| view! { <EpisodeRow ep=e/> }).collect_view()}
                        </ul>
                    }.into_any()
                }
            }}
        </div>
    }
}

#[component]
fn AddShow(on_done: Callback<()>) -> impl IntoView {
    let toast = toast();
    let url = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let add = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let u = url.get_untracked();
        if u.trim().is_empty() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, Show>("/api/shows", &serde_json::json!({ "url": u })).await {
                Ok(s) => {
                    toast.show(format!("Following {}", s.title));
                    on_done.run(());
                    go(&format!("#/podcasts/show/{}", s.id));
                }
                Err(e) => toast.show(e.0),
            }
            busy.set(false);
        });
    };
    let import = move |ev: web_sys::Event| {
        let input: web_sys::HtmlInputElement = event_target(&ev);
        let Some(file) = input.files().and_then(|f| f.get(0)) else { return };
        busy.set(true);
        spawn_local(async move {
            #[derive(serde::Deserialize)]
            struct Done {
                added: usize,
                failed: usize,
            }
            match api::upload::<Done>("/api/shows/opml", file).await {
                Ok(d) => {
                    toast.show(if d.failed > 0 { format!("Imported {} shows ({} couldn't be reached)", d.added, d.failed) } else { format!("Imported {} shows", d.added) });
                    on_done.run(());
                }
                Err(e) => toast.show(e.0),
            }
            busy.set(false);
        });
    };
    view! {
        <div class="pods-add">
            <form on:submit=add>
                <label>"Feed address, Apple Podcasts link, or YouTube channel or playlist"
                    <input type="url" placeholder="https://…" prop:value=move || url.get() on:input=move |ev| url.set(event_target_value(&ev))/>
                </label>
                <button class="primary" disabled=move || busy.get()>"Follow"</button>
            </form>
            <p class="muted small">"Private feeds work too (Patreon, Supercast, Substack): copy your personal RSS link from the site. "
               "For YouTube, paste a channel link, an @handle, or a playlist link."</p>
            <h4>"Bring your shows from another app"</h4>
            <p class="muted small">"Export your subscriptions as OPML from your podcast app (most have it in their settings), then pick the file here."</p>
            <label class="button ghost file-pick">
                <input type="file" accept=".opml,.xml,text/xml,text/x-opml" on:change=import/>
                {move || if busy.get() { view! { <span class="spinner small"></span> }.into_any() } else { icon(icons::DOWNLOAD).into_any() }}
                "Import OPML"
            </label>
            <h4>"Take them with you"</h4>
            <button class="ghost" on:click=move |_| sp_download("/api/shows/opml")>"Export OPML"</button>
        </div>
    }
}

/// Playing and ad-finding preferences. Saved as you change them.
#[component]
fn PodSettings() -> impl IntoView {
    let toast = toast();
    let p = player();
    let save = move |f: Box<dyn FnOnce(&mut ShowSettings)>| {
        let mut s = p.prefs.get_untracked();
        f(&mut s);
        p.prefs.set(s.clone());
        spawn_local(async move {
            match api::put::<_, ShowSettings>("/api/shows/settings", &s).await {
                Ok(_) => toast.show("Saved"),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let check = move |label: &'static str, hint: &'static str, get: fn(&ShowSettings) -> bool, set: fn(&mut ShowSettings, bool)| {
        view! {
            <label class="check pods-check" title=hint>
                <input type="checkbox" prop:checked=move || p.prefs.with(get) on:change=move |ev| { let on = event_target_checked(&ev); save(Box::new(move |s| set(s, on))) }/>
                <span>{label}<small>{hint}</small></span>
            </label>
        }
    };
    view! {
        <div class="pods-settings">
            <h4>"Ads " {crate::ai_tag()}</h4>
            {check("Skip ads", "Episodes are downloaded, listened to, and the ad breaks found by AI are skipped as you play.", |s| s.skip_ads, |s, v| s.skip_ads = v)}
            {check("Skip promos too", "Plugs for other shows, and the show's own membership or donation pitches.", |s| s.skip_promos, |s, v| s.skip_promos = v)}
            {check("Get new episodes ready", "New episodes are downloaded and checked as they come out, so they play ad-free right away.", |s| s.prepare_new, |s, v| s.prepare_new = v)}
            <label>"What reads the transcripts"
                <select on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| s.ad_model = v)) }>
                    <option value="chat" selected=move || p.prefs.with(|s| s.ad_model != "local")>"The chat model (Settings → AI)"</option>
                    <option value="local" selected=move || p.prefs.with(|s| s.ad_model == "local")>"The local model (stays on this computer)"</option>
                </select>
            </label>
            {check("Use SponsorBlock for YouTube", "YouTube shows skip the sponsor segments marked by SponsorBlock's community (sponsor.ajay.app) straight away; the AI listens only when there are none.", |s| s.sponsorblock, |s, v| s.sponsorblock = v)}
            <p class="muted small">"Listening is done by the voice server's speech recognition on your graphics card."</p>
            <h4>"Finding shows"</h4>
            <label>"Charts from"
                <select on:change=move |ev| { let v = event_target_value(&ev); save(Box::new(move |s| s.country = v)) }>
                    {COUNTRIES.map(|(c, n)| view! { <option value=c selected=move || p.prefs.with(|s| s.country == c)>{n}</option> })}
                </select>
            </label>
            <p class="muted small">"Podcast Index is the open podcast directory (free): with a key, search includes the shows it has that Apple doesn't, and Discover shows what's trending. "
                <a href="https://api.podcastindex.org/signup" target="_blank" rel="noopener noreferrer">"Get a key"</a></p>
            <div class="brief-row">
                <label>"Podcast Index key"
                    <input type="text" autocomplete="off" spellcheck="false" prop:value=move || p.prefs.with(|s| s.podcastindex_key.clone())
                        on:change=move |ev| { let v = event_target_value(&ev).trim().to_string(); save(Box::new(move |s| s.podcastindex_key = v)) }/>
                </label>
                <label>"Secret"
                    <input type="password" autocomplete="off" prop:value=move || p.prefs.with(|s| s.podcastindex_secret.clone())
                        on:change=move |ev| { let v = event_target_value(&ev).trim().to_string(); save(Box::new(move |s| s.podcastindex_secret = v)) }/>
                </label>
            </div>
            <h4>"Playing"</h4>
            <div class="brief-row">
                <label>"Skip back"
                    <select on:change=move |ev| { let v = event_target_value(&ev).parse().unwrap_or(15); save(Box::new(move |s| s.back_seconds = v)) }>
                        {[5u32, 10, 15, 30].map(|n| view! { <option value=n.to_string() selected=move || p.prefs.with(|s| s.back_seconds == n)>{format!("{n} seconds")}</option> })}
                    </select>
                </label>
                <label>"Skip forward"
                    <select on:change=move |ev| { let v = event_target_value(&ev).parse().unwrap_or(30); save(Box::new(move |s| s.forward_seconds = v)) }>
                        {[10u32, 15, 30, 45, 60].map(|n| view! { <option value=n.to_string() selected=move || p.prefs.with(|s| s.forward_seconds == n)>{format!("{n} seconds")}</option> })}
                    </select>
                </label>
            </div>
            <label>"Delete played downloads after"
                <select on:change=move |ev| { let v = event_target_value(&ev).parse().unwrap_or(7); save(Box::new(move |s| s.keep_days = v)) }>
                    {[(1u32, "1 day"), (3, "3 days"), (7, "a week"), (30, "a month"), (0, "never")].map(|(n, l)| view! { <option value=n.to_string() selected=move || p.prefs.with(|s| s.keep_days == n)>{l}</option> })}
                </select>
            </label>
        </div>
    }
}

// ---- The player's views ----

/// The bar along the bottom while something's playing; opens the full player.
#[component]
pub fn MiniPlayer(#[prop(into)] hidden: Signal<bool>) -> impl IntoView {
    let p = player();
    let ep = move || p.ep.get().unwrap_or_default();
    let back = move || p.prefs.with(|s| s.back_seconds);
    let fwd = move || p.prefs.with(|s| s.forward_seconds);
    let frac = move || { let d = p.duration.get(); if d > 0.0 { (p.time.get() / d * 100.0).clamp(0.0, 100.0) } else { 0.0 } };
    view! {
        <Show when=move || p.active() && !hidden.get() && !p.open.get()>
            <div class="mini-player" role="region" aria-label="Now playing">
                <div class="mini-bar">
                    <span style:width=move || format!("{:.2}%", frac())></span>
                    {move || {
                        let d = p.duration.get();
                        p.ep.with(|e| e.as_ref().map(|e| e.ads.ads.clone()).unwrap_or_default()).into_iter().filter(|_| d > 0.0).map(|a| view! {
                            <i class="mini-ad" style:left=format!("{:.2}%", a.start / d * 100.0) style:width=format!("{:.2}%", (a.end - a.start) / d * 100.0)></i>
                        }).collect_view()
                    }}
                </div>
                <button class="mini-open" on:click=move |_| p.open.set(true) aria-label="Open the player">
                    {move || { let e = ep(); art(e.image.clone(), &e.show_title, "mini-art") }}
                    <span class="mini-text">
                        <strong>{move || ep().title}</strong>
                        <small>{move || {
                            if let Some(s) = p.preparing.get() { if s.doing.is_empty() { "Getting it ready…".to_string() } else { s.doing } }
                            else if let Some((_, secs)) = p.skipped.get() { format!("Skipped a {} ad", format_time(secs)) }
                            else { ep().show_title }
                        }}</small>
                    </span>
                </button>
                <button class="icon-button mini-skip" aria-label=move || format!("Back {} seconds", back()) on:click=move |_| sp_nudge(-(back() as f64))>{move || back_icon(back())}</button>
                <button class="mini-play" aria-label=move || if p.playing() { "Pause" } else { "Play" } on:click=move |_| p.toggle()>
                    {move || if p.preparing.get().is_some() || p.state.get() == "waiting" { view! { <span class="spinner small"></span> }.into_any() } else { play_glyph(p.playing()).into_any() }}
                </button>
                <button class="icon-button mini-skip mini-fwd" aria-label=move || format!("Forward {} seconds", fwd()) on:click=move |_| sp_nudge(fwd() as f64)>{move || forward_icon(fwd())}</button>
            </div>
        </Show>
    }
}

/// The scrubber: what's played, the ads found (marked), and how far it's been checked.
#[component]
fn Scrubber() -> impl IntoView {
    let p = player();
    let dragging = RwSignal::new(None::<f64>);
    let bar = NodeRef::<leptos::html::Div>::new();
    let at = move |ev: &web_sys::PointerEvent| -> f64 {
        let Some(el) = bar.get_untracked() else { return 0.0 };
        let r = el.get_bounding_client_rect();
        ((ev.client_x() as f64 - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0) * p.duration.get_untracked()
    };
    let shown = move || dragging.get().unwrap_or_else(|| p.time.get());
    let pct = move |t: f64| { let d = p.duration.get(); if d > 0.0 { (t / d * 100.0).clamp(0.0, 100.0) } else { 0.0 } };
    view! {
        <div class="pscrub">
            <div class="pscrub-track" node_ref=bar
                on:pointerdown=move |ev| {
                    let _ = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()).map(|t| t.set_pointer_capture(ev.pointer_id()));
                    dragging.set(Some(at(&ev)));
                }
                on:pointermove=move |ev| if dragging.get_untracked().is_some() { dragging.set(Some(at(&ev))) }
                on:pointerup=move |ev| { let t = at(&ev); dragging.set(None); sp_seek(t); p.time.set(t); }
                on:pointercancel=move |_| dragging.set(None)>
                <div class="pscrub-rail">
                    <Show when=move || p.ep.with(|e| e.as_ref().is_some_and(|e| e.ads.busy()))>
                        <span class="pscrub-checked" style:width=move || format!("{:.2}%", pct(p.ep.with(|e| e.as_ref().map(|e| e.ads.checked).unwrap_or(0.0))))></span>
                    </Show>
                    <span class="pscrub-played" style:width=move || format!("{:.2}%", pct(shown()))></span>
                </div>
                <div class="pscrub-marks">
                    {move || p.ep.with(|e| e.as_ref().map(|e| e.ads.ads.clone()).unwrap_or_default()).into_iter().enumerate().map(|(i, a)| {
                        let label = if a.label.is_empty() { "Ad".to_string() } else { a.label.clone() };
                        let when = format!("{} – {}", format_time(a.start), format_time(a.end));
                        let len = format_time(a.end - a.start);
                        view! {
                            <span class="pscrub-ad" class:done=move || p.skipped_ads.with(|v| v.contains(&i))
                                aria-label=format!("Ad: {label}, {when}")
                                style:left=move || format!("{:.2}%", pct(a.start)) style:width=move || format!("{:.2}%", (pct(a.end) - pct(a.start)).max(0.6))>
                                <span class="pscrub-tip" role="tooltip">
                                    <strong>{label.clone()}</strong>
                                    <span>{format!("{when} · {len}")}</span>
                                    <em>{move || if p.skipped_ads.with(|v| v.contains(&i)) { "Skipped" } else if !p.skipping() { "Will play" } else if p.time.get() > a.end { "Played" } else { "Will be skipped" }}</em>
                                </span>
                            </span>
                        }
                    }).collect_view()}
                </div>
                <span class="pscrub-thumb" class:grab=move || dragging.get().is_some() style:left=move || format!("{:.2}%", pct(shown()))></span>
            </div>
            <div class="pscrub-times">
                <span>{move || format_time(shown())}</span>
                <span>{move || format!("-{}", format_time((p.duration.get() - shown()).max(0.0)))}</span>
            </div>
        </div>
    }
}

/// The full player: big artwork, controls, and the episode's notes, ads and transcript.
#[component]
pub fn NowPlaying() -> impl IntoView {
    let p = player();
    let toast = toast();
    let ep = move || p.ep.get().unwrap_or_default();
    let tab = RwSignal::new("notes");
    let back = move || p.prefs.with(|s| s.back_seconds);
    let fwd = move || p.prefs.with(|s| s.forward_seconds);
    let show_speed = RwSignal::new(false);
    let show_sleep = RwSignal::new(false);
    let sleep_left = RwSignal::new(0.0f64);
    // The sleep timer's countdown.
    spawn_local(async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(1000).await;
            if p.sleep.get_untracked() > 0 {
                let left = sp_sleep_left();
                sleep_left.set(left);
                if left <= 0.0 {
                    p.sleep.set(0);
                }
            }
        }
    });
    let set_rate = move |r: f64| {
        p.rate.set(r);
        sp_rate(r);
        show_speed.set(false);
        let mut s = p.prefs.get_untracked();
        s.speed = r;
        p.prefs.set(s.clone());
        spawn_local(async move {
            let _ = api::put::<_, ShowSettings>("/api/shows/settings", &s).await;
        });
    };
    let set_sleep = move |m: i32| {
        p.sleep.set(m);
        sp_sleep(if m > 0 { m as f64 } else { 0.0 });
        sleep_left.set(if m > 0 { m as f64 * 60.0 } else { 0.0 });
        show_sleep.set(false);
    };
    // "End of episode": the player stops when it ends anyway; nothing to do but remember.
    let not_ad = move |i: usize| {
        let Some(id) = p.ep.with_untracked(|e| e.as_ref().map(|e| e.id)) else { return };
        spawn_local(async move {
            match api::post::<_, AdScan>(&format!("/api/shows/episodes/{id}/ads/{i}/remove"), &()).await {
                Ok(s) => {
                    p.ep.update(|e| {
                        if let Some(e) = e {
                            e.ads = s;
                        }
                    });
                    p.send_ads();
                    p.skipped_ads.update(|v| {
                        v.retain(|x| *x != i);
                        for x in v.iter_mut() {
                            if *x > i {
                                *x -= 1;
                            }
                        }
                    });
                    toast.show("That part will play from now on");
                }
                Err(e) => toast.show(e.0),
            }
        });
    };
    let rescan = move |_| {
        let Some(id) = p.ep.with_untracked(|e| e.as_ref().map(|e| e.id)) else { return };
        spawn_local(async move {
            match api::post::<_, AdScan>(&format!("/api/shows/episodes/{id}/rescan"), &()).await {
                Ok(s) => p.ep.update(|e| {
                    if let Some(e) = e {
                        e.ads = s;
                    }
                }),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let transcript = RwSignal::new(Vec::<(f64, String)>::new());
    // Each line's words' times (when Whisper gave them), to light up the word being said.
    let words = RwSignal::new(Vec::<Vec<(f64, f64, String)>>::new());
    Effect::new(move |_| {
        if tab.get() != "transcript" {
            return;
        }
        let Some(id) = p.ep.with(|e| e.as_ref().map(|e| e.id)) else { return };
        let _ = p.ep.with(|e| e.as_ref().map(|e| e.ads.checked));
        spawn_local(async move {
            #[derive(serde::Deserialize)]
            struct Heard {
                start: f64,
                end: f64,
                text: String,
                #[serde(default)]
                words: Vec<(f64, f64, String)>,
            }
            if let Ok(h) = api::get::<Vec<Heard>>(&format!("/api/shows/episodes/{id}/transcript")).await {
                words.set(h.iter().map(|h| if h.words.is_empty() { spread_words(h.start, h.end, &h.text) } else { h.words.clone() }).collect());
                transcript.set(h.into_iter().map(|h| (h.start, h.text)).collect());
            }
        });
    });
    // The line being said now.
    let said = Memo::new(move |_| {
        let x = p.time.get();
        transcript.with(|all| all.iter().rposition(|(t, _)| *t <= x))
    });
    let ep_id = Memo::new(move |_| p.ep.with(|e| e.as_ref().map(|e| e.id).unwrap_or(0)));
    let outline = outline_for(Signal::derive(move || p.ep.with(|e| e.as_ref().map(|e| (e.id, e.ads.state.clone())).unwrap_or_default())));
    // Swipe the sheet down to close it (phones).
    let drag = RwSignal::new(None::<(f64, f64)>);
    let skipping = move || p.skipping();
    let bg = move || ep().image.map(|u| format!("url(\"{}\")", u.replace('"', "%22"))).unwrap_or_default();

    view! {
        <Show when=move || p.active() && p.open.get()>
            <leptos::portal::Portal>
            <div class="np-scrim" on:click=move |_| p.open.set(false)></div>
            <section class="now-playing" role="dialog" aria-label="Now playing"
                style:--np-bg=bg style:--hue=move || hue_of(&ep().show_title).to_string()
                style:transform=move || drag.get().map(|(_, dy)| format!("translateY({}px)", dy.max(0.0))).unwrap_or_default()
                on:keydown=move |ev: web_sys::KeyboardEvent| if ev.key() == "Escape" { p.open.set(false) }>
                <div class="np-backdrop" aria-hidden="true"></div>
                <header class="np-head"
                    on:pointerdown=move |ev| drag.set(Some((ev.client_y() as f64, 0.0)))
                    on:pointermove=move |ev| if let Some((y0, _)) = drag.get_untracked() { drag.set(Some((y0, ev.client_y() as f64 - y0))) }
                    on:pointerup=move |_| { if drag.get_untracked().is_some_and(|(_, dy)| dy > 90.0) { p.open.set(false) } drag.set(None) }
                    on:pointercancel=move |_| drag.set(None)>
                    <span class="np-grip" aria-hidden="true"></span>
                    <button class="icon-button" aria-label="Close the player" on:click=move |_| p.open.set(false)>
                        <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M6 9.5l6 6 6-6"/></svg>
                    </button>
                    <a class="np-show-link" href=move || format!("#/podcasts/show/{}", ep().show_id) on:click=move |_| p.open.set(false)>{move || ep().show_title}</a>
                    <button class="icon-button" title="Stop and close" aria-label="Stop" on:click=move |_| p.close()>"×"</button>
                </header>
                <div class="np-body">
                    <div class="np-main">
                        <div class="np-art-wrap" class:playing=move || p.playing()>
                            {move || { let e = ep(); art(e.image.clone(), &e.show_title, "np-art") }}
                        </div>
                        <div class="np-titles">
                            <h2>{move || ep().title}</h2>
                            <small>{move || { let e = ep(); e.published.map(|d| sp_date(&d)).unwrap_or_default() }}</small>
                        </div>
                        {move || p.preparing.get().map(|s| view! {
                            <div class="np-preparing">
                                <span class="spinner small"></span>
                                <span>{if s.doing.is_empty() { "Getting it ready to skip the ads…".to_string() } else { s.doing.clone() }}</span>
                                <div class="mini-progress"><span style:width=format!("{:.0}%", s.progress * 100.0)></span></div>
                                <Show when=move || !p.ep.with(|e| e.as_ref().is_some_and(|e| e.needs_download))>
                                    <button class="link" on:click=move |_| p.play_without_skipping()>"Play now, with ads"</button>
                                </Show>
                            </div>
                        })}
                        <Scrubber/>
                        <div class="np-controls">
                            <button class="icon-button np-speed" title="Speed" on:click=move |_| show_speed.update(|v| *v = !*v)>{move || format!("{}×", p.rate.get())}</button>
                            <button class="icon-button np-skip" aria-label=move || format!("Back {} seconds", back()) on:click=move |_| sp_nudge(-(back() as f64))>{move || back_icon(back())}</button>
                            <button class="np-play primary" aria-label=move || if p.playing() { "Pause" } else { "Play" } on:click=move |_| p.toggle()>
                                {move || if p.preparing.get().is_some() { view! { <span class="spinner"></span> }.into_any() } else { play_glyph(p.playing()).into_any() }}
                            </button>
                            <button class="icon-button np-skip" aria-label=move || format!("Forward {} seconds", fwd()) on:click=move |_| sp_nudge(fwd() as f64)>{move || forward_icon(fwd())}</button>
                            <button class="icon-button np-sleep" class:on=move || p.sleep.get() != 0 title="Sleep timer" on:click=move |_| show_sleep.update(|v| *v = !*v)>
                                <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M19 14.5A7.5 7.5 0 1 1 9.5 5a6 6 0 0 0 9.5 9.5z"/></svg>
                                {move || match p.sleep.get() { 0 => ().into_any(), -1 => view! { <small>"end"</small> }.into_any(), _ => view! { <small>{format!("{}m", (sleep_left.get() / 60.0).ceil())}</small> }.into_any() }}
                            </button>
                        </div>
                        <Show when=move || show_speed.get()>
                            <div class="np-choices">
                                {[0.8, 1.0, 1.1, 1.2, 1.3, 1.5, 1.75, 2.0].map(|r| view! { <button class:on=move || p.rate.get() == r on:click=move |_| set_rate(r)>{format!("{r}×")}</button> })}
                            </div>
                        </Show>
                        <Show when=move || show_sleep.get()>
                            <div class="np-choices">
                                <button class:on=move || p.sleep.get() == 0 on:click=move |_| set_sleep(0)>"Off"</button>
                                {[5, 15, 30, 45, 60].map(|m| view! { <button class:on=move || p.sleep.get() == m on:click=move |_| set_sleep(m)>{format!("{m} min")}</button> })}
                                <button class:on=move || p.sleep.get() == -1 on:click=move |_| set_sleep(-1)>"End of episode"</button>
                            </div>
                        </Show>
                        <div class="np-ads-line">
                            {move || {
                                let e = ep();
                                if e.id == 0 {
                                    return view! { <span class="muted">"Follow the show to have its ads skipped"</span> }.into_any();
                                }
                                if !skipping() {
                                    return view! { <span class="muted">"Ad skipping is off for this show"</span> }.into_any();
                                }
                                let saved = p.saved.get();
                                let n = e.ads.ads.len();
                                let k = p.skipped_ads.with(Vec::len);
                                let text = if n == 0 {
                                    ad_summary(&e.ads).unwrap_or_else(|| "Ads not checked yet".into())
                                } else {
                                    let mut t = format!("{n} ad{} found", if n == 1 { "" } else { "s" });
                                    if e.ads.busy() {
                                        t.push_str(" so far");
                                    }
                                    t.push_str(&format!(" · {k} skipped"));
                                    if saved > 1.0 {
                                        t.push_str(&format!(" · {} saved", format_time(saved)));
                                    }
                                    t
                                };
                                view! {
                                    <button class="np-ads-status link" class:busy=e.ads.busy() title="See where the ads are" on:click=move |_| tab.set("ads")>
                                        {if e.ads.busy() { view! { <span class="spinner tiny"></span> }.into_any() } else { view! { <span class="ad-dot"></span> }.into_any() }}
                                        {text}
                                        {e.ads.busy().then(|| view! { <small class="muted">{format!(" (checking {})", e.ads.doing.trim_start_matches("Listening for ads: "))}</small> })}
                                    </button>
                                }.into_any()
                            }}
                        </div>
                        {move || p.skipped.get().map(|(i, secs)| view! {
                            <div class="np-skipped" role="status">
                                <span>{format!("Skipped {} of ads", format_time(secs))}</span>
                                <button class="link" on:click=move |_| { sp_allow(i); p.skipped.set(None) }>"Undo"</button>
                            </div>
                        })}
                    </div>
                    <div class="np-side">
                        <div class="seg np-tabs">
                            <button class:on=move || tab.get() == "notes" on:click=move |_| tab.set("notes")>"Notes"</button>
                            <button class:on=move || tab.get() == "ads" on:click=move |_| tab.set("ads")>{move || { let n = ep().ads.ads.len(); if n > 0 { format!("Ads ({n})") } else { "Ads".into() } }}</button>
                            <button class:on=move || tab.get() == "transcript" on:click=move |_| tab.set("transcript")>"Transcript"</button>
                        </div>
                        {move || match tab.get() {
                            "ads" => {
                                let e = ep();
                                view! {
                                    <div class="np-ads">
                                        {if e.ads.ads.is_empty() {
                                            view! { <p class="muted">{ad_summary(&e.ads).unwrap_or_else(|| "Not checked for ads yet.".into())}</p> }.into_any()
                                        } else {
                                            view! {
                                                <ul>
                                                    {e.ads.ads.iter().cloned().enumerate().map(|(i, a)| view! {
                                                        <li>
                                                            <button class="link ts" on:click=move |_| { sp_allow(i); sp_play() } title="Play this ad">{format!("{} – {}", format_time(a.start), format_time(a.end))}</button>
                                                            <span class="ad-label">{if a.label.is_empty() { "Ad".to_string() } else { a.label.clone() }}<small class="muted">{format!(" · {}", format_time(a.end - a.start))}</small></span>
                                                            <span class="ad-state" class:done=move || p.skipped_ads.with(|v| v.contains(&i))>
                                                                {move || if p.skipped_ads.with(|v| v.contains(&i)) { "Skipped" } else if p.time.get() > a.end { "Played" } else { "Coming up" }}
                                                            </span>
                                                            <button class="link" on:click=move |_| not_ad(i) title="This isn't an ad: always play it">"Not an ad"</button>
                                                        </li>
                                                    }).collect_view()}
                                                </ul>
                                            }.into_any()
                                        }}
                                        {(e.id != 0 && !e.ads.busy()).then(|| view! { <button class="ghost small" on:click=rescan>"Check again"</button> })}
                                        <p class="muted small">"Ads are found by AI from a transcript (or by SponsorBlock for YouTube), so now and then it'll miss one or catch a bit of the show. Tap a time to hear it."</p>
                                    </div>
                                }.into_any()
                            }
                            "transcript" => view! { <Transcript lines=transcript words said outline/> }.into_any(),
                            _ => {
                                let id = ep_id.get();
                                view! { <Notes id ep=Signal::derive(move || p.ep.get().unwrap_or_default())/> }.into_any()
                            }
                        }}
                    </div>
                </div>
            </section>
            </leptos::portal::Portal>
        </Show>
    }
}

const COUNTRIES: [(&str, &str); 12] = [
    ("us", "United States"),
    ("gb", "United Kingdom"),
    ("ca", "Canada"),
    ("au", "Australia"),
    ("ie", "Ireland"),
    ("nz", "New Zealand"),
    ("in", "India"),
    ("de", "Germany"),
    ("fr", "France"),
    ("es", "Spain"),
    ("jp", "Japan"),
    ("br", "Brazil"),
];

/// The charts: Apple's top shows, overall or by category, ranked; and what's trending on
/// Podcast Index when it's set up.
#[component]
fn Discover(genre: Option<i64>) -> impl IntoView {
    let chart = RwSignal::new(None::<Chart>);
    let failed = RwSignal::new(None::<String>);
    spawn_local(async move {
        let url = match genre {
            Some(g) => format!("/api/shows/charts?genre={g}"),
            None => "/api/shows/charts".into(),
        };
        match api::get::<Chart>(&url).await {
            Ok(c) => chart.set(Some(c)),
            Err(e) => failed.set(Some(e.0)),
        }
    });
    let name = genre.and_then(|g| GENRES.iter().find(|(id, _)| *id == g).map(|(_, n)| *n)).unwrap_or("All shows");
    let tab = RwSignal::new("top");
    view! {
        <div class="pods pods-discover">
            <a class="back-link" href="#/podcasts">
                <svg viewBox="0 0 24 24" class="icon" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14.5 6l-6 6 6 6"/></svg>
                "Podcasts"
            </a>
            <header class="pods-head"><h1>"Discover"</h1></header>
            <nav class="genre-chips" aria-label="Categories">
                <a href="#/podcasts/discover" class:on=genre.is_none()>"All"</a>
                {GENRES.iter().map(|(id, n)| view! { <a href=format!("#/podcasts/discover/{id}") class:on=genre == Some(*id)>{*n}</a> }).collect_view()}
            </nav>
            <div class="section-head discover-head">
                <h2>{name}</h2>
                <Show when=move || chart.with(|c| c.as_ref().is_some_and(|c| !c.trending.is_empty()))>
                    <div class="seg">
                        <button class:on=move || tab.get() == "top" on:click=move |_| tab.set("top")>"Top charts"</button>
                        <button class:on=move || tab.get() == "trending" on:click=move |_| tab.set("trending")>"Trending"</button>
                    </div>
                </Show>
            </div>
            {move || failed.get().map(|e| view! { <p class="brief-error">{format!("Couldn't load the charts: {e}")}</p> })}
            {move || match chart.get() {
                None if failed.with(Option::is_none) => view! { <div class="pods-loading"><span class="spinner"></span></div> }.into_any(),
                None => ().into_any(),
                Some(c) => {
                    let hits = if tab.get() == "trending" && !c.trending.is_empty() { c.trending } else { c.shows };
                    if hits.is_empty() {
                        view! { <p class="muted">"Nothing in this chart right now."</p> }.into_any()
                    } else {
                        view! { <HitGrid hits ranked=true on_follow=Callback::new(|_| ())/> }.into_any()
                    }
                }
            }}
            <p class="muted small discover-note">"Top charts are Apple Podcasts' rankings (country in Podcasts settings). Trending comes from Podcast Index when it's set up there."</p>
        </div>
    }
}

/// The episode's transcript: as an AI table of contents whose sections open into what's said
/// in them, or whole. Follows along as it plays (or not, while you're reading back), marks the
/// ads, searches, and jumps to any line you tap.
#[component]
fn Transcript(lines: RwSignal<Vec<(f64, String)>>, words: RwSignal<Vec<Vec<(f64, f64, String)>>>, said: Memo<Option<usize>>, outline: RwSignal<Outline>) -> impl IntoView {
    let p = player();
    let toast = toast();
    let query = RwSignal::new(String::new());
    let follow = RwSignal::new(true);
    let whole = RwSignal::new(false);
    let opened = RwSignal::new(std::collections::HashSet::<usize>::new());
    let list = NodeRef::<leptos::html::Div>::new();
    let ads = move || p.ep.with(|e| e.as_ref().map(|e| e.ads.ads.clone()).unwrap_or_default());
    let has_outline = move || outline.with(|o| o.state == "done" && !o.sections.is_empty());
    let contents = move || has_outline() && !whole.get() && query.with(|q| q.trim().is_empty());
    // The section playing now.
    let now_section = Memo::new(move |_| {
        let t = p.time.get();
        outline.with(|o| o.sections.iter().position(|s| t >= s.start && t < s.end))
    });
    // Following along opens the section being played, and closes the one it opened before.
    let auto_opened = StoredValue::new(None::<usize>);
    Effect::new(move |_| {
        if let Some(k) = now_section.get()
            && follow.get()
            && contents()
        {
            let before = auto_opened.get_value();
            if before == Some(k) && opened.with_untracked(|o| o.contains(&k)) {
                return;
            }
            opened.update(|o| {
                if let Some(j) = before.filter(|j| *j != k) {
                    o.remove(&j);
                }
                o.insert(k);
            });
            auto_opened.set_value(Some(k));
        }
    });
    Effect::new(move |_| {
        let Some(i) = said.get() else { return };
        if !follow.get() || !query.with(String::is_empty) {
            return;
        }
        let _ = opened.get();
        let Some(el) = list.get() else { return };
        if let Ok(Some(line)) = el.query_selector(&format!("[data-i=\"{i}\"]")) {
            let opts = web_sys::ScrollIntoViewOptions::new();
            opts.set_block(web_sys::ScrollLogicalPosition::Center);
            opts.set_behavior(web_sys::ScrollBehavior::Smooth);
            line.scroll_into_view_with_scroll_into_view_options(&opts);
        }
    });
    let prepare = move |_| {
        let Some(id) = p.ep.with_untracked(|e| e.as_ref().map(|e| e.id)) else { return };
        spawn_local(async move {
            match api::post::<_, AdScan>(&format!("/api/shows/episodes/{id}/prepare"), &serde_json::json!({ "now": true })).await {
                Ok(s) => p.ep.update(|e| {
                    if let Some(e) = e {
                        e.ads = s;
                    }
                }),
                Err(e) => toast.show(e.0),
            }
        });
    };
    let remake = move |_| {
        let id = p.ep.with_untracked(|e| e.as_ref().map(|e| e.id).unwrap_or(0));
        if id != 0 {
            remake_outline(id, outline);
        }
    };
    let jump = move |t: f64| {
        sp_seek(t);
        sp_play();
        follow.set(true);
    };
    view! {
        <div class="np-transcript">
            <div class="tr-tools">
                <label class="tr-search">
                    {icon(icons::DISCOVER)}
                    <input type="search" placeholder="Search the transcript" prop:value=move || query.get() on:input=move |ev| query.set(event_target_value(&ev))/>
                </label>
                <Show when=has_outline>
                    <div class="seg tr-mode">
                        <button class:on=move || !whole.get() on:click=move |_| whole.set(false) title="AI table of contents">"Contents"</button>
                        <button class:on=move || whole.get() on:click=move |_| whole.set(true)>"Full text"</button>
                    </div>
                </Show>
                <button class="ghost small tr-follow" class:selected=move || follow.get() on:click=move |_| follow.update(|f| *f = !*f)
                    title="Keep the line being said in view">
                    {move || if follow.get() { "Following along" } else { "Follow along" }}
                </button>
            </div>
            <div class="tr-lines" node_ref=list
                on:wheel=move |_| follow.set(false) on:touchmove=move |_| follow.set(false)>
                {move || {
                    if lines.with(Vec::is_empty) {
                        let e = p.ep.get().unwrap_or_default();
                        return if e.ads.busy() {
                            view! { <p class="muted"><span class="spinner tiny"></span>" Writing the transcript… it appears here a few minutes at a time."</p> }.into_any()
                        } else {
                            view! {
                                <div class="tr-empty">
                                    <p class="muted">"No transcript yet. It's written when the episode's checked for ads, then AI makes a summary and a table of contents from it."</p>
                                    {(e.id != 0).then(|| view! { <button class="ghost" on:click=prepare>"Make the transcript"</button> })}
                                </div>
                            }.into_any()
                        };
                    }
                    let q = query.get().trim().to_lowercase();
                    let all_ads = ads();
                    let all = lines.get();
                    if contents() {
                        let o = outline.get();
                        let open = opened.get();
                        let sections = o.sections.into_iter().enumerate().map(|(k, sec)| {
                            let is_open = open.contains(&k);
                            let len = sec.end - sec.start;
                            let start = sec.start;
                            let toggle = move |_| {
                                let closing = opened.with_untracked(|o| o.contains(&k));
                                if closing && now_section.get_untracked() == Some(k) {
                                    follow.set(false);
                                }
                                opened.update(|o| if !o.remove(&k) { o.insert(k); });
                            };
                            let subs_list = (!is_open && !sec.subsections.is_empty()).then(|| view! {
                                <ul class="ptoc-subs">
                                    {sec.subsections.iter().cloned().map(|sub| { let t = sub.start; view! {
                                        <li><button class="link ts" on:click=move |_| jump(t)>{format_time(t)}</button><span>{sub.title}</span></li>
                                    }}).collect_view()}
                                </ul>
                            });
                            let body = is_open.then(|| {
                                let mine: Vec<(usize, f64, String)> = all.iter().enumerate()
                                    .filter(|(_, (t, _))| *t >= sec.start - 0.5 && *t < sec.end - 0.5)
                                    .map(|(i, (t, x))| (i, *t, x.clone())).collect();
                                view! { <div class="ptoc-body">{transcript_rows(mine, &all_ads, "", &sec.subsections.iter().map(|x| (x.start, x.title.clone(), true)).collect::<Vec<_>>(), said, follow, words)}</div> }
                            });
                            view! {
                                <section class="ptoc-sec" class:open=is_open class:now=move || now_section.get() == Some(k)>
                                    <div class="ptoc-head">
                                        <button class="ptoc-toggle" on:click=toggle aria-expanded=is_open.to_string()>
                                            <svg viewBox="0 0 24 24" class="icon ptoc-chev" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 6l6 6-6 6"/></svg>
                                            <span class="ptoc-title">{sec.title.clone()}</span>
                                        </button>
                                        <button class="link ts" title="Play from here" on:click=move |_| jump(start)>{format_time(start)}</button>
                                        <small class="muted ptoc-len">{format!("{} min", (len / 60.0).round().max(1.0))}</small>
                                    </div>
                                    {(!sec.summary.is_empty()).then(|| view! { <p class="ptoc-sum">{sec.summary.clone()}</p> })}
                                    {subs_list}
                                    {body}
                                </section>
                            }
                        }).collect_view();
                        return view! {
                            <EpisodeSummary outline/>
                            <div class="ptoc-head-row">
                                <span class="muted small">"Contents"<span class="ai-badge" title="Titles and summaries written by AI from the transcript">"AI"</span></span>
                                <button class="link small" on:click=move |_| { let n = outline.with_untracked(|o| o.sections.len()); opened.update(|o| if o.len() == n { o.clear() } else { o.extend(0..n) }) }>
                                    {move || if opened.with(|o| o.len()) == outline.with(|o| o.sections.len()) { "Close all" } else { "Open all" }}
                                </button>
                            </div>
                            <div class="ptoc">{sections}</div>
                            <button class="link small ptoc-remake" on:click=remake>"Write the outline again"</button>
                        }.into_any();
                    }
                    let shown: Vec<(usize, f64, String)> = all.into_iter().enumerate().filter(|(_, (_, t))| q.is_empty() || t.to_lowercase().contains(&q)).map(|(i, (t, x))| (i, t, x)).collect();
                    if shown.is_empty() {
                        return view! { <p class="muted">"No lines match."</p> }.into_any();
                    }
                    let count = (!q.is_empty()).then(|| view! { <p class="tr-count muted small">{format!("{} line{} match", shown.len(), if shown.len() == 1 { "" } else { "s" })}</p> });
                    // (While there's no outline: how it's going, or a way to make one.)
                    let e = p.ep.get_untracked().unwrap_or_default();
                    let note = q.is_empty().then(|| outline.with(|o| match o.state.as_str() {
                        "making" => view! { <p class="tr-outline-note muted small"><span class="spinner tiny"></span>" AI is writing a summary and table of contents…"</p> }.into_any(),
                        "waiting" => view! { <p class="tr-outline-note muted small">"The summary and table of contents are written once the whole episode's transcribed."</p> }.into_any(),
                        "done" => ().into_any(),
                        _ if e.ads.busy() => view! { <p class="tr-outline-note muted small">"A summary and table of contents are written once the whole episode's transcribed."</p> }.into_any(),
                        st => {
                            let failed = (st == "failed").then(|| o.error.clone().unwrap_or_default());
                            view! {
                                <p class="tr-outline-note muted small">
                                    {failed.map(|e| format!("Couldn't write the outline: {e}. "))}
                                    <button class="link" on:click=remake>"Write a summary and table of contents"</button>
                                </p>
                            }.into_any()
                        }
                    }));
                    // (The whole text has the outline's headings and subheadings through it.)
                    let subs: Vec<(f64, String, bool)> = if q.is_empty() && has_outline() {
                        outline.with(|o| o.sections.iter().flat_map(|x| std::iter::once((x.start, x.title.clone(), false)).chain(x.subsections.iter().map(|y| (y.start, y.title.clone(), true)))).collect())
                    } else {
                        vec![]
                    };
                    view! { {note}{count}{transcript_rows(shown, &all_ads, &q, &subs, said, follow, words)} }.into_any()
                }}
            </div>
        </div>
    }
}

/// Times for a line's words when Whisper didn't give them (asking costs a lot more on AMD cards):
/// the line's time shared out by the words' lengths, which is close enough to light them up.
fn spread_words(start: f64, end: f64, text: &str) -> Vec<(f64, f64, String)> {
    let ws: Vec<&str> = text.split_whitespace().collect();
    let total: f64 = ws.iter().map(|w| w.chars().count() as f64 + 1.0).sum();
    let span = (end - start).max(0.1);
    let mut t = start;
    ws.into_iter()
        .map(|w| {
            let d = span * (w.chars().count() as f64 + 1.0) / total.max(1.0);
            let out = (t, t + d, w.to_string());
            t += d;
            out
        })
        .collect()
}

/// Transcript lines, ads marked, with a heading where each of `heads` starts.
fn transcript_rows(
    shown: Vec<(usize, f64, String)>,
    all_ads: &[duet_common::shows::AdSpan],
    q: &str,
    heads: &[(f64, String, bool)],
    said: Memo<Option<usize>>,
    follow: RwSignal<bool>,
    words: RwSignal<Vec<Vec<(f64, f64, String)>>>,
) -> impl IntoView + use<> {
    let time = player().time;
    let ad_of = |t: f64| all_ads.iter().position(|a| t >= a.start - 0.5 && t < a.end - 0.5);
    let mut prev_ad = None;
    let mut next_head = 0;
    shown
        .into_iter()
        .map(|(i, t, text)| {
            let mut here = Vec::new();
            while next_head < heads.len() && heads[next_head].0 <= t + 0.5 {
                here.push(heads[next_head].clone());
                next_head += 1;
            }
            let head = here.into_iter().map(|(_, h, sub)| if sub { view! { <h4 class="tr-head sub">{h}</h4> }.into_any() } else { view! { <h3 class="tr-head top">{h}</h3> }.into_any() }).collect_view();
            let ad = ad_of(t);
            let starts = ad.is_some() && ad != prev_ad;
            prev_ad = ad;
            let tag = ad.filter(|_| starts).map(|k| {
                let a = &all_ads[k];
                view! { <span class="tr-ad-tag">{format!("Ad · {} · {}", if a.label.is_empty() { "Ad" } else { &a.label }, format_time(a.end - a.start))}</span> }
            });
            let text = if q.is_empty() {
                // The line being said lights up word by word, as the audiobooks do.
                (move || {
                    let mine = (said.get() == Some(i)).then(|| words.with_untracked(|w| w.get(i).cloned().unwrap_or_default())).unwrap_or_default();
                    if mine.is_empty() {
                        return view! { <span>{text.clone()}</span> }.into_any();
                    }
                    let n = mine.len();
                    let starts: Vec<f64> = mine.iter().map(|w| w.0).collect();
                    view! {
                        <span>{mine.into_iter().enumerate().map(|(k, (a, b, w))| {
                            let next = starts.get(k + 1).copied().unwrap_or(b + 0.3);
                            view! {
                                <span class="tr-w" class:said={move || time.get() >= a} class:now={move || { let t = time.get(); t >= a && (t < next || k + 1 == n && t < b + 0.3) }}>{w}</span>
                                " "
                            }
                        }).collect_view()}</span>
                    }.into_any()
                })
                .into_any()
            } else {
                highlight(&text, q).into_any()
            };
            view! {
                {head}
                <p data-i=i.to_string() class:now=move || said.get() == Some(i) class:ad=ad.is_some()
                    on:click=move |_| { sp_seek(t); sp_play(); follow.set(true) }>
                    {tag}
                    <span class="ts">{format_time(t)}</span>{text}
                </p>
            }
        })
        .collect_view()
}

/// `text` with each match of `q` (lowercase) marked.
fn highlight(text: &str, q: &str) -> impl IntoView + use<> {
    let lower = text.to_lowercase();
    let mut parts = Vec::new();
    let mut at = 0;
    // (Lowercasing can change byte lengths for some scripts; fall back to unmarked then.)
    if lower.len() == text.len() {
        while let Some(k) = lower[at..].find(q) {
            let start = at + k;
            parts.push((text[at..start].to_string(), false));
            parts.push((text[start..start + q.len()].to_string(), true));
            at = start + q.len();
        }
    }
    parts.push((text[at..].to_string(), false));
    view! { <span>{parts.into_iter().map(|(t, m)| if m { view! { <mark>{t}</mark> }.into_any() } else { t.into_any() }).collect_view()}</span> }
}
