// Browser bits that are simpler in JavaScript: the microphone, highlighting, scrolling.

let recorder = null;
let chunks = [];

export function micSupported() {
  return !!(navigator.mediaDevices && window.MediaRecorder);
}

export async function startRecording() {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  chunks = [];
  recorder = new MediaRecorder(stream);
  recorder.ondataavailable = (e) => chunks.push(e.data);
  recorder.start();
}

// Stops recording and returns the transcript from the server.
export async function stopRecording() {
  if (!recorder) return "";
  const done = new Promise((resolve) => (recorder.onstop = resolve));
  recorder.stop();
  await done;
  recorder.stream.getTracks().forEach((t) => t.stop());
  const type = recorder.mimeType || "audio/webm";
  recorder = null;
  const ext = type.includes("mp4") ? "m4a" : type.includes("ogg") ? "ogg" : "webm";
  const form = new FormData();
  form.append("audio", new Blob(chunks, { type }), `speech.${ext}`);
  const resp = await fetch("/api/assistant/transcribe", { method: "POST", body: form });
  const body = await resp.json();
  if (!resp.ok) throw new Error(body.error || `HTTP ${resp.status}`);
  return body;
}

// Highlights sentence `n` in the reader and optionally scrolls it into view.
export function highlight(container, n, scroll) {
  if (!container) return;
  container.querySelectorAll(".s.now").forEach((e) => e.classList.remove("now"));
  if (n < 0) return;
  const spans = container.querySelectorAll(`.s[data-s="${n}"]`);
  spans.forEach((e) => e.classList.add("now"));
  if (scroll && spans.length) {
    const r = spans[0].getBoundingClientRect();
    if (r.top < 80 || r.bottom > window.innerHeight - 160) {
      spans[0].scrollIntoView({ behavior: "smooth", block: "center" });
    }
  }
}

// The sentence nearest the top of the screen, so "resume" and the assistant know where you are.
export function visibleSentence(container) {
  if (!container) return 0;
  for (const e of container.querySelectorAll(".s")) {
    if (e.getBoundingClientRect().bottom > 70) return Number(e.dataset.s);
  }
  return 0;
}

export function scrollToSentence(container, n) {
  const e = container && container.querySelector(`.s[data-s="${n}"]`);
  if (e) e.scrollIntoView({ block: "center" });
  else window.scrollTo(0, 0);
}

// The sentence number or internal link under a click inside the reader.
export function clickTarget(ev) {
  const link = ev.target.closest("a[data-href]");
  if (link) {
    ev.preventDefault();
    return { link: link.dataset.href };
  }
  const s = ev.target.closest(".s");
  // A text selection is not a tap on a sentence.
  if (s && String(window.getSelection()) === "") return { sentence: Number(s.dataset.s) };
  return {};
}

export function selectedText() {
  return String(window.getSelection()).trim();
}

export function playDataUrl(url) {
  new Audio(url).play();
}

export function formatTime(secs) {
  secs = Math.max(0, Math.floor(secs));
  const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
  return (h ? `${h}:${String(m).padStart(2, "0")}` : `${m}`) + `:${String(s).padStart(2, "0")}`;
}

export function setMediaSession(title, artist, artwork) {
  if (!("mediaSession" in navigator)) return;
  navigator.mediaSession.metadata = new MediaMetadata({
    title, artist, artwork: artwork ? [{ src: artwork }] : [],
  });
}
