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
// Strips the rolling part of a turning page is drawn with.
const STRIPS = 16;

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
    this.book.querySelectorAll(".curl-layer, .curl-flap-wrap").forEach((e) => e.remove());
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
  // The page is modelled as a sheet folded flat along a line: the corner being pulled is at P,
  // the fold is the perpendicular bisector of the corner's resting place C and P, the part past
  // the fold is mirrored over it to show the sheet's back, and shading along the fold gives it
  // the roundness of paper curling over.
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
    t.front = this.layer("curl-front", front);
    t.frontShade = document.createElement("div");
    t.frontShade.className = "curl-shade";
    t.front.appendChild(t.frontShade);
    // The back of the sheet: the next page for a spread; for a single page, this page seen
    // through the paper. It's laid out mirrored about the spine so turning mirrors it back.
    const backFace = () => {
      const inner = document.createElement("div");
      inner.className = ghost ? "curl-back ghosted" : "curl-back";
      inner.style.transform = ghost ? "none" : `translateX(${2 * t.hinge}px) scaleX(-1)`;
      inner.appendChild(back.cloneNode(true));
      return inner;
    };
    // Where the sheet lifts it rolls over a cylinder: drawn as thin strips, each a copy of the
    // page squeezed and shaded as that band of paper faces away from you and then back.
    t.flapWrap = document.createElement("div");
    t.flapWrap.className = "curl-flap-wrap";
    t.strips = [];
    for (let k = 0; k < STRIPS; k++) {
      const strip = this.layer("curl-strip");
      strip.appendChild(k < STRIPS / 2 ? front.cloneNode(true) : backFace());
      const shade = document.createElement("div");
      shade.className = "curl-shade";
      strip.appendChild(shade);
      t.strips.push({ el: strip, shade });
    }
    // Past the roll, the rest of the sheet lies flat, back side up.
    t.flap = this.layer("curl-flap");
    t.flapShade = document.createElement("div");
    t.flapShade.className = "curl-shade";
    t.flap.append(backFace(), t.flapShade);
    t.flapWrap.append(...t.strips.map((x) => x.el), t.flap);
    t.layers.push(t.under, t.front, t.flapWrap);
    this.book.append(...t.layers);
    return t;
  }

  halfLayer(x, content) {
    const s = this.layer("curl-static", content);
    s.style.clipPath = `inset(0 ${this.width - x - this.width / 2}px 0 ${x}px)`;
    return s;
  }

  // Where the corner rests (flat) and where it lands when the page has turned all the way.
  ends(t, y) {
    const C = { x: t.C0x, y };
    const F = { x: 2 * t.hinge - t.C0x, y };
    return t.reverse ? [F, C] : [C, F];
  }

  // Draws the sheet with its pulled corner at P (its resting place is t.C).
  //
  // The sheet stays flat up to a line, then rolls over a cylinder of radius R and lies flat
  // again, back up, beyond it. A point s past the line lands at R·sin(s/R) on the roll, or at
  // πR − s once it's off it, so the corner (at s = (d + πR) / 2) lands exactly at P.
  pose(t, P) {
    const { W, H } = t;
    const C = { x: t.C0x, y: t.C.y };
    // Paper doesn't stretch: the corner stays within a page's width of the spine.
    const hp = { x: t.hinge, y: C.y };
    const w = t.leaf[1] - t.leaf[0];
    let vx = P.x - hp.x, vy = P.y - hp.y;
    const r = Math.hypot(vx, vy);
    if (r > w) { vx *= w / r; vy *= w / r; }
    P = { x: hp.x + vx, y: hp.y + vy };
    const dx = P.x - C.x, dy = P.y - C.y;
    const d = Math.hypot(dx, dy);
    const rect = [[t.leaf[0], 0], [t.leaf[1], 0], [t.leaf[1], H], [t.leaf[0], H]];
    const none = "polygon(0 0, 0 0, 0 0)";
    if (d < 0.5) {
      t.front.style.clipPath = poly(rect);
      t.flapWrap.style.display = "none";
      t.under.style.opacity = "0";
      t.progress = 0;
      return;
    }
    t.flapWrap.style.display = "";
    // n points from the corner's resting place toward where it's been pulled.
    const n = { x: dx / d, y: dy / d };
    // How tightly the paper rolls: it starts as a small curl at the corner, bows into a wide,
    // soft bend mid-turn, and flexes further the faster it's moved, easing back as it slows.
    const now = performance.now();
    if (t.lastP) {
      const dt = Math.max(1, now - t.lastT);
      const speed = Math.hypot(P.x - t.lastP.x, P.y - t.lastP.y) / dt;
      t.flex = (t.flex || 0) * 0.8 + Math.min(w * 0.12, speed * 22) * 0.2;
    }
    t.lastP = P;
    t.lastT = now;
    const turned = Math.min(1, d / (Math.abs(2 * t.hinge - 2 * C.x) || W));
    const R = Math.min(w * 0.4, 6 + d * 0.12 + w * 0.2 * Math.sin(Math.PI * turned) + (t.flex || 0));
    const sC = (d + Math.PI * R) / 2;
    // The line where the sheet starts to lift; s is measured from it, back toward the corner.
    const L0 = { x: C.x + n.x * sC, y: C.y + n.y * sC };
    const sOf = (pt) => -((pt[0] - L0.x) * n.x + (pt[1] - L0.y) * n.y);
    const band = (lo, hi) => {
      let q = rect;
      if (lo !== null) q = clipPoly(q, (pt) => sOf(pt) - lo);
      if (hi !== null) q = clipPoly(q, (pt) => hi - sOf(pt));
      return q.length > 2 ? poly(q) : none;
    };
    // Moves each point (f(s) - s) along the sheet (toward the corner, -n), f(s) = a·s + b.
    const affine = (a, b) => {
      const ux = -n.x, uy = -n.y;
      const k = a - 1;
      const T = b - k * (ux * L0.x + uy * L0.y);
      return `matrix(${1 + k * ux * ux}, ${k * ux * uy}, ${k * ux * uy}, ${1 + k * uy * uy}, ${T * ux}, ${T * uy})`;
    };
    t.front.style.clipPath = band(null, 0);
    const span = Math.abs(2 * t.hinge - 2 * C.x) || W;
    const p = Math.min(1, Math.abs(P.x - C.x) / span);
    t.progress = t.reverse ? 1 - p : p;
    const lift = Math.sin(Math.PI * Math.min(1, d / span));
    // The roll.
    const roll = Math.PI * R;
    t.strips.forEach(({ el, shade }, k) => {
      const s0 = (roll * k) / STRIPS, s1 = (roll * (k + 1)) / STRIPS;
      const f0 = R * Math.sin(s0 / R), f1 = R * Math.sin(s1 / R);
      const a = (f1 - f0) / (s1 - s0);
      el.style.clipPath = band(s0 - 0.4, s1 + 0.4);
      el.style.transform = affine(a, f0 - a * s0);
      // Light from the viewer: the paper darkens as it turns edge-on, the back brightens as it
      // comes round to face you, with a gloss where the curve catches the light.
      const th = (Math.PI * (k + 0.5)) / STRIPS;
      const dark = k < STRIPS / 2 ? 0.34 * (1 - Math.cos(th)) : 0.3 * (1 + Math.cos(th));
      const gloss = Math.exp(-(((th - 0.78 * Math.PI) / (0.11 * Math.PI)) ** 2));
      shade.style.background =
        `linear-gradient(rgba(255,255,255,${(0.3 * gloss).toFixed(3)}), rgba(255,255,255,${(0.3 * gloss).toFixed(3)})), ` +
        `rgba(0,0,0,${dark.toFixed(3)})`;
    });
    // Off the roll: flat, back up, folded over the line at s = πR/2.
    t.flap.style.clipPath = band(roll, null);
    t.flap.style.transform = affine(-1, roll);
    // A point s along the sheet from the lift line.
    const at = (s) => ({ x: L0.x - n.x * s, y: L0.y - n.y * s });
    // The flat back darkens a little toward the roll it's coming off.
    t.flapShade.style.background = gradient(W, H, { x: -n.x, y: -n.y }, at(roll), [[0, "rgba(0,0,0,0.16)"], [Math.min(40, 6 + d * 0.1), "rgba(0,0,0,0)"]]);
    // Shadow of the lifted sheet on the page it uncovers, deepest right under the roll.
    const reach = Math.min(170, 30 + d * 0.5);
    t.under.style.clipPath = band(R, null);
    t.under.style.opacity = "1";
    t.under.style.background = gradient(W, H, { x: -n.x, y: -n.y }, at(R), [
      [0, `rgba(0,0,0,${(0.45 * lift + 0.06).toFixed(3)})`],
      [reach * 0.35, `rgba(0,0,0,${(0.12 * lift).toFixed(3)})`],
      [reach, "rgba(0,0,0,0)"],
    ]);
    // The flat page darkens slightly just before it starts to lift.
    t.frontShade.style.background = gradient(W, H, { x: n.x, y: n.y }, L0, [[0, `rgba(0,0,0,${(0.1 * lift).toFixed(3)})`], [Math.min(40, 6 + d * 0.1), "rgba(0,0,0,0)"]]);
    t.flapWrap.style.filter = `drop-shadow(${(n.x * 3 * lift).toFixed(1)}px ${(n.y * 3 * lift).toFixed(1)}px ${(4 + 10 * lift).toFixed(1)}px rgba(0,0,0,${(0.2 + 0.16 * lift).toFixed(2)}))`;
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
      const x = (e.clientX - this.book.getBoundingClientRect().left) / this.width;
      const band = this.spread ? 0.16 : 0.3;
      return x > 1 - band || x < band;
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
        if (e.pointerType === "mouse") this.book.style.cursor = !this.busy && grabZone(e) ? "grab" : "";
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
        this.book.style.cursor = "grabbing";
        try { this.book.setPointerCapture(e.pointerId); } catch (_) {}
      }
      // The paper under your finger stays under it: the fold is halfway between where you took
      // hold and where your finger is now, and the corner is wherever that fold carries it.
      const F = { x: e.clientX - rect.left, y: Math.max(-drag.H * 0.2, Math.min(drag.H * 1.2, e.clientY - rect.top)) };
      const G = drag.reverse ? { x: 2 * drag.hinge - drag.G.x, y: drag.G.y } : drag.G;
      const vx = F.x - G.x, vy = F.y - G.y;
      const d = Math.hypot(vx, vy);
      let P = drag.from;
      if (drag.reverse) {
        // Laying the previous page back down: the corner comes in with your finger.
        P = { x: drag.from.x + (F.x - drag.G.x) * 2, y: drag.from.y + (F.y - drag.G.y) };
      } else if (d > 0.5) {
        const n = { x: vx / d, y: vy / d };
        const M = { x: (G.x + F.x) / 2, y: (G.y + F.y) / 2 };
        const C = drag.C;
        const k = 2 * ((C.x - M.x) * n.x + (C.y - M.y) * n.y);
        P = { x: C.x - k * n.x, y: C.y - k * n.y };
      }
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
      this.book.style.cursor = "";
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

// Sutherland–Hodgman against one half-plane: keeps the points where f(pt) >= 0.
function clipPoly(pts, f) {
  const out = [];
  for (let i = 0; i < pts.length; i++) {
    const a = pts[i], b = pts[(i + 1) % pts.length];
    const fa = f(a), fb = f(b);
    if (fa >= 0) out.push(a);
    if ((fa >= 0) !== (fb >= 0)) {
      const k = fa / (fa - fb);
      out.push([a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k]);
    }
  }
  return out;
}

function poly(pts) {
  return `polygon(${pts.map((p) => `${p[0].toFixed(1)}px ${p[1].toFixed(1)}px`).join(", ")})`;
}

// A linear gradient across a W×H box running along n, with stops given as signed distances
// from the line through M perpendicular to n.
function gradient(W, H, n, M, stops) {
  const angle = (Math.atan2(n.x, -n.y) * 180) / Math.PI;
  const len = Math.abs(W * n.x) + Math.abs(H * n.y);
  const at0 = (M.x - W / 2) * n.x + (M.y - H / 2) * n.y + len / 2;
  const s = stops.slice().sort((a, b) => a[0] - b[0]).map(([d, c]) => `${c} ${(at0 + d).toFixed(1)}px`);
  return `linear-gradient(${angle.toFixed(2)}deg, ${s.join(", ")})`;
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
