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

// Highlights sentence `n` in the reader and optionally brings it into view (scrolling, or
// turning to its page in the paged reader).
export function highlight(container, n, scroll) {
  if (!container) return;
  container.querySelectorAll(".s.now").forEach((e) => e.classList.remove("now"));
  if (n < 0) return;
  const spans = container.querySelectorAll(`.s[data-s="${n}"]`);
  spans.forEach((e) => e.classList.add("now"));
  if (!scroll || !spans.length) return;
  const pager = pagerOf(container);
  if (pager) {
    pager.showElement(spans[0], true);
    return;
  }
  const r = spans[0].getBoundingClientRect();
  if (r.top < 80 || r.bottom > window.innerHeight - 160) {
    spans[0].scrollIntoView({ behavior: "smooth", block: "center" });
  }
}

// The sentence at the top of the screen (or page), so "resume" and the assistant know where you are.
export function visibleSentence(container) {
  if (!container) return 0;
  const pager = pagerOf(container);
  if (pager) return pager.firstSentence();
  for (const e of container.querySelectorAll(".s")) {
    if (e.getBoundingClientRect().bottom > 70) return Number(e.dataset.s);
  }
  return 0;
}

export function scrollToSentence(container, n) {
  const e = container && container.querySelector(`.s[data-s="${n}"]`);
  const pager = pagerOf(container);
  if (pager) {
    if (e) pager.showElement(e, false);
    else pager.go(0, false);
    return;
  }
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

// ---------------------------------------------------------------------------------------------
// The paged reader: the chapter is laid out in CSS columns, one column per page, and pages turn
// with a 3D leaf that follows your finger or mouse. Tap the left or right edge, swipe, or use the
// arrow keys.

function pagerOf(container) {
  const book = container && container.closest(".bookview");
  return book && book.__pager;
}

const TURN_MS = 520;

class Pager {
  constructor(book, article, handlers) {
    this.book = book;
    this.article = article;
    this.h = handlers;
    this.page = 0;
    this.pages = 1;
    this.busy = false;
    this.layout();
    this.resize = new ResizeObserver(() => {
      const anchor = this.firstSentence();
      this.layout();
      const e = this.article.querySelector(`.s[data-s="${anchor}"]`);
      if (e) this.showElement(e, false);
    });
    this.resize.observe(book);
    this.onKey = (e) => {
      if (e.target.closest && e.target.closest("input, textarea, select")) return;
      if (["ArrowRight", "PageDown", " "].includes(e.key)) { e.preventDefault(); this.next(); }
      if (["ArrowLeft", "PageUp"].includes(e.key)) { e.preventDefault(); this.prev(); }
    };
    document.addEventListener("keydown", this.onKey);
    this.bindPointer();
  }

  destroy() {
    this.resize.disconnect();
    document.removeEventListener("keydown", this.onKey);
    this.book.querySelectorAll(".leaf, .turn-shadow").forEach((e) => e.remove());
    delete this.book.__pager;
  }

  get width() {
    return this.book.clientWidth;
  }

  layout() {
    const w = this.width;
    const pad = Math.round(Math.min(56, Math.max(22, w * 0.07)));
    this.article.style.setProperty("--page-w", `${w}px`);
    this.article.style.setProperty("--page-pad", `${pad}px`);
    this.article.style.columnWidth = `${w - 2 * pad}px`;
    this.article.style.columnGap = `${2 * pad}px`;
    this.pages = Math.max(1, Math.round(this.book.scrollWidth / w));
    // Columns can leave a sliver; trust the last element's position too.
    const last = this.article.lastElementChild;
    if (last) this.pages = Math.max(this.pages, this.pageOf(last) + 1);
    this.page = Math.min(this.page, this.pages - 1);
    this.place(this.article, this.page);
    this.report();
  }

  place(article, page) {
    article.style.transform = `translateX(${-page * this.width}px)`;
  }

  pageOf(el) {
    const a = this.article.getBoundingClientRect();
    const r = el.getBoundingClientRect();
    const x = r.left - a.left + 1;
    return Math.max(0, Math.min(this.pages - 1, Math.floor(x / this.width)));
  }

  firstSentence() {
    const left = this.book.getBoundingClientRect().left;
    for (const e of this.article.querySelectorAll(".s")) {
      const r = e.getBoundingClientRect();
      if (r.width && r.left >= left - 2 && r.left < left + this.width) return Number(e.dataset.s);
    }
    return 0;
  }

  report() {
    if (this.h.onPage) this.h.onPage(this.page + 1, this.pages);
  }

  showElement(el, animate) {
    const p = this.pageOf(el);
    if (p !== this.page) this.go(p, animate);
  }

  go(p, animate) {
    p = Math.max(0, Math.min(this.pages - 1, p));
    if (p === this.page) return;
    if (animate && Math.abs(p - this.page) === 1 && !this.busy) {
      this.turn(p > this.page ? 1 : -1);
      return;
    }
    this.page = p;
    this.place(this.article, p);
    this.report();
  }

  end() {
    this.go(this.pages - 1, false);
  }

  next() {
    if (this.busy) return;
    if (this.page + 1 < this.pages) this.turn(1);
    else if (this.h.onNext) this.h.onNext();
  }

  prev() {
    if (this.busy) return;
    if (this.page > 0) this.turn(-1);
    else if (this.h.onPrev) this.h.onPrev();
  }

  // A leaf: a copy of one page that rotates around the left edge like a page of a book.
  makeLeaf(page) {
    const leaf = document.createElement("div");
    leaf.className = "leaf";
    const front = document.createElement("div");
    front.className = "leaf-front";
    const copy = this.article.cloneNode(true);
    copy.classList.remove("synced");
    copy.removeAttribute("id");
    this.place(copy, page);
    front.appendChild(copy);
    const shade = document.createElement("div");
    shade.className = "leaf-shade";
    front.appendChild(shade);
    const back = document.createElement("div");
    back.className = "leaf-back";
    leaf.append(front, back);
    const shadow = document.createElement("div");
    shadow.className = "turn-shadow";
    this.book.append(shadow, leaf);
    return { leaf, shade, shadow };
  }

  // Sets the leaf's angle: 0 = lying flat over the page, 1 = turned all the way over.
  pose(parts, t) {
    const angle = -180 * t;
    // A slight lift and skew in mid-turn reads as paper bending rather than a rigid card.
    const lift = Math.sin(Math.PI * t);
    // Perspective lives on the leaf, not the book, so the book still clips what's outside it.
    parts.leaf.style.transform = `perspective(2400px) rotateY(${angle}deg) skewY(${(-3 * lift).toFixed(2)}deg)`;
    parts.shade.style.opacity = (0.55 * lift).toFixed(3);
    parts.shadow.style.opacity = (0.45 * (1 - t)).toFixed(3);
    parts.shadow.style.transform = `translateX(${(-this.width * t * 0.6).toFixed(1)}px)`;
  }

  animate(parts, from, to, ms) {
    return new Promise((resolve) => {
      const start = performance.now();
      const ease = (x) => 1 - Math.pow(1 - x, 3);
      const step = (now) => {
        const x = Math.min(1, (now - start) / ms);
        this.pose(parts, from + (to - from) * ease(x));
        if (x < 1) requestAnimationFrame(step);
        else resolve();
      };
      requestAnimationFrame(step);
    });
  }

  // dir 1: the current page lifts away to reveal the next; dir -1: the previous page falls back.
  begin(dir) {
    this.busy = true;
    const target = this.page + dir;
    let parts;
    if (dir > 0) {
      parts = this.makeLeaf(this.page);
      this.place(this.article, target);
      this.pose(parts, 0);
    } else {
      parts = this.makeLeaf(target);
      this.pose(parts, 1);
    }
    return { parts, dir, target };
  }

  async finish(t, from, complete) {
    const { parts, dir, target } = t;
    const done = dir > 0 ? (complete ? 1 : 0) : (complete ? 0 : 1);
    await this.animate(parts, from, done, TURN_MS * Math.max(0.35, Math.abs(done - from)));
    if (complete) {
      this.page = target;
      this.place(this.article, target);
      this.report();
    } else {
      this.place(this.article, this.page);
    }
    parts.leaf.remove();
    parts.shadow.remove();
    this.busy = false;
  }

  async turn(dir) {
    const t = this.begin(dir);
    await this.finish(t, dir > 0 ? 0 : 1, true);
  }

  bindPointer() {
    let start = null;
    let drag = null;
    this.book.addEventListener("pointerdown", (e) => {
      if (this.busy || e.button !== 0) return;
      start = { x: e.clientX, y: e.clientY, time: performance.now() };
    });
    this.book.addEventListener("pointermove", (e) => {
      if (!start) return;
      const dx = e.clientX - start.x;
      if (!drag) {
        if (Math.abs(dx) < 12 || Math.abs(dx) < Math.abs(e.clientY - start.y)) return;
        const dir = dx < 0 ? 1 : -1;
        if ((dir > 0 && this.page + 1 >= this.pages) || (dir < 0 && this.page === 0)) return;
        if (String(window.getSelection()) !== "") return;
        drag = this.begin(dir);
        this.book.setPointerCapture(e.pointerId);
      }
      const p = Math.max(0, Math.min(1, Math.abs(dx) / (this.width * 0.85)));
      this.pose(drag.parts, drag.dir > 0 ? p : 1 - p);
      drag.p = p;
    });
    const release = (e) => {
      if (drag) {
        const fast = performance.now() - start.time < 250;
        const p = drag.p || 0;
        const from = drag.dir > 0 ? p : 1 - p;
        this.finish(drag, from, p > 0.3 || (fast && p > 0.05));
        // The click that follows a drag isn't a tap.
        this.swallowClick = true;
        setTimeout(() => (this.swallowClick = false), 50);
      }
      start = null;
      drag = null;
    };
    this.book.addEventListener("pointerup", release);
    this.book.addEventListener("pointercancel", release);
    // Taps on the outer edges turn the page; taps elsewhere reach the sentences.
    this.book.addEventListener(
      "click",
      (e) => {
        if (this.swallowClick) {
          e.stopPropagation();
          e.preventDefault();
          return;
        }
        if (e.target.closest("a[data-href]") || String(window.getSelection()) !== "") return;
        const x = (e.clientX - this.book.getBoundingClientRect().left) / this.width;
        if (x < 0.22) { e.stopPropagation(); this.prev(); }
        else if (x > 0.78) { e.stopPropagation(); this.next(); }
      },
      true,
    );
  }
}

export function pagerInit(book, article, onNext, onPrev, onPage) {
  if (book.__pager) book.__pager.destroy();
  book.__pager = new Pager(book, article, { onNext, onPrev, onPage });
}

export function pagerDestroy(book) {
  if (book && book.__pager) book.__pager.destroy();
}

export function pagerEnd(book) {
  if (book && book.__pager) book.__pager.end();
}

export function pagerTurn(book, dir) {
  if (!book || !book.__pager) return;
  if (dir > 0) book.__pager.next();
  else book.__pager.prev();
}
