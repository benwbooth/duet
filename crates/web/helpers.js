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
// like paper: the corner follows your finger or mouse and the sheet folds back over itself. Tap the left or right edge, swipe, or use the
// arrow keys.

function pagerOf(container) {
  const book = container && container.closest(".bookview");
  return book && book.__pager;
}

const TURN_MS = 620;
// Strips a turning page is made of (more bends more smoothly, but costs more to draw).
const STRIPS = 14;

// The next animation frame, or a timer when frames aren't coming (a hidden or throttled page),
// so a page turn always finishes.
function frame(cb) {
  let ran = false;
  const id = requestAnimationFrame((now) => { ran = true; cb(now); });
  setTimeout(() => {
    if (!ran) {
      cancelAnimationFrame(id);
      cb(performance.now());
    }
  }, 50);
}

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
    // Arriving from the neighbouring chapter: finish the page turn that started there (after the
    // reader has gone to this chapter's first or last page).
    const carry = book.__carry;
    delete book.__carry;
    if (carry && carry.spread === this.spread) setTimeout(() => this.turn(carry.dir, carry), 0);
  }

  destroy() {
    this.resize.disconnect();
    document.removeEventListener("keydown", this.onKey);
    this.book.querySelectorAll(".curl-layer, .page3d").forEach((e) => e.remove());
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

  // Two facing pages, like an open book, whenever there's room for two readable pages (or when
  // asked to); one page only on narrow screens or when chosen.
  wantsSpread() {
    // Never two pages on a phone-sized screen, whatever the setting.
    if (this.book.clientWidth < 640 && (this.book.parentElement || this.book).clientWidth < 640) return false;
    if (this.spreadPref === "two") return true;
    if (this.spreadPref === "one") return false;
    const room = (this.book.parentElement || this.book).clientWidth;
    return room >= 720;
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
    if (this.busy) { this.queued = 1; return; }
    if (this.page + 1 < this.pages) this.turn(1);
    else if (this.h.onNext) this.carryInto(1, this.h.onNext);
  }

  prev() {
    if (this.busy) { this.queued = -1; return; }
    if (this.page > 0) this.turn(-1);
    else if (this.h.onPrev) this.carryInto(-1, this.h.onPrev);
  }

  // Turning past the chapter's first or last page: the page on screen is kept, the neighbouring
  // chapter is laid out underneath, and the kept page turns away over it.
  carryInto(dir, go) {
    this.book.__carry = { dir, snap: this.article.cloneNode(true), at: this.page * this.width, spread: this.spread };
    go();
  }

  // A copy of the laid-out chapter (or of a kept one), shifted so `offset` px of it sits at the
  // left edge of the book.
  copy(offset, from) {
    const c = (from || this.article).cloneNode(true);
    c.classList.remove("synced");
    c.querySelectorAll(".s.now").forEach((e) => e.classList.remove("now"));
    c.style.width = `${this.width}px`;
    c.style.transform = `translateX(${-offset}px)`;
    return c;
  }

  layer(cls, content) {
    const l = document.createElement("div");
    l.className = `curl-layer ${cls}`;
    l.style.width = `${this.width}px`;
    if (content) l.appendChild(content);
    return l;
  }

  // Sets up a turn: dir 1 shows the next page (or spread), -1 the previous one. With `carry`, the
  // page being turned is the kept page of the chapter just left.
  //
  // The page is a real 3D sheet hinged at the spine, made of vertical strips: each strip turns a
  // little further than the one before, so the page bends along its width as it swings over,
  // the free edge leading and the bound edge lying flat until the page has to lift from it.
  begin(dir, carry) {
    this.busy = true;
    const W = this.width, H = this.book.clientHeight, half = W / 2;
    const cur = carry ? carry.at : this.page * W;
    const src = carry ? carry.snap : null;
    const target = carry ? this.page : this.page + dir;
    const next = target * W;
    const t = { dir, target, W, H, layers: [] };
    let front, back, ghost = false;
    if (!this.spread) {
      // One page: forward, this page peels off to the left; back, the previous one is laid back down.
      t.leaf = [0, W];
      t.hinge = 0;
      t.single = true;
      t.C0x = W;
      if (dir > 0) {
        front = this.copy(cur, src);
        back = this.copy(cur, src);
        if (!carry) this.place(this.article, target);
      } else {
        front = this.copy(next);
        back = this.copy(next);
        if (carry) t.layers.push(this.layer("curl-under", this.copy(cur, src)));
      }
      ghost = true;
      t.reverse = dir < 0;
    } else if (dir > 0) {
      // Two pages: the right page turns over the spine; its back is the next left page.
      t.leaf = [half, W];
      t.hinge = half;
      t.C0x = W;
      front = this.copy(cur, src);
      back = this.copy(next);
      t.layers.push(this.halfLayer(0, this.copy(cur, src)));
      if (!carry) this.place(this.article, target);
    } else {
      // Two pages, backwards: the left page turns over to the right; its back is the previous right page.
      t.leaf = [0, half];
      t.hinge = half;
      t.C0x = 0;
      front = this.copy(cur, src);
      back = this.copy(next);
      t.layers.push(this.halfLayer(half, this.copy(cur, src)));
      if (!carry) this.place(this.article, target);
    }
    // The page underneath darkens in the turning page's shadow.
    t.under = this.layer("curl-shadow");
    const sigma = t.C0x > t.hinge ? 1 : -1;
    t.sigma = sigma;
    const w = t.leaf[1] - t.leaf[0];
    const sw = w / STRIPS;
    t.sw = sw;
    const scene = document.createElement("div");
    scene.className = "page3d";
    scene.style.perspectiveOrigin = `${t.hinge}px 50%`;
    // A slice of a laid-out page: the part of the book between x and x + sw.
    const slice = (content, x, mirrored) => {
      const face = document.createElement("div");
      face.className = "p3-content";
      const inner = document.createElement("div");
      inner.className = "p3-slice";
      inner.style.left = `${-x}px`;
      inner.style.width = `${W}px`;
      inner.appendChild(content.cloneNode(true));
      face.appendChild(inner);
      if (mirrored) face.style.transform = "scaleX(-1)";
      return face;
    };
    t.strips = [];
    let parent = scene;
    for (let i = 0; i < STRIPS; i++) {
      const strip = document.createElement("div");
      strip.className = "p3-strip";
      strip.style.width = `${sw + 0.6}px`;
      strip.style.height = `${H}px`;
      strip.style.transformOrigin = sigma > 0 ? "0 50%" : "100% 50%";
      strip.style.left = i === 0 ? `${sigma > 0 ? t.hinge : t.hinge - sw}px` : `${sigma > 0 ? sw : -sw}px`;
      const frontX = sigma > 0 ? t.hinge + i * sw : t.hinge - (i + 1) * sw;
      const f = document.createElement("div");
      f.className = "p3-face";
      f.appendChild(slice(front, frontX, false));
      const fs = document.createElement("div");
      fs.className = "curl-shade";
      f.appendChild(fs);
      const b = document.createElement("div");
      b.className = "p3-face p3-back" + (ghost ? " ghosted" : "");
      // The back: the next page where this strip lands once turned (two pages), or this page
      // seen through the paper (one page).
      b.appendChild(ghost ? slice(front, frontX, true) : slice(back, sigma > 0 ? t.hinge - (i + 1) * sw : t.hinge + i * sw, false));
      const bs = document.createElement("div");
      bs.className = "curl-shade";
      b.appendChild(bs);
      strip.append(f, b);
      parent.appendChild(strip);
      t.strips.push({ el: strip, fs, bs });
      parent = strip;
    }
    t.scene = scene;
    t.layers.push(t.under, scene);
    this.book.append(...t.layers);
    return t;
  }

  halfLayer(x, content) {
    const s = this.layer("curl-static", content);
    s.style.clipPath = `inset(0 ${this.width - x - this.width / 2}px 0 ${x}px)`;
    return s;
  }

  // Where the free edge rests (flat) and where it lands when the page has turned all the way.
  ends(t, y) {
    const C = { x: t.C0x, y };
    const F = { x: 2 * t.hinge - t.C0x, y };
    return t.reverse ? [F, C] : [C, F];
  }

  // Turns the sheet so its free edge is over x = P.x (P.y is where it's held, which tips it).
  pose(t, P) {
    const { H } = t;
    const sigma = t.sigma, w = t.leaf[1] - t.leaf[0];
    const theta = Math.acos(Math.max(-1, Math.min(1, (sigma * (P.x - t.hinge)) / w)));
    // Faster movement flexes the paper more; it eases back as it slows.
    const now = performance.now();
    if (t.lastP) {
      const dt = Math.max(1, now - t.lastT);
      const speed = Math.abs(P.x - t.lastP.x) / dt;
      t.flex = (t.flex || 0) * 0.8 + Math.min(0.6, speed * 0.25) * 0.2;
    }
    t.lastP = P;
    t.lastT = now;
    // The free edge leads; the bound edge stays flat until the page has to lift from the spine.
    const bend = Math.min(1.5, (0.95 + (t.flex || 0)) * Math.sin(theta));
    const hinge = Math.max(0, theta - bend);
    const hold = Math.max(-0.5, Math.min(0.5, (t.C ? t.C.y : H / 2) / H - 0.5));
    let prev = 0, reach = 0, x = 0;
    t.strips.forEach(({ el, fs, bs }, i) => {
      const u = (i + 1) / STRIPS;
      const a = hinge + (theta - hinge) * u * u;
      const turn = a - prev;
      prev = a;
      const tilt = i === 0 ? `rotateX(${(-hold * 14 * Math.sin(theta)).toFixed(2)}deg) ` : "";
      el.style.transform = `${tilt}rotateY(${(-sigma * turn).toFixed(4)}rad)`;
      // Light from in front: paper darkens as it turns edge-on, the back brightens as it comes
      // round to face you, with a gloss where it catches the light.
      // Shaded smoothly across each strip, from the angle at its inner edge to its outer one.
      const a0 = a - turn;
      const frontShade = (ang) => `rgba(0,0,0,${(0.42 * (1 - Math.max(0, Math.cos(ang))) ** 1.5).toFixed(3)})`;
      const backShade = (ang) => {
        const gloss = 0.3 * Math.exp(-(((ang - 0.7 * Math.PI) / (0.13 * Math.PI)) ** 2));
        const dark = 0.4 * (Math.max(0, 1 + Math.cos(ang)) / 2) ** 1.5;
        // Gloss lightens, shade darkens: one colour that does both.
        return gloss > dark ? `rgba(255,255,255,${(gloss - dark).toFixed(3)})` : `rgba(0,0,0,${(dark - gloss).toFixed(3)})`;
      };
      const out = sigma > 0 ? "to right" : "to left";
      const back = sigma > 0 ? "to left" : "to right";
      fs.style.background = `linear-gradient(${out}, ${frontShade(a0)}, ${frontShade(a)})`;
      bs.style.background = `linear-gradient(${back}, ${backShade(a0)}, ${backShade(a)})`;
      const c = Math.cos(a);
      x += (w / STRIPS) * c;
      reach = Math.max(reach, x);
    });
    t.progress = t.reverse ? 1 - theta / Math.PI : theta / Math.PI;
    // The turning page's shadow on the page it uncovers, just past its furthest reach.
    const lift = Math.sin(theta);
    const edge = t.hinge + sigma * Math.max(0, reach);
    const fade = 30 + 140 * lift;
    const dir = sigma > 0 ? "to right" : "to left";
    const from = sigma > 0 ? edge : t.W - edge;
    t.under.style.opacity = lift > 0.01 ? "1" : "0";
    t.under.style.background =
      `linear-gradient(${dir}, rgba(0,0,0,0) ${(from - 4).toFixed(0)}px, rgba(0,0,0,${(0.34 * lift).toFixed(3)}) ${from.toFixed(0)}px, rgba(0,0,0,0) ${(from + fade).toFixed(0)}px)`;
    t.under.style.clipPath = `inset(0 ${t.W - t.leaf[1]}px 0 ${t.leaf[0]}px)`;
  }

  animate(t, from, to, ms, arc) {
    return new Promise((resolve) => {
      const start = performance.now();
      const ease = (x) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2);
      const step = (now) => {
        const x = Math.min(1, (now - start) / ms);
        const e = ease(x);
        // Turned by a tap, the corner lifts as it travels, as a finger would carry it.
        const up = arc ? -arc * Math.sin(Math.PI * e) : 0;
        this.pose(t, { x: from.x + (to.x - from.x) * e, y: from.y + (to.y - from.y) * e + up });
        if (x < 1) frame(step);
        else resolve();
      };
      frame(step);
    });
  }

  async finish(t, from, complete, arc) {
    const [rest, landed] = this.ends(t, t.C.y);
    const to = complete === !t.reverse ? landed : rest;
    const left = Math.abs(to.x - from.x) / (Math.abs(landed.x - rest.x) || 1);
    await this.animate(t, from, to, TURN_MS * Math.max(0.4, Math.min(1, left)), arc);
    const done = complete;
    if (done) this.page = t.target;
    this.place(this.article, this.page);
    if (done) this.report();
    t.layers.forEach((e) => e.remove());
    this.busy = false;
    if (this.queued) {
      const q = this.queued;
      this.queued = 0;
      q > 0 ? this.next() : this.prev();
    }
  }

  async turn(dir, carry) {
    const t = this.begin(dir, carry);
    const H = t.H;
    t.C = { x: t.C0x, y: H * 0.94 };
    const [rest] = this.ends(t, t.C.y);
    this.pose(t, rest);
    await this.finish(t, rest, true, H * 0.16);
  }

  bindPointer() {
    let start = null;
    let drag = null;
    // Where a mouse can take hold of the page: the outer part of the page being turned (the rest
    // of the page selects text as usual). A finger can take hold anywhere.
    const grabZone = (e) => {
      if (e.pointerType === "touch") return true;
      // The page's outer margins (and a little into them from the text), like the edge of a page
      // you'd pinch to turn.
      const x = e.clientX - this.book.getBoundingClientRect().left;
      const pad = parseFloat(this.article.style.getPropertyValue("--page-pad")) || 40;
      const band = Math.max(36, pad + 10);
      return x > this.width - band || x < band;
    };
    this.book.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      if (e.target.closest && e.target.closest("a[data-href]")) return;
      start = { x: e.clientX, y: e.clientY, time: performance.now(), grab: grabZone(e) };
      // Holding the page, not selecting its words.
      if (start.grab && e.pointerType !== "touch") e.preventDefault();
    });
    this.book.addEventListener("pointermove", (e) => {
      if (!start) {
        if (e.pointerType === "mouse") this.book.classList.toggle("grab-edge", !this.busy && grabZone(e));
        return;
      }
      const dx = e.clientX - start.x;
      const rect = this.book.getBoundingClientRect();
      if (!drag) {
        if (this.busy || !start.grab || Math.abs(dx) < 6 || Math.abs(dx) < Math.abs(e.clientY - start.y)) return;
        const dir = dx < 0 ? 1 : -1;
        if ((dir > 0 && this.page + 1 >= this.pages) || (dir < 0 && this.page === 0)) return;
        window.getSelection().removeAllRanges();
        drag = this.begin(dir);
        const y = Math.max(0, Math.min(drag.H, start.y - rect.top));
        drag.C = { x: drag.C0x, y };
        drag.G = { x: start.x - rect.left, y };
        const [rest] = this.ends(drag, y);
        drag.from = rest;
        this.book.classList.add("grab-edge");
        try { this.book.setPointerCapture(e.pointerId); } catch (_) {}
      }
      // The page's edge follows your finger, held at the height you took hold of it.
      const P = { x: drag.from.x + dx, y: drag.from.y + (e.clientY - start.y) };
      drag.at = P;
      this.pose(drag, P);
    });
    const release = () => {
      if (drag) {
        const fast = performance.now() - start.time < 300;
        const p = drag.progress || 0;
        const moved = drag.reverse ? 1 - p : p;
        this.finish(drag, drag.at || drag.from, moved > 0.3 || (fast && moved > 0.04));
        // The click that follows a drag isn't a tap.
        this.swallowClick = true;
        setTimeout(() => (this.swallowClick = false), 50);
      }
      start = null;
      drag = null;
      this.book.classList.remove("grab-edge");
    };
    // A sideways two-finger swipe on a trackpad turns the page too.
    let swipe = 0, swiped = false, swipeTimer = null;
    this.book.addEventListener("wheel", (e) => {
      if (Math.abs(e.deltaX) <= Math.abs(e.deltaY)) return;
      e.preventDefault();
      swipe += e.deltaX;
      clearTimeout(swipeTimer);
      // One turn per swipe: it ends when the trackpad goes quiet.
      swipeTimer = setTimeout(() => { swipe = 0; swiped = false; }, 250);
      if (!swiped && Math.abs(swipe) > 60) {
        swiped = true;
        swipe > 0 ? this.next() : this.prev();
      }
    }, { passive: false });
    this.book.addEventListener("pointerleave", () => { if (!drag) this.book.classList.remove("grab-edge"); });
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

// The recording's waveform as rounded bars: played part in the text colour, the rest faint.
// Redrawn as it plays; sized to the canvas's box at the screen's pixel density.
export function drawWave(canvas, peaks, fraction) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth, h = canvas.clientHeight;
  if (!w || !h) return;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  const cs = getComputedStyle(canvas);
  const played = cs.color;
  const rest = cs.getPropertyValue("--wave-rest").trim() || "rgba(128,128,128,0.35)";
  const bar = 2, gap = 1.5, step = bar + gap;
  const n = Math.max(1, Math.floor(w / step));
  const at = fraction * w;
  for (let i = 0; i < n; i++) {
    let v = 0.06;
    if (peaks.length) {
      // The loudest moment in this bar's stretch of the recording.
      const a = Math.floor((i / n) * peaks.length), b = Math.max(a + 1, Math.floor(((i + 1) / n) * peaks.length));
      v = 0;
      for (let k = a; k < b && k < peaks.length; k++) v = Math.max(v, peaks[k]);
      v = Math.max(0.06, v);
    }
    const bh = Math.max(2, v * h);
    const x = i * step;
    ctx.fillStyle = x + bar <= at ? played : rest;
    ctx.beginPath();
    if (ctx.roundRect) ctx.roundRect(x, (h - bh) / 2, bar, bh, 1);
    else ctx.rect(x, (h - bh) / 2, bar, bh);
    ctx.fill();
  }
}
