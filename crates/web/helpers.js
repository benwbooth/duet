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
  constructor(book, article, handlers, spreadPref) {
    this.book = book;
    this.article = article;
    this.h = handlers;
    this.spreadPref = spreadPref || "auto";
    this.page = 0;
    this.pages = 1;
    this.busy = false;
    this.layout();
    this.resize = new ResizeObserver(() => this.relayout());
    this.resize.observe(book.parentElement || book);
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
    this.book.querySelectorAll(".leaf, .leaf-static, .turn-shadow").forEach((e) => e.remove());
    this.book.classList.remove("spread");
    delete this.book.__pager;
  }

  // Lays out again (resize, one/two pages) keeping the same text on screen.
  relayout() {
    if (this.busy) return;
    const anchor = this.firstSentence();
    this.layout();
    const e = this.article.querySelector(`.s[data-s="${anchor}"]`);
    if (e) this.showElement(e, false);
  }

  setSpread(pref) {
    this.spreadPref = pref;
    this.relayout();
  }

  get width() {
    return this.book.clientWidth;
  }

  // Two facing pages when asked to, or (auto) when the window is wide and not tall.
  wantsSpread() {
    // Never two pages on a phone-sized screen, whatever the setting.
    if (this.book.clientWidth < 640 && (this.book.parentElement || this.book).clientWidth < 640) return false;
    if (this.spreadPref === "two") return true;
    if (this.spreadPref === "one") return false;
    const room = (this.book.parentElement || this.book).clientWidth;
    return room >= 980 && room > this.book.clientHeight * 1.25;
  }

  layout() {
    this.spread = this.wantsSpread();
    this.book.classList.toggle("spread", this.spread);
    const w = this.width;
    const pageW = this.spread ? w / 2 : w;
    const pad = Math.round(Math.min(56, Math.max(22, pageW * 0.08)));
    this.article.style.setProperty("--page-pad", `${pad}px`);
    // The height text can use on a page, so pictures can be sized to fit on one page.
    const cs = getComputedStyle(this.article);
    const colH = this.book.clientHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom);
    this.article.style.setProperty("--col-h", `${Math.max(120, colH)}px`);
    // A fixed count (not a width) so rounding can't drop a column; overflow continues to the right.
    this.article.style.columnWidth = "auto";
    this.article.style.columnCount = this.spread ? "2" : "1";
    this.article.style.columnGap = `${2 * pad}px`;
    this.pages = Math.max(1, Math.round(this.book.scrollWidth / w));
    // Columns can leave a sliver; trust the last element's position too.
    const last = this.article.lastElementChild;
    if (last) this.pages = Math.max(this.pages, this.pageOf(last, true) + 1);
    this.page = Math.min(this.page, this.pages - 1);
    this.place(this.article, this.page);
    this.report();
  }

  place(article, page) {
    article.style.transform = `translateX(${-page * this.width}px)`;
  }

  pageOf(el, unclamped) {
    const a = this.article.getBoundingClientRect();
    const r = el.getBoundingClientRect();
    const p = Math.floor((r.left - a.left + 1) / this.width);
    return unclamped ? Math.max(0, p) : Math.max(0, Math.min(this.pages - 1, p));
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
    // Page images (comics, PDF) load as they come near: this spread, and one either side.
    const imgs = this.article.querySelectorAll(".img-page img");
    if (imgs.length) {
      const per = this.spread ? 2 : 1;
      for (let i = (this.page - 1) * per; i < (this.page + 2) * per; i++) {
        if (imgs[i]) imgs[i].loading = "eager";
      }
    }
    if (this.h.onPage) this.h.onPage(this.page + 1, this.pages, this.spread);
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

  // A copy of the laid-out chapter, shifted so `offset` px of it sits at the left edge.
  copy(offset) {
    const c = this.article.cloneNode(true);
    c.classList.remove("synced");
    c.style.width = `${this.width}px`;
    c.style.transform = `translateX(${-offset}px)`;
    return c;
  }

  face(cls, offset) {
    const f = document.createElement("div");
    f.className = cls;
    if (offset !== null) f.appendChild(this.copy(offset));
    return f;
  }

  // A turning leaf over [x, x + w), hinged on its left or right edge. The front shows the text
  // at `front` px into the chapter, the back the text at `back` (or blank paper when null).
  makeLeaf({ x, w, hinge, front, back }) {
    const leaf = document.createElement("div");
    leaf.className = "leaf";
    leaf.style.left = `${x}px`;
    leaf.style.width = `${w}px`;
    leaf.style.transformOrigin = `${hinge} center`;
    const f = this.face("leaf-front", front);
    const shade = document.createElement("div");
    shade.className = "leaf-shade";
    shade.style.background = `linear-gradient(to ${hinge === "left" ? "left" : "right"}, rgba(0,0,0,0.38), rgba(0,0,0,0.06) 30%, rgba(255,255,255,0.12) 55%, rgba(0,0,0,0) 80%)`;
    // A soft highlight where the paper bends toward the light.
    const gloss = document.createElement("div");
    gloss.className = "leaf-gloss";
    gloss.style.background = `linear-gradient(to ${hinge === "left" ? "left" : "right"}, rgba(255,255,255,0) 0%, rgba(255,255,255,0.35) 6%, rgba(255,255,255,0) 16%)`;
    f.append(shade, gloss);
    const b = this.face("leaf-back", back);
    if (back === null && front !== null) {
      // A single page's back: the same page seen through the paper, faint and mirrored.
      const wrap = document.createElement("div");
      wrap.className = "ghost";
      wrap.appendChild(this.copy(front));
      b.appendChild(wrap);
    }
    leaf.append(f, b);
    return { leaf, shade, gloss };
  }

  // A still copy of half a spread, kept on top until the turning leaf lands on it.
  makeStatic(x, w, offset) {
    const s = document.createElement("div");
    s.className = "leaf-static";
    s.style.left = `${x}px`;
    s.style.width = `${w}px`;
    s.appendChild(this.copy(offset));
    return s;
  }

  // Sets the leaf's turn: 0 = flat where it started, 1 = turned all the way over.
  // `y` (0 top .. 1 bottom) is where the page is being held: pulling from a corner tips the page
  // that way, so it curls from the corner instead of turning like a door.
  pose(t, x, y) {
    const p = t.parts;
    if (y !== undefined) t.y = y;
    const hold = (t.y === undefined ? 0.5 : t.y) - 0.5;
    const angle = t.sign * 180 * x;
    // A slight lift and skew in mid-turn reads as paper bending rather than a rigid card.
    const lift = Math.sin(Math.PI * x);
    const tilt = -hold * 14 * lift;
    // Perspective lives on the leaf, not the book, so the book still clips what's outside it.
    p.leaf.style.transform =
      `perspective(2200px) rotateY(${angle.toFixed(2)}deg) rotateX(${tilt.toFixed(2)}deg) ` +
      `skewY(${(t.sign * (3 + hold * 6) * lift).toFixed(2)}deg) scale(${(1 + 0.015 * lift).toFixed(4)})`;
    p.shade.style.opacity = (0.55 * lift).toFixed(3);
    p.gloss.style.opacity = (0.9 * lift).toFixed(3);
    p.gloss.style.transform = `translateX(${(t.sign * -40 * lift).toFixed(1)}px)`;
    if (t.spread) {
      p.shadow.style.opacity = (0.4 * lift).toFixed(3);
    } else {
      // The shadow falls just beyond the lifted edge, on the page being uncovered.
      const W = this.width;
      const edge = Math.max(0, W * Math.cos(Math.PI * x));
      p.shadow.style.opacity = (0.9 * Math.min(1, lift * 1.4)).toFixed(3);
      p.shadow.style.transform = "none";
      p.shadow.style.background =
        `linear-gradient(to right, rgba(0,0,0,0) ${Math.max(0, edge - 2).toFixed(0)}px, ` +
        `rgba(0,0,0,${(0.32 * lift).toFixed(3)}) ${edge.toFixed(0)}px, rgba(0,0,0,0) ${(edge + 90 * lift + 10).toFixed(0)}px)`;
    }
  }

  animate(t, from, to, ms) {
    return new Promise((resolve) => {
      const start = performance.now();
      // Quick to lift, gentle as it settles.
      const ease = (x) => 1 - Math.pow(1 - x, 4);
      const step = (now) => {
        const x = Math.min(1, (now - start) / ms);
        this.pose(t, from + (to - from) * ease(x));
        if (x < 1) requestAnimationFrame(step);
        else resolve();
      };
      requestAnimationFrame(step);
    });
  }

  // Sets up a turn. dir 1 shows the next page (spread), dir -1 the previous one.
  // Returns where the leaf starts and where it ends when the turn completes.
  begin(dir) {
    this.busy = true;
    const W = this.width;
    const half = W / 2;
    const cur = this.page * W;
    const target = this.page + dir;
    const next = target * W;
    const shadow = document.createElement("div");
    shadow.className = "turn-shadow";
    const t = { dir, target, spread: this.spread, extra: [shadow] };
    if (!this.spread) {
      // One page: the current page lifts off to the left, or the previous one falls back.
      t.sign = -1;
      t.parts = this.makeLeaf({ x: 0, w: W, hinge: "left", front: dir > 0 ? cur : next, back: null });
      [t.start, t.end] = dir > 0 ? [0, 1] : [1, 0];
      if (dir > 0) this.place(this.article, target);
    } else if (dir > 0) {
      // Two pages: the right page turns over the spine; its back is the next left page.
      t.sign = -1;
      t.parts = this.makeLeaf({ x: half, w: half, hinge: "left", front: cur + half, back: next });
      t.extra.push(this.makeStatic(0, half, cur));
      [t.start, t.end] = [0, 1];
      shadow.style.left = `${half}px`;
      shadow.style.width = `${half}px`;
      this.place(this.article, target);
    } else {
      // Two pages, backwards: the left page turns over to the right; its back is the previous right page.
      t.sign = 1;
      t.parts = this.makeLeaf({ x: 0, w: half, hinge: "right", front: cur, back: next + half });
      t.extra.push(this.makeStatic(half, half, cur + half));
      [t.start, t.end] = [0, 1];
      shadow.style.width = `${half}px`;
      shadow.style.background = "linear-gradient(to right, rgba(0,0,0,0.3), rgba(0,0,0,0) 40%)";
      this.place(this.article, target);
    }
    t.parts.shadow = shadow;
    this.book.append(...t.extra, t.parts.leaf);
    this.pose(t, t.start);
    return t;
  }

  async finish(t, from, complete) {
    const to = complete ? t.end : t.start;
    await this.animate(t, from, to, TURN_MS * Math.max(0.35, Math.abs(to - from)));
    if (complete) this.page = t.target;
    this.place(this.article, this.page);
    if (complete) this.report();
    t.parts.leaf.remove();
    t.extra.forEach((e) => e.remove());
    this.busy = false;
  }

  async turn(dir) {
    const t = this.begin(dir);
    t.y = 0.78;
    await this.finish(t, t.start, true);
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
      const reach = this.spread ? this.width * 0.7 : this.width * 0.85;
      const p = Math.max(0, Math.min(1, Math.abs(dx) / reach));
      const rect = this.book.getBoundingClientRect();
      this.pose(drag, drag.start + (drag.end - drag.start) * p, (e.clientY - rect.top) / rect.height);
      drag.p = p;
    });
    const release = () => {
      if (drag) {
        const fast = performance.now() - start.time < 250;
        const p = drag.p || 0;
        this.finish(drag, drag.start + (drag.end - drag.start) * p, p > 0.3 || (fast && p > 0.05));
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
        const edge = this.spread ? 0.12 : 0.22;
        if (x < edge) { e.stopPropagation(); this.prev(); }
        else if (x > 1 - edge) { e.stopPropagation(); this.next(); }
      },
      true,
    );
  }
}

export function pagerInit(book, article, onNext, onPrev, onPage, spread) {
  if (book.__pager) book.__pager.destroy();
  book.__pager = new Pager(book, article, { onNext, onPrev, onPage }, spread);
}

// "one", "two" or "auto" (two facing pages on wide windows).
export function pagerSetSpread(book, spread) {
  if (book && book.__pager) book.__pager.setSpread(spread);
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

// The text of sentence n in a chapter (a sentence can be split over several spans).
export function sentenceText(container, n) {
  if (!container || n < 0) return "";
  return Array.from(container.querySelectorAll(`.s[data-s="${n}"]`)).map((e) => e.textContent).join(" ").replace(/\s+/g, " ").trim();
}

// Asks for a URL ahead of time (the full cast's next sentences), ignoring the answer.
export async function prefetch(url) {
  try {
    const r = await fetch(url);
    await r.arrayBuffer();
    return r.ok;
  } catch (_) {
    return false;
  }
}
