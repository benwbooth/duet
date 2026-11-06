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
// Cross-fade between the live page and its picture at each end of a turn.
const FADE_MS = 90;
// For checking page turns frame by frame: window.__duetSlow = 10 plays them ten times slower.
const slow = () => window.__duetSlow || 1;
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

// ---------------------------------------------------------------------------------------------
// Page curl on the graphics card, the way iBooks-style readers do it: each page is a picture
// (a snapshot of the laid-out text) on a fine mesh, and the vertex shader rolls the mesh around a
// cylinder whose axis follows the finger. Past the lift line the paper wraps over the cylinder,
// and beyond it lies flat again, back side up. Everything stays one continuous sheet, so it can
// peel diagonally from any corner, and the bound edge stays put.

const CURL_VS = `
attribute vec2 a_uv;
uniform vec4 u_leaf;   // x0, width, height, hinge x
uniform vec2 u_view;   // book width, height
uniform vec2 u_L0;     // a point on the lift line
uniform vec2 u_dir;    // across the lift line, toward the corner being pulled
uniform float u_R;     // cylinder radius
uniform float u_squash;
varying vec2 v_pos;
varying float v_theta;
void main() {
  vec2 X = vec2(u_leaf.x + a_uv.x * u_leaf.y, a_uv.y * u_leaf.z);
  float s = dot(X - u_L0, u_dir);
  vec3 P = vec3(X, 0.0);
  float th = 0.0;
  if (s > 0.0) {
    float roll = 3.14159265 * u_R;
    vec2 base = X - u_dir * s;
    if (s < roll) {
      th = s / u_R;
      P = vec3(base + u_dir * u_R * sin(th), u_R * (1.0 - cos(th)));
    } else {
      th = 3.14159265;
      P = vec3(base + u_dir * (roll - s), 2.0 * u_R);
    }
    P.x = u_leaf.w + (P.x - u_leaf.w) * u_squash;
  }
  v_pos = X;
  v_theta = th;
  float D = 2600.0;
  vec2 c = u_view * 0.5;
  vec2 scr = c + (P.xy - c) * D / (D - P.z);
  gl_Position = vec4(scr.x / u_view.x * 2.0 - 1.0, 1.0 - scr.y / u_view.y * 2.0, -P.z / (8.0 * D), 1.0);
}`;

const CURL_FS = `
precision highp float;
uniform sampler2D u_front;
uniform sampler2D u_back;
uniform float u_ghost;
uniform float u_flip;
uniform vec2 u_view;
uniform float u_hinge;
uniform vec3 u_paper;
uniform vec4 u_edge;   // leaf x0, width, height, hinge x (for the outline)
varying vec2 v_pos;
varying float v_theta;
// A fine light-grey line round the sheet's free edges (not the bound one), so the turning page
// reads clearly against the page beneath it.
vec3 outline(vec3 col) {
  float l = v_pos.x - u_edge.x, r = u_edge.x + u_edge.y - v_pos.x;
  float side = abs(u_edge.w - u_edge.x) < 1.0 ? r : l;
  float d = min(side, min(v_pos.y, u_edge.z - v_pos.y));
  float a = 1.0 - smoothstep(0.6, 1.8, d);
  return mix(col, vec3(0.72), a * 0.9);
}
void main() {
  vec2 uv = v_pos / u_view;
  float c = cos(v_theta);
  bool front = gl_FrontFacing == (u_flip < 0.5);
  if (front) {
    vec3 col = texture2D(u_front, uv).rgb;
    float shade = 0.42 * pow(1.0 - max(c, 0.0), 1.5);
    gl_FragColor = vec4(outline(col * (1.0 - shade)), 1.0);
  } else {
    vec3 col;
    if (u_ghost > 0.5) {
      // A single page's back: the page itself, faintly, through the paper.
      col = mix(u_paper, texture2D(u_front, uv).rgb, 0.08);
    } else {
      col = texture2D(u_back, vec2((2.0 * u_hinge - v_pos.x) / u_view.x, uv.y)).rgb;
    }
    float dark = 0.4 * pow(max(0.0, 1.0 + c) / 2.0, 1.5);
    float gloss = 0.3 * exp(-pow((v_theta - 2.2) / 0.41, 2.0));
    gl_FragColor = vec4(outline(col * (1.0 - dark) + gloss), 1.0);
  }
}`;

let sharedCurl = null;

// Takes whatever the curl canvas is showing off the screen.
function uncover() {
  if (!sharedCurl || sharedCurl.lost) return;
  sharedCurl.clear();
  sharedCurl.canvas.style.opacity = "0";
}

// Flat parts of the book drawn with the curl, so the whole book comes from one picture at a time:
// the page underneath (with the turning page's shadow on it) and the facing page that stays put.
const FLAT_VS = `
attribute vec2 a_uv;
uniform vec4 u_rect;   // x0, width, height
uniform vec2 u_view;
varying vec2 v_pos;
void main() {
  vec2 X = vec2(u_rect.x + a_uv.x * u_rect.y, a_uv.y * u_rect.z);
  v_pos = X;
  gl_Position = vec4(X.x / u_view.x * 2.0 - 1.0, 1.0 - X.y / u_view.y * 2.0, 0.0, 1.0);
}`;

const FLAT_FS = `
precision highp float;
uniform sampler2D u_tex;
uniform vec2 u_view;
uniform vec4 u_sh;     // shadow: edge point, direction away from the turning page
uniform vec3 u_shp;    // reach, strength, on
varying vec2 v_pos;
void main() {
  vec3 col = texture2D(u_tex, v_pos / u_view).rgb;
  if (u_shp.z > 0.5) {
    float d = dot(v_pos - u_sh.xy, u_sh.zw);
    float r = u_shp.x;
    float a = d < 0.0 ? 0.0 : d < 0.35 * r ? mix(u_shp.y, u_shp.y * 0.3, d / (0.35 * r)) : mix(u_shp.y * 0.3, 0.0, clamp((d - 0.35 * r) / (0.65 * r), 0.0, 1.0));
    col *= 1.0 - a;
  }
  gl_FragColor = vec4(col, 1.0);
}`;

class CurlGL {
  constructor(book) {
    this.canvas = document.createElement("canvas");
    this.canvas.className = "page-gl";
    // The GPU can drop the context (driver reset, too many tabs); make a new one next turn.
    this.canvas.addEventListener("webglcontextlost", (e) => { e.preventDefault(); this.lost = true; });
    const gl = this.canvas.getContext("webgl", { antialias: true, alpha: true, premultipliedAlpha: true });
    if (!gl) throw new Error("no WebGL");
    this.gl = gl;
    const sh = (type, src) => {
      const s = gl.createShader(type);
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
      return s;
    };
    const prog = gl.createProgram();
    gl.attachShader(prog, sh(gl.VERTEX_SHADER, CURL_VS));
    gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, CURL_FS));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
    this.prog = prog;
    const flat = gl.createProgram();
    gl.attachShader(flat, sh(gl.VERTEX_SHADER, FLAT_VS));
    gl.attachShader(flat, sh(gl.FRAGMENT_SHADER, FLAT_FS));
    gl.linkProgram(flat);
    if (!gl.getProgramParameter(flat, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(flat));
    this.flat = flat;
    this.f = {};
    for (const n of ["u_rect", "u_view", "u_tex", "u_sh", "u_shp"]) this.f[n] = gl.getUniformLocation(flat, n);
    this.flatLoc = gl.getAttribLocation(flat, "a_uv");
    gl.useProgram(prog);
    // A fine grid over the sheet: fine enough that the roll looks round from any angle.
    const NX = 72, NY = 48, verts = [], idx = [];
    for (let j = 0; j <= NY; j++) for (let i = 0; i <= NX; i++) verts.push(i / NX, j / NY);
    for (let j = 0; j < NY; j++)
      for (let i = 0; i < NX; i++) {
        const a = j * (NX + 1) + i, b = a + 1, c = a + NX + 1, d = c + 1;
        idx.push(a, c, b, b, c, d);
      }
    this.count = idx.length;
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(verts), gl.STATIC_DRAW);
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, new Uint16Array(idx), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(prog, "a_uv");
    this.curlLoc = loc;
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    this.u = {};
    for (const n of ["u_leaf", "u_view", "u_L0", "u_dir", "u_R", "u_squash", "u_front", "u_back", "u_ghost", "u_flip", "u_hinge", "u_paper", "u_edge"])
      this.u[n] = gl.getUniformLocation(prog, n);
    gl.uniform1i(this.u.u_front, 0);
    gl.uniform1i(this.u.u_back, 1);
    gl.uniform1f(this.u.u_flip, 0);
    this.tex = [gl.createTexture(), gl.createTexture(), gl.createTexture()];
    gl.enable(gl.DEPTH_TEST);
    gl.depthFunc(gl.LEQUAL);
    book.appendChild(this.canvas);
  }

  // Uploads a page picture (throws if the browser won't allow it, e.g. a tainted canvas).
  texture(unit, source) {
    const gl = this.gl;
    gl.activeTexture(gl.TEXTURE0 + unit);
    gl.bindTexture(gl.TEXTURE_2D, this.tex[unit]);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, source);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  size(W, H) {
    const dpr = window.devicePixelRatio || 1;
    this.canvas.style.width = `${W}px`;
    this.canvas.style.height = `${H}px`;
    if (this.canvas.width !== Math.round(W * dpr) || this.canvas.height !== Math.round(H * dpr)) {
      this.canvas.width = Math.round(W * dpr);
      this.canvas.height = Math.round(H * dpr);
    }
    this.gl.viewport(0, 0, this.canvas.width, this.canvas.height);
  }

  // Nothing on the canvas (between turns).
  clear() {
    const gl = this.gl;
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
  }

  // A flat rectangle of the book from texture `unit`, optionally with the turning page's shadow.
  flatRect(x0, w, H, W, unit, shadow) {
    const gl = this.gl, f = this.f;
    gl.useProgram(this.flat);
    gl.enableVertexAttribArray(this.flatLoc);
    gl.vertexAttribPointer(this.flatLoc, 2, gl.FLOAT, false, 0, 0);
    gl.uniform4f(f.u_rect, x0, w, H, 0);
    gl.uniform2f(f.u_view, W, H);
    gl.uniform1i(f.u_tex, unit);
    if (shadow) {
      gl.uniform4f(f.u_sh, shadow.edge.x, shadow.edge.y, shadow.dir.x, shadow.dir.y);
      gl.uniform3f(f.u_shp, shadow.reach, shadow.strength, 1);
    } else {
      gl.uniform3f(f.u_shp, 1, 0, 0);
    }
    gl.drawElements(gl.TRIANGLES, this.count, gl.UNSIGNED_SHORT, 0);
  }

  // Just a picture of the book, flat (keeping the old page on screen while the next chapter loads).
  cover(picture, W, H) {
    this.texture(0, picture);
    this.size(W, H);
    this.clear();
    this.flatRect(0, W, H, W, 0, null);
    this.canvas.style.visibility = "visible";
    this.canvas.style.opacity = "1";
  }

  draw(p) {
    const gl = this.gl, u = this.u;
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
    // The page uncovered beneath (texture 2) and the facing page that doesn't move (texture 0).
    if (p.under) this.flatRect(p.x0, p.w, p.H, p.W, 2, p.shadow);
    if (p.still) this.flatRect(p.still[0], p.still[1] - p.still[0], p.H, p.W, 0, null);
    gl.useProgram(this.prog);
    gl.enableVertexAttribArray(this.curlLoc);
    gl.vertexAttribPointer(this.curlLoc, 2, gl.FLOAT, false, 0, 0);
    gl.uniform4f(u.u_leaf, p.x0, p.w, p.H, p.hinge);
    gl.uniform2f(u.u_view, p.W, p.H);
    gl.uniform2f(u.u_L0, p.L0.x, p.L0.y);
    gl.uniform2f(u.u_dir, p.dir.x, p.dir.y);
    gl.uniform1f(u.u_R, p.R);
    gl.uniform1f(u.u_squash, p.squash);
    gl.uniform1f(u.u_ghost, p.ghost ? 1 : 0);
    gl.uniform1f(u.u_hinge, p.hinge);
    gl.uniform3f(u.u_paper, p.paper[0], p.paper[1], p.paper[2]);
    gl.uniform4f(u.u_edge, p.x0, p.w, p.H, p.hinge);
    gl.drawElements(gl.TRIANGLES, this.count, gl.UNSIGNED_SHORT, 0);
  }
}

let pageCssText = null;
const dataUrls = new Map();

async function asDataUrl(src) {
  if (!dataUrls.has(src)) {
    dataUrls.set(src, fetch(src)
      .then((r) => r.blob())
      .then((b) => new Promise((res) => { const f = new FileReader(); f.onload = () => res(f.result); f.readAsDataURL(b); }))
      .catch(() => ""));
  }
  return dataUrls.get(src);
}

const xmlText = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const xmlAttr = (s) => xmlText(s || "").replace(/"/g, "&quot;");

// A picture of the book as it looks with `article` scrolled to `offset`: the page drawn through
// an SVG foreignObject with the reader's own styles, so it matches the real page.
async function snapshotPage(pager, article, offset) {
  if (document.fonts) await document.fonts.ready;
  const W = pager.width, H = pager.book.clientHeight;
  const reader = pager.book.closest(".reader") || pager.book.parentElement;
  if (pageCssText === null) {
    pageCssText = await fetch("styles.css").then((r) => r.text()).catch(() => "");
  }
  const clone = article.cloneNode(true);
  clone.classList.remove("synced");
  clone.querySelectorAll(".s.now").forEach((e) => e.classList.remove("now"));
  clone.style.width = `${W}px`;
  // Pictures in the book, and blocks styled by EPUB markup (epub:type, which doesn't survive into
  // the picture's XML as something CSS can match), keep the size and layout they have live.
  const liveEls = article.querySelectorAll("img, [epub\\:type]");
  const copyEls = clone.querySelectorAll("img, [epub\\:type]");
  liveEls.forEach((el, i) => {
    const c = copyEls[i];
    if (!c) return;
    if (el.tagName === "IMG") {
      const r = el.getBoundingClientRect();
      if (r.width && r.height) {
        c.style.width = `${r.width}px`;
        c.style.height = `${r.height}px`;
        c.style.maxWidth = "none";
        c.style.maxHeight = "none";
      }
    } else {
      const cs = getComputedStyle(el);
      for (const k of ["display", "flex-direction", "justify-content", "align-items",
        "margin-top", "margin-bottom", "padding-top", "padding-bottom", "text-align", "break-before", "break-after"]) {
        c.style.setProperty(k, cs.getPropertyValue(k));
      }
      // A fixed height only for page-sized blocks (a full-page illustration): a whole chapter
      // section's height would be wrong for the cut-down copy.
      if (el.getBoundingClientRect().height <= H + 1 && /\d/.test(el.style.height + cs.height) && (el.matches("figure, figure *") || cs.display === "flex")) {
        c.style.height = cs.height;
      }
    }
  });
  // Paragraph indents follow from what comes before them (first child, after a heading...), which
  // cutting the picture down to one page changes: each keeps its live indent.
  const liveP = article.querySelectorAll("p, li, dd, blockquote");
  const copyP = clone.querySelectorAll("p, li, dd, blockquote");
  liveP.forEach((el, i) => {
    if (copyP[i]) copyP[i].style.textIndent = getComputedStyle(el).textIndent;
  });
  // The picture starts at the very word the live page starts with: laid out from the chapter's
  // beginning, small line-break differences would add up over the pages and the picture would
  // show a few lines more or less than the page.
  // ...and stops a page later (only what can show; much quicker to draw).
  const end = pageStart(article, offset + W);
  if (end) {
    const node = end.path.reduce((n, i) => (n ? n.childNodes[i] : null), clone);
    if (node) {
      const tail = document.createRange();
      tail.setStart(node, end.index);
      tail.setEndAfter(clone.lastChild);
      tail.deleteContents();
    }
  }
  const start = offset > 0 ? pageStart(article, offset) : null;
  if (start) {
    const node = start.path.reduce((n, i) => (n ? n.childNodes[i] : null), clone);
    if (node) {
      // (Found before cutting: the cut renumbers the children before it.)
      const firstEl = node.nodeType === 1 && node.childNodes[start.index] && node.childNodes[start.index].nodeType === 1
        ? node.childNodes[start.index] : null;
      const cut = document.createRange();
      cut.setStart(clone, 0);
      cut.setEnd(node, start.index);
      cut.deleteContents();
      // The block the page begins in keeps the indent it has on the live page: none when the
      // page begins partway through a paragraph, and otherwise exactly the live one (with what
      // came before cut away it would count as a first paragraph, which isn't indented).
      let block = node.nodeType === 1 ? node : node.parentElement;
      while (block && block !== clone && !BLOCK.test(block.tagName)) block = block.parentElement;
      if (block && block !== clone) block.style.textIndent = start.continues ? "0" : start.indent;
      // On the live page this content follows a column break, where top margins are dropped;
      // as the first thing in the picture it would keep them and sit lower.
      for (let el = node.nodeType === 1 ? node : node.parentElement; el && el !== clone; el = el.parentElement) {
        el.style.marginTop = "0";
      }
      // (A page that starts with a picture: the picture's own top margin goes too.)
      if (firstEl) firstEl.style.marginTop = "0";
    }
    clone.style.transform = "none";
  } else {
    clone.style.transform = `translateX(${-offset}px)`;
  }
  // EPUB markup carries prefixed attributes (epub:type...) that an XML document must declare;
  // epub: is declared below, anything else is dropped.
  for (const el of clone.querySelectorAll("*")) {
    for (const a of [...el.attributes]) {
      if (a.name.includes(":") && !a.name.startsWith("epub:") && !a.name.startsWith("xml:")) el.removeAttribute(a.name);
    }
  }
  for (const img of clone.querySelectorAll("img")) {
    const src = img.getAttribute("src");
    img.removeAttribute("srcset");
    img.removeAttribute("loading");
    if (src && !src.startsWith("data:")) img.setAttribute("src", await asDataUrl(new URL(src, location.href).href));
  }
  const bookCls = pager.book.className.replace(/\bgrab-edge\b/g, "");
  // The running head and page number belong in the picture too (they're beside the text, not
  // in it), with this page's number.
  let furniture = "";
  for (const sel of [":scope > .running-head", ":scope > .folio"]) {
    const el = pager.book.querySelector(sel);
    if (!el) continue;
    const c = el.cloneNode(true);
    const pageEl = c.querySelector(".folio-page");
    if (pageEl) {
      const n = pager.pages, p = Math.round(offset / W) + 1, left = n - p;
      pageEl.textContent = `Page ${p} of ${n} · ${left === 0 ? "last page in chapter" : left === 1 ? "1 page left in chapter" : `${left} pages left in chapter`}`;
    }
    furniture += new XMLSerializer().serializeToString(c);
  }
  // The page must break lines exactly as the live one does: same language (hyphenation), and
  // the body's type settings the styles inherit from (kerning, ligatures, spacing).
  const body = getComputedStyle(document.body);
  const inherit = ["font-family", "font-size", "line-height", "letter-spacing", "text-rendering", "font-kerning",
    "font-feature-settings", "font-variant-ligatures", "-webkit-font-smoothing"]
    .map((k) => `${k}:${body.getPropertyValue(k)}`).join(";");
  const lang = document.documentElement.lang || "en";
  const html =
    `<div xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="${xmlAttr(lang)}" xml:lang="${xmlAttr(lang)}" ` +
    `class="${xmlAttr(reader.className)}" data-theme="${xmlAttr(reader.dataset.theme || "")}" ` +
    `style="${xmlAttr(inherit)};${xmlAttr(reader.getAttribute("style") || "")};position:relative;inset:auto;width:${W}px;height:${H}px;margin:0;padding:0;overflow:hidden;min-height:0">` +
    `<style>${xmlText(pageCssText)}</style>` +
    `<div class="${xmlAttr(bookCls)}" style="position:relative;top:0;left:0;right:auto;bottom:auto;width:${W}px;height:${H}px;margin:0;max-width:none;box-shadow:none;border-radius:0;overflow:hidden">` +
    new XMLSerializer().serializeToString(clone) + furniture +
    `</div></div>`;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}"><foreignObject x="0" y="0" width="${W}" height="${H}">${html}</foreignObject></svg>`;
  if (window.__duetSlow) (window.__duetPictures ||= {})[offset] = svg; // for checking pictures
  const img = new Image();
  img.src = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svg);
  await img.decode();
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  const canvas = document.createElement("canvas");
  canvas.width = Math.round(W * dpr);
  canvas.height = Math.round(H * dpr);
  const ctx = canvas.getContext("2d");
  ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
  ctx.getImageData(0, 0, 1, 1); // throws now, rather than mid-turn, if the browser taints it
  return canvas;
}

// Where the page at `offset` (px into the laid-out chapter) begins: the text node (as a path of
// child indices from the article) and the character in it. Columns run left to right, so it's
// the first character whose box lies at or past the page's left edge.
const BLOCK = /^(P|LI|BLOCKQUOTE|DIV|H\d|FIGCAPTION|DD|DT|SECTION|ARTICLE)$/;

// Whether there's text in `node`'s block before character `index` (the page starts
// mid-paragraph), and the block's first-line indent on the live page.
function blockAt(article, node, index) {
  let block = node.nodeType === 1 ? node : node.parentElement;
  while (block && block !== article && !BLOCK.test(block.tagName)) block = block.parentElement;
  if (!block || block === article) return { continues: false, indent: "0" };
  const before = document.createRange();
  before.setStart(block, 0);
  before.setEnd(node, index);
  return { continues: before.toString().trim().length > 0, indent: getComputedStyle(block).textIndent };
}

function pageStart(article, offset) {
  const left = article.getBoundingClientRect().left;
  // Text, and pictures (a page can begin with one).
  const walker = document.createTreeWalker(article, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT, {
    acceptNode: (n) => (n.nodeType === 3 || /^(IMG|SVG|HR)$/i.test(n.nodeName) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP),
  });
  const range = document.createRange();
  const x = (node, i) => {
    range.setStart(node, i);
    range.setEnd(node, i + 1);
    const r = range.getClientRects()[0] || range.getBoundingClientRect();
    return r ? r.left - left : -Infinity;
  };
  const pathTo = (n) => {
    const path = [];
    for (; n !== article; n = n.parentNode) path.unshift([...n.parentNode.childNodes].indexOf(n));
    return path;
  };
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    if (node.nodeType === 1) {
      const r = node.getBoundingClientRect();
      if (r.width && r.left - left >= offset - 1) {
        const path = pathTo(node);
        return { path: path.slice(0, -1), index: path[path.length - 1], continues: false, indent: "0" };
      }
      continue;
    }
    const len = node.length;
    if (!len || !node.data.trim()) continue;
    if (x(node, len - 1) < offset - 1) continue;
    // Binary search for the first character on this page.
    let lo = 0, hi = len - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (x(node, mid) >= offset - 1) hi = mid;
      else lo = mid + 1;
    }
    return { path: pathTo(node), index: lo, ...blockAt(article, node, lo) };
  }
  return null;
}

// A linear gradient across a W×H box running along n, with stops given as signed distances
// from the line through M perpendicular to n.
function lineGradient(W, H, n, M, stops) {
  const angle = (Math.atan2(n.x, -n.y) * 180) / Math.PI;
  const len = Math.abs(W * n.x) + Math.abs(H * n.y);
  const at0 = (M.x - W / 2) * n.x + (M.y - H / 2) * n.y + len / 2;
  const s = stops.map(([d, c]) => `${c} ${(at0 + d).toFixed(1)}px`);
  return `linear-gradient(${angle.toFixed(2)}deg, ${s.join(", ")})`;
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
    // Pictures of nearby pages for the curl, by offset; made in idle time after each turn.
    this.snaps = new Map();
    this.layout();
    this.resize = new ResizeObserver(() => (this.book.isConnected ? this.relayout() : this.destroy()));
    this.resize.observe(book.parentElement || book);
    this.onKey = (e) => {
      // A pager whose book has left the page (the reader re-rendered it) must not keep turning.
      if (!this.book.isConnected) return this.destroy();
      if (e.target.closest && e.target.closest("input, textarea, select")) return;
      if (e.altKey || e.ctrlKey || e.metaKey) return;
      if (["ArrowRight", "ArrowDown", "PageDown", " "].includes(e.key)) { e.preventDefault(); this.next(); }
      if (["ArrowLeft", "ArrowUp", "PageUp"].includes(e.key)) { e.preventDefault(); this.prev(); }
    };
    document.addEventListener("keydown", this.onKey);
    this.bindPointer();
    // Pictures and fonts arriving after the first layout add pages: count them again then.
    for (const img of article.querySelectorAll("img")) {
      if (!img.complete) img.addEventListener("load", () => this.relayoutSoon(), { once: true });
    }
    if (document.fonts) document.fonts.ready.then(() => this.relayoutSoon());
    // Text size, typeface or theme changing reflows the pages: count them again.
    const reader = book.closest(".reader");
    if (reader) {
      this.styleWatch = new MutationObserver(() => this.relayoutSoon());
      this.styleWatch.observe(reader, { attributes: true, attributeFilter: ["style", "data-theme"] });
    }
    // Arriving from the neighbouring chapter: finish the page turn that started there (after the
    // reader has gone to this chapter's first or last page).
    // (The reader may lay the chapter out twice; whichever pager is still alive plays it.)
    const carry = book.__carry;
    if (carry) {
      setTimeout(() => {
        if (this.dead || book.__carry !== carry) return;
        delete book.__carry;
        if (carry.spread === this.spread) this.turn(carry.dir, carry);
        else uncover();
      }, 0);
    }
  }

  destroy() {
    this.resize.disconnect();
    document.removeEventListener("keydown", this.onKey);
    this.book.querySelectorAll(".curl-layer, .page3d, .page-warp").forEach((e) => e.remove());
    // The shared curl canvas stays in the book; it's left showing only while a turn into the
    // next chapter is waiting to play (the old page over the new one).
    if (sharedCurl && !this.book.__carry && sharedCurl.canvas.parentNode === this.book) uncover();
    this.dead = true;
    clearTimeout(this.warmTimer);
    if (this.styleWatch) this.styleWatch.disconnect();
    this.book.classList.remove("spread");
    delete this.book.__pager;
  }

  // Lays out again (resize, one/two pages) keeping the same text on screen.
  relayout() {
    if (this.busy) {
      this.relayoutAfter = true;
      return;
    }
    const anchor = this.firstSentence();
    this.layout();
    if (this.atEnd) return this.go(this.pages - 1, false);
    const e = this.article.querySelector(`.s[data-s="${anchor}"]`);
    if (e) this.showElement(e, false);
  }

  relayoutSoon() {
    clearTimeout(this.relayoutTimer);
    this.relayoutTimer = setTimeout(() => !this.dead && this.relayout(), 120);
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
    // Measured from the first page: shifted to a later one, the book's scroll width only counts
    // what's to the right of it.
    this.article.style.transform = "none";
    this.pages = Math.max(1, Math.round(this.book.scrollWidth / w));
    // Columns can leave a sliver; trust the last element's position too.
    const last = this.article.lastElementChild;
    if (last) this.pages = Math.max(this.pages, this.pageOf(last, true) + 1);
    this.page = Math.min(this.page, this.pages - 1);
    this.place(this.article, this.page);
    this.snaps.clear();
    this.warp();
    this.report();
  }

  // Real pages aren't flat: they curve down into the spine and their corners lift a little. The
  // page's outline is drawn that way over its very edge (the text, well inside the margins, stays
  // flat): what lies outside the curved outline shows as the page beneath.
  warp() {
    let svg = this.book.querySelector(":scope > .page-warp");
    const narrow = this.book.clientWidth < 640 && (this.book.parentElement || this.book).clientWidth < 700;
    if (narrow) {
      if (svg) svg.remove();
      return;
    }
    const W = this.width, H = this.book.clientHeight;
    const spine = this.spread ? W / 2 : 0;
    const pageW = this.spread ? W / 2 : W;
    // How far the edge sits inside the box at x along the top or bottom.
    const dip = (x) => {
      const u = this.spread ? (x < spine ? x / pageW : (x - spine) / pageW) : x / pageW;
      return 6.5 * Math.exp(-Math.abs(x - spine) / 46) + 1.6 * (1 - Math.sin(Math.PI * Math.max(0, Math.min(1, u))));
    };
    const side = (y) => 2.4 * (1 - Math.sin((Math.PI * y) / H));
    const pts = [];
    for (let x = 0; x <= W; x += 6) pts.push([Math.max(x, side(0)), dip(x)]);
    for (let y = 0; y <= H; y += 12) pts.push([W - side(y), dip(W) + (H - 2 * dip(W)) * (y / H)]);
    for (let x = W; x >= 0; x -= 6) pts.push([Math.min(Math.max(x, side(H)), W - side(H)), H - dip(x)]);
    for (let y = H; y >= 0; y -= 12) pts.push([side(y), dip(0) + (H - 2 * dip(0)) * (y / H)]);
    const outline = "M" + pts.map((p) => `${p[0].toFixed(1)},${p[1].toFixed(1)}`).join("L") + "Z";
    if (!svg) {
      svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      svg.setAttribute("class", "page-warp");
      svg.setAttribute("aria-hidden", "true");
      this.book.appendChild(svg);
    }
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    svg.setAttribute("width", W);
    svg.setAttribute("height", H);
    svg.innerHTML =
      `<path class="warp-under" fill-rule="evenodd" d="M0,0H${W}V${H}H0Z${outline}"/>` +
      `<path class="warp-edge" fill="none" d="${outline}"/>`;
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
    this.edges();
    this.warm();
  }

  // The book's edges: page stacks either side, as thick as the pages read and still to come,
  // on a hard cover a little larger than the pages.
  edges() {
    const narrow = this.book.clientWidth < 640 && (this.book.parentElement || this.book).clientWidth < 700;
    if (narrow) {
      this.book.style.boxShadow = "";
      return;
    }
    const place = this.book.__place || { chapter: 0, chapters: 1 };
    const f = Math.max(0, Math.min(1, (place.chapter + (this.page + 1) / this.pages) / Math.max(1, place.chapters)));
    const D = 14;
    const before = Math.max(1, Math.round(D * f)), after = Math.max(1, Math.round(D * (1 - f)));
    const sh = [];
    // Leaves stepping out from under the open pages: paper, with a fine line between each.
    for (let i = 1; i <= Math.max(before, after); i++) {
      const c = i % 2 ? "var(--edge-line)" : "var(--page-bg)";
      if (i <= before) sh.push(`${-i}px ${(i * 0.55).toFixed(2)}px 0 0 ${c}`);
      if (i <= after) sh.push(`${i}px ${(i * 0.55).toFixed(2)}px 0 0 ${c}`);
    }
    // The cover board, then its shadow on the table.
    const ring = D + 9;
    sh.push(`0 ${(D * 0.55 + 5).toFixed(1)}px 0 ${ring}px var(--cover, #6b2d2d)`);
    sh.push(`0 ${(D * 0.55 + 6).toFixed(1)}px 0 ${ring + 1}px rgba(0,0,0,0.35)`);
    sh.push(`0 ${D + 26}px 50px ${D}px rgba(0,0,0,0.38)`);
    this.book.style.boxShadow = sh.join(", ");
  }

  // What a page picture depends on besides its place: size, colours and type.
  snapKey() {
    const reader = this.book.closest(".reader");
    return `${this.width}x${this.book.clientHeight}|${reader ? reader.dataset.theme : ""}|${reader ? reader.getAttribute("style") : ""}`;
  }

  // Which sentences the page at `offset` starts and ends with, as laid out right now: a picture
  // made when this was different (text size changed, a picture loaded) shows the wrong text.
  pageSig(offset) {
    const a = this.article.getBoundingClientRect().left;
    let first = -1, last = -1;
    for (const e of this.article.querySelectorAll(".s")) {
      const x = e.getBoundingClientRect().left - a;
      if (x >= offset - 1 && x < offset + this.width - 1) {
        if (first < 0) first = e.dataset.s;
        last = e.dataset.s;
      }
    }
    return `${first}-${last}`;
  }

  snap(offset) {
    const s = this.snaps.get(offset);
    return s && s.key === this.snapKey() && s.canvas && s.sig === this.pageSig(offset) ? s.canvas : null;
  }

  // Pictures of these pages (by offset), making any that are missing.
  async pictures(offsets) {
    const key = this.snapKey();
    const out = [];
    for (const offset of offsets) {
      let entry = this.snaps.get(offset);
      const sig = this.pageSig(offset);
      if (!entry || entry.key !== key || entry.sig !== sig) {
        entry = { key, sig, canvas: null };
        entry.promise = snapshotPage(this, this.article, offset).then((c) => (entry.canvas = c));
        this.snaps.set(offset, entry);
      }
      try {
        await entry.promise;
      } catch (e) {
        // A browser that won't let a page be pictured at all (it taints the canvas) turns pages
        // with the 3D strips; any other failure is for this picture only, tried again next time.
        console.warn("page picture failed:", e);
        this.snaps.delete(offset);
        if (e && e.name === "SecurityError") this.noGL = true;
        return null;
      }
      out.push(entry.canvas);
    }
    return out;
  }

  // Pictures this page and the ones either side, quietly, so a turn can start at once.
  warm() {
    if (this.noGL || !this.curl) return;
    clearTimeout(this.warmTimer);
    this.warmTimer = setTimeout(async () => {
      for (const p of [this.page, this.page + 1, this.page - 1]) {
        if (this.dead || this.noGL || p < 0 || p >= this.pages) continue;
        await this.pictures([p * this.width]);
        await new Promise((r) => (window.requestIdleCallback ? requestIdleCallback(r, { timeout: 300 }) : setTimeout(r, 30)));
      }
    }, 150);
  }

  showElement(el, animate) {
    const p = this.pageOf(el);
    if (p !== this.page) this.go(p, animate);
  }

  go(p, animate) {
    p = Math.max(0, Math.min(this.pages - 1, p));
    // Mid-turn, moving the page underneath would tear the turn; go there when it lands.
    if (this.busy) {
      this.pendingGo = p;
      return;
    }
    if (p === this.page) return;
    if (animate && Math.abs(p - this.page) === 1 && !this.busy) {
      this.turn(p > this.page ? 1 : -1);
      return;
    }
    this.page = p;
    this.place(this.article, p);
    this.report();
  }

  // Opens on the last page (coming back from the next chapter), and stays there if pictures or
  // fonts arriving later add pages.
  end() {
    this.atEnd = true;
    this.go(this.pages - 1, false);
  }

  next() {
    this.atEnd = false;
    if (this.busy) { this.queued = 1; return; }
    if (this.page + 1 < this.pages) this.turn(1);
    else if (this.h.onNext) this.carryInto(1, this.h.onNext);
  }

  prev() {
    this.atEnd = false;
    if (this.busy) { this.queued = -1; return; }
    if (this.page > 0) this.turn(-1);
    else if (this.h.onPrev) this.carryInto(-1, this.h.onPrev);
  }

  // Turning past the chapter's first or last page: the page on screen is kept, the neighbouring
  // chapter is laid out underneath, and the kept page turns away over it.
  async carryInto(dir, go) {
    this.busy = true;
    if (this.curl && !this.noGL) await Promise.race([this.pictures([this.page * this.width]), new Promise((r) => setTimeout(r, 800))]);
    this.busy = false;
    if (this.dead) return;
    this.book.__carry = {
      dir, snap: this.article.cloneNode(true), at: this.page * this.width, spread: this.spread,
      picture: this.snap(this.page * this.width), key: this.snapKey(),
    };
    // Keep this page on screen (as a picture) while the next chapter loads underneath, so the
    // new chapter doesn't show before the page has turned away.
    const pic = this.book.__carry.picture;
    if (pic && !this.noGL) {
      try {
        if (!sharedCurl || sharedCurl.lost) sharedCurl = new CurlGL(this.book);
        if (sharedCurl.canvas.parentNode !== this.book) this.book.appendChild(sharedCurl.canvas);
        sharedCurl.cover(pic, this.width, this.book.clientHeight);
        // Never stuck: if no turn picks it up, the cover goes.
        setTimeout(() => { if (this.book.__carry && this.book.__carry.picture === pic) { delete this.book.__carry; uncover(); } }, 6000);
      } catch (_) {}
    }
    go();
  }

  // A copy of the laid-out chapter (or of a kept one), shifted so `offset` px of it sits at the
  // left edge of the book.
  copy(offset, from) {
    const c = (from || this.article).cloneNode(true);
    c.classList.remove("synced");
    c.querySelectorAll(".s.now").forEach((e) => e.classList.remove("now"));
    // Lazy pictures in a fresh copy would start blank and pop in.
    c.querySelectorAll("img[loading]").forEach((e) => e.removeAttribute("loading"));
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
    const t = this.beginGL(dir, carry);
    if (t) return t;
    // The strips are only for browsers without the curl; elsewhere a missing picture means no
    // animation this time (the caller changes the page directly) rather than a different one.
    return this.noGL ? this.begin3d(dir, carry) : null;
  }

  // The curl on the graphics card, when pictures of both sides of the turning page are ready.
  beginGL(dir, carry) {
    if (this.noGL) return null;
    const W = this.width, H = this.book.clientHeight, half = W / 2;
    const target = carry ? this.page : this.page + dir;
    const cur = carry ? carry.at : this.page * W;
    const curPic = carry ? (carry.key === this.snapKey() ? carry.picture : null) : this.snap(cur);
    const nextPic = this.snap(target * W);
    if (!curPic || !nextPic) return null;
    try {
      // One WebGL context for the whole app (browsers allow only a few), moved into this book.
      if (!sharedCurl || sharedCurl.lost) sharedCurl = new CurlGL(this.book);
      this.gl = sharedCurl;
      if (this.gl.canvas.parentNode !== this.book) this.book.appendChild(this.gl.canvas);
    } catch (e) {
      console.warn("no WebGL page curl:", e);
      this.noGL = true;
      return null;
    }
    this.busy = true;
    // The canvas draws the whole book for the length of the turn (the turning page, the page it
    // uncovers and the facing page), from pictures; the live page underneath doesn't change
    // until the turn has landed, so nothing can show through early.
    const t = { dir, target, W, H, layers: [], gl: true };
    let front, back, under;
    if (!this.spread) {
      t.leaf = [0, W];
      t.hinge = 0;
      t.single = true;
      t.C0x = W;
      t.ghost = true;
      t.reverse = dir < 0;
      if (dir > 0) {
        [front, back, under] = [curPic, curPic, nextPic];
      } else {
        // The previous page is laid back down over this one.
        [front, back, under] = [nextPic, nextPic, curPic];
      }
    } else if (dir > 0) {
      t.leaf = [half, W];
      t.hinge = half;
      t.C0x = W;
      t.still = [0, half];
      [front, back, under] = [curPic, nextPic, nextPic];
    } else {
      t.leaf = [0, half];
      t.hinge = half;
      t.C0x = 0;
      t.still = [half, W];
      [front, back, under] = [curPic, nextPic, nextPic];
    }
    try {
      this.gl.texture(0, front);
      this.gl.texture(1, back);
      this.gl.texture(2, under);
    } catch (e) {
      console.warn("page pictures can't be drawn:", e);
      this.noGL = true;
      this.busy = false;
      return null;
    }
    // The paper colour, for the back of a single page.
    const probe = document.createElement("canvas").getContext("2d");
    probe.fillStyle = getComputedStyle(this.book).getPropertyValue("--page-bg").trim() || "#fff";
    const hex = probe.fillStyle.startsWith("#") ? probe.fillStyle : "#ffffff";
    t.paper = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
    this.gl.size(W, H);
    this.gl.canvas.style.visibility = "visible";
    if (carry) {
      // Coming from the last chapter: its page is already on the canvas (see carryInto).
      this.gl.canvas.style.opacity = "1";
    } else {
      // The pictures and the live page can differ by a pixel here and there; a quick cross-fade
      // hides that instead of letting it jump.
      this.gl.canvas.style.opacity = "0";
      t.fadeStart = performance.now();
    }
    t.canvasLayer = { remove: () => this.gl.clear() };
    t.layers.push(t.canvasLayer);
    return t;
  }

  begin3d(dir, carry) {
    this.busy = true;
    if (this.gl) this.gl.clear();
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
        if (!carry) t.placeTarget = true;
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
      if (!carry) t.placeTarget = true;
    } else {
      // Two pages, backwards: the left page turns over to the right; its back is the previous right page.
      t.leaf = [0, half];
      t.hinge = half;
      t.C0x = 0;
      front = this.copy(cur, src);
      back = this.copy(next);
      t.layers.push(this.halfLayer(half, this.copy(cur, src)));
      if (!carry) t.placeTarget = true;
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
    // On the graphics card a single page rolls up into the spine; otherwise it swings over it.
    const F = { x: t.gl && t.single ? t.hinge : 2 * t.hinge - t.C0x, y };
    return t.reverse ? [F, C] : [C, F];
  }

  pose(t, P) {
    if (t.gl) this.poseGL(t, P);
    else this.pose3d(t, P);
  }

  // The curl: the corner of the sheet pulled to P. The sheet is flat up to a lift line, rolls
  // over a cylinder of radius R, and lies flat again (back up) beyond it, so the corner (at
  // s = (d + πR) / 2 past the line) lands exactly at P. The lift line never crosses the spine.
  poseGL(t, P) {
    const { W, H } = t;
    const C = { x: t.C0x, y: t.C.y };
    const w = t.leaf[1] - t.leaf[0];
    // Paper doesn't stretch: the corner stays within a page's width of the spine.
    const hp = { x: t.hinge, y: C.y };
    let vx = P.x - hp.x, vy = P.y - hp.y;
    const r = Math.hypot(vx, vy);
    if (r > w) { vx *= w / r; vy *= w / r; }
    P = { x: hp.x + vx, y: hp.y + vy };
    let d = Math.hypot(P.x - C.x, P.y - C.y);
    const flat = { x0: t.leaf[0], w, W, H, hinge: t.hinge, ghost: !!t.ghost, paper: t.paper, squash: 1, under: true, still: t.still };
    if (d < 0.5) {
      // Lying flat: the lift line far off on the side away from the corner (s < 0 everywhere),
      // so the whole sheet is drawn flat, front up, over the page beneath it.
      this.gl.draw({ ...flat, L0: { x: -1e5, y: 0 }, dir: { x: -1, y: 0 }, R: 1 });
      this.revealUnder(t);
      t.progress = 0;
      return;
    }
    let n = { x: (P.x - C.x) / d, y: (P.y - C.y) / d };
    // Held at the spine, a page can only be pulled toward it: the fold stays within about 25°
    // of upright, so it can't be dragged round to lie sideways or folded top to bottom.
    const toward = Math.sign(t.hinge - C.x) || -1;
    if (n.x * toward < 0.05) n = { x: 0.05 * toward, y: n.y };
    const maxTilt = 0.47;
    if (Math.abs(n.y) > maxTilt * Math.abs(n.x)) n = { x: n.x, y: Math.sign(n.y) * maxTilt * Math.abs(n.x) };
    const nl = Math.hypot(n.x, n.y);
    n = { x: n.x / nl, y: n.y / nl };
    const toSpine = Math.min((t.hinge - C.x) * n.x - C.y * n.y, (t.hinge - C.x) * n.x + (H - C.y) * n.y);
    if (d > 2 * toSpine) d = Math.max(0.5, 2 * toSpine);
    P = { x: C.x + n.x * d, y: C.y + n.y * d };
    const span = (t.single ? Math.abs(t.hinge - C.x) : Math.abs(2 * t.hinge - 2 * C.x)) || W;
    const turned = Math.min(1, d / span);
    // Flex: a small curl at the corner, a wide soft bend mid-turn, more when moved quickly.
    const now = performance.now();
    if (t.lastP) {
      const speed = Math.hypot(P.x - t.lastP.x, P.y - t.lastP.y) / Math.max(1, now - t.lastT);
      t.flex = (t.flex || 0) * 0.8 + Math.min(w * 0.12, speed * 22) * 0.2;
    }
    t.lastP = P;
    t.lastT = now;
    const most = (2 * toSpine - d) / Math.PI;
    let R = Math.min(w * 0.35, 8 + d * 0.12 + w * 0.18 * Math.sin(Math.PI * turned) + (t.flex || 0));
    // One page, at the end: the sheet lifts off whole and rolls up against the spine...
    const tail = t.single ? Math.max(0, Math.min(1, (turned - 0.7) / 0.3)) : 0;
    R = Math.max(0.5, Math.min(most, R + (most - R) * tail));
    const sC = (d + Math.PI * R) / 2;
    const L0 = { x: C.x + n.x * sC, y: C.y + n.y * sC };
    // ...and turns edge-on into it.
    const upright = t.single ? Math.max(0, Math.min(1, (turned - 0.8) / 0.2)) : 0;
    const p = Math.min(1, Math.abs(P.x - C.x) / span);
    t.progress = t.reverse ? 1 - p : p;
    // The sheet's shadow on the page it uncovers, deepest just past the roll.
    const lift = Math.sin(Math.PI * Math.min(1, turned));
    const shadow = {
      edge: { x: L0.x - n.x * R, y: L0.y - n.y * R },
      dir: { x: -n.x, y: -n.y },
      reach: Math.min(170, 30 + d * 0.5),
      strength: 0.4 * lift + 0.06,
    };
    this.gl.draw({ ...flat, L0, dir: { x: -n.x, y: -n.y }, R, squash: 1 - 0.98 * upright, shadow });
    this.revealUnder(t);
  }

  fadeIn(t) {
    return new Promise((resolve) => {
      const step = () => {
        this.revealUnder(t);
        if (t.fadeStart === undefined) frame(() => resolve());
        else frame(step);
      };
      step();
    });
  }

  // The canvas fades in over the live page at the start of a turn; and for the strips (which are
  // live copies), the page underneath changes once they're drawn over it.
  revealUnder(t) {
    if (t.fadeStart !== undefined) {
      const a = Math.min(1, (performance.now() - t.fadeStart) / (FADE_MS * slow()));
      this.gl.canvas.style.opacity = a.toFixed(3);
      if (a < 1) return;
      t.fadeStart = undefined;
    }
    if (!t.placeTarget) return;
    t.placeTarget = false;
    frame(() => {
      if (this.busy && !t.done) this.place(this.article, t.target);
    });
  }

  // Turns the sheet so its free edge is over x = P.x (P.y is where it's held, which tips it).
  pose3d(t, P) {
    const { H } = t;
    // (The page underneath changes once the strips are drawn over it.)
    this.revealUnder(t);
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
    await this.animate(t, from, to, TURN_MS * slow() * Math.max(0.4, Math.min(1, left)), arc);
    t.done = true;
    const done = complete;
    if (done) this.page = t.target;
    this.place(this.article, this.page);
    if (t.gl) {
      // The live page is in place underneath (the copies that stood in for it go now); let the
      // picture of it fade away.
      t.layers.forEach((e) => e !== t.canvasLayer && e.remove());
      t.layers = [t.canvasLayer];
      this.gl.canvas.style.opacity = "1";
      await new Promise((resolve) => {
        const start = performance.now();
        const step = (now) => {
          const a = Math.max(0, 1 - (now - start) / (FADE_MS * slow()));
          this.gl.canvas.style.opacity = a.toFixed(3);
          if (a > 0) frame(step);
          else resolve();
        };
        frame(step);
      });
    }
    if (done) this.report();
    t.layers.forEach((e) => e.remove());
    this.busy = false;
    if (this.relayoutAfter) {
      this.relayoutAfter = false;
      this.relayout();
    }
    if (this.pendingGo !== undefined) {
      const p = this.pendingGo;
      this.pendingGo = undefined;
      this.go(p, false);
    }
    if (this.queued) {
      const q = this.queued;
      this.queued = 0;
      q > 0 ? this.next() : this.prev();
    }
  }

  get curl() {
    return this.book.__curl !== false;
  }

  async turn(dir, carry) {
    if (!this.curl) {
      // No animation: just show the page (a turn into the next chapter has already happened).
      if (!carry) this.go(this.page + dir, false);
      return;
    }
    // Every turn is the same curl: wait (briefly) for the pictures it needs rather than fall
    // back to the strips mid-book.
    if (!this.noGL) {
      this.busy = true;
      const target = carry ? this.page : this.page + dir;
      const need = carry ? [target * this.width] : [this.page * this.width, target * this.width];
      const got = await Promise.race([this.pictures(need), new Promise((r) => setTimeout(() => r(null), 4000))]);
      this.busy = false;
      if (this.dead) return;
      // Pictures not to be had: change the page without an animation rather than show a
      // different, flickery one.
      if (!got && !this.noGL && !(carry ? carry.picture && this.snap(target * this.width) : this.snap(this.page * this.width) && this.snap(target * this.width))) {
        console.warn("page turn without animation: page pictures not ready");
        if (!carry) this.go(target, false);
        uncover();
        return;
      }
    }
    const t = this.begin(dir, carry);
    if (!t) {
      if (!carry) this.go(this.page + dir, false);
      uncover();
      return;
    }
    const H = t.H;
    t.C = { x: t.C0x, y: H * 0.94 };
    const [rest] = this.ends(t, t.C.y);
    this.pose(t, rest);
    // Fade the picture in over the page while it's still at rest, so the page underneath never
    // shows through a lifting corner before it's been changed to the right one.
    if (t.gl) await this.fadeIn(t);
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
      this.swallowClick = false;
      if (e.button !== 0) return;
      if (e.target.closest && e.target.closest("a[data-href]")) return;
      // A finger already selecting text (a selection is showing) adjusts it, not the page.
      const selecting = e.pointerType === "touch" && String(window.getSelection()) !== "";
      start = { x: e.clientX, y: e.clientY, time: performance.now(), grab: this.curl && !selecting && grabZone(e) };
      this.selecting = !start.grab;
      // Holding the page, not selecting its words.
      if (start.grab && e.pointerType !== "touch") e.preventDefault();
    });
    this.book.addEventListener("pointermove", (e) => {
      if (!start) {
        if (e.pointerType === "mouse") this.book.classList.toggle("grab-edge", this.curl && !this.busy && grabZone(e));
        return;
      }
      const dx = e.clientX - start.x;
      const rect = this.book.getBoundingClientRect();
      if (!drag) {
        if (!this.curl || this.busy || !start.grab || Math.abs(dx) < 6 || Math.abs(dx) < Math.abs(e.clientY - start.y)) return;
        const dir = dx < 0 ? 1 : -1;
        if ((dir > 0 && this.page + 1 >= this.pages) || (dir < 0 && this.page === 0)) {
          // At the chapter's edge the next page isn't laid out yet: a pull far enough turns into
          // the neighbouring chapter when you let go (never before).
          start.cross = dir;
          start.crossDx = dx;
          window.getSelection().removeAllRanges();
          return;
        }
        window.getSelection().removeAllRanges();
        if (!this.noGL && (!this.snap(this.page * this.width) || !this.snap((this.page + dir) * this.width))) {
          // Pictures still being made: take hold once they're ready (a moment).
          this.pictures([this.page * this.width, (this.page + dir) * this.width]);
          return;
        }
        drag = this.begin(dir);
        if (!drag) return;
        const y = Math.max(0, Math.min(drag.H, start.y - rect.top));
        drag.C = { x: drag.C0x, y };
        drag.G = { x: start.x - rect.left, y };
        const [rest] = this.ends(drag, y);
        drag.from = rest;
        this.book.classList.add("grab-edge");
        try { this.book.setPointerCapture(e.pointerId); } catch (_) {}
      }
      let P = { x: drag.from.x + dx, y: drag.from.y + (e.clientY - start.y) };
      if (drag.gl && !drag.reverse) {
        // The paper under your finger stays under it: the fold is halfway between where you
        // took hold and where your finger is now, and the corner goes wherever that carries it.
        const F = { x: e.clientX - rect.left, y: Math.max(-drag.H * 0.2, Math.min(drag.H * 1.2, e.clientY - rect.top)) };
        const G = drag.G;
        const gx = F.x - G.x, gy = F.y - G.y;
        const gd = Math.hypot(gx, gy);
        if (gd > 0.5) {
          const n = { x: gx / gd, y: gy / gd };
          const M = { x: (G.x + F.x) / 2, y: (G.y + F.y) / 2 };
          const k = 2 * ((drag.C.x - M.x) * n.x + (drag.C.y - M.y) * n.y);
          P = { x: drag.C.x - k * n.x, y: drag.C.y - k * n.y };
        } else {
          P = drag.from;
        }
      }
      drag.at = P;
      this.pose(drag, P);
    });
    const release = () => {
      if (start && start.cross && !drag) {
        const fast = performance.now() - start.time < 300;
        if (Math.abs(start.crossDx) > 80 || (fast && Math.abs(start.crossDx) > 20)) {
          this.swallowClick = true;
          start.cross > 0 ? this.next() : this.prev();
        }
      }
      if (drag) {
        const fast = performance.now() - start.time < 300;
        const p = drag.progress || 0;
        const moved = drag.reverse ? 1 - p : p;
        this.finish(drag, drag.at || drag.from, moved > 0.3 || (fast && moved > 0.04));
        // The click that follows a drag isn't a tap (however late it arrives).
        this.swallowClick = true;
      }
      start = null;
      drag = null;
      this.book.classList.remove("grab-edge");
    };
    // A sideways two-finger swipe on a trackpad moves the page with your fingers, like a drag;
    // the turn finishes (or falls back) only once the fingers stop.
    let swipe = null;
    const endSwipe = () => {
      const sw = swipe;
      swipe = null;
      if (!sw) return;
      if (sw.cross) {
        if (Math.abs(sw.total) > 90) sw.cross > 0 ? this.next() : this.prev();
        return;
      }
      const p = sw.t.progress || 0;
      this.finish(sw.t, sw.t.at || sw.t.from, (sw.t.reverse ? 1 - p : p) > 0.3);
    };
    this.book.addEventListener("wheel", (e) => {
      if (Math.abs(e.deltaX) <= Math.abs(e.deltaY)) return;
      e.preventDefault();
      if (!swipe) {
        if (this.busy || !this.curl) return;
        const dir = e.deltaX > 0 ? 1 : -1;
        if ((dir > 0 && this.page + 1 >= this.pages) || (dir < 0 && this.page === 0)) {
          swipe = { cross: dir, total: 0 };
        } else {
          if (!this.noGL && (!this.snap(this.page * this.width) || !this.snap((this.page + dir) * this.width))) {
            this.pictures([this.page * this.width, (this.page + dir) * this.width]);
            return;
          }
          const t = this.begin(dir);
          if (!t) return;
          t.C = { x: t.C0x, y: t.H * 0.75 };
          [t.from] = this.ends(t, t.C.y);
          swipe = { t, total: 0 };
        }
      }
      swipe.total += e.deltaX;
      if (swipe.t) {
        swipe.t.at = { x: swipe.t.from.x - swipe.total * 1.4, y: swipe.t.from.y };
        this.pose(swipe.t, swipe.t.at);
      }
      clearTimeout(swipe.timer);
      swipe.timer = setTimeout(endSwipe, 160);
    }, { passive: false });
    this.book.addEventListener("pointerleave", () => { if (!drag) this.book.classList.remove("grab-edge"); });
    this.book.addEventListener("pointerup", release);
    this.book.addEventListener("pointercancel", release);
    // Double-click selects a word (the browser's own), drag selects any stretch of text, and a
    // triple-click selects the whole sentence (rather than the paragraph).
    this.book.addEventListener("click", (e) => {
      if (e.detail !== 3) return;
      const sentence = e.target.closest && e.target.closest(".s");
      if (!sentence) return;
      const spans = this.article.querySelectorAll(`.s[data-s="${sentence.dataset.s}"]`);
      const range = document.createRange();
      range.setStartBefore(spans[0]);
      range.setEndAfter(spans[spans.length - 1]);
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
    });
    // Taps on the outer edges turn the page; taps elsewhere reach the sentences.
    this.book.addEventListener(
      "click",
      (e) => {
        if (this.swallowClick) {
          this.swallowClick = false;
          e.stopPropagation();
          e.preventDefault();
          return;
        }
        if (e.target.closest("a[data-href]")) return;
        const x = (e.clientX - this.book.getBoundingClientRect().left) / this.width;
        const edge = this.spread ? 0.12 : 0.22;
        const onEdge = x < edge || x > 1 - edge;
        // A click that ends a text selection isn't a turn; any other click on an edge is, even if
        // something elsewhere happens to be selected.
        if (!onEdge || (this.selecting && String(window.getSelection()) !== "")) return;
        e.stopPropagation();
        window.getSelection().removeAllRanges();
        x < edge ? this.prev() : this.next();
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
// Page turns animated (the curl) or instant.
export function pagerSetCurl(book, on) {
  if (!book) return;
  book.__curl = !!on;
}

// Where this chapter sits in the book (for the thickness of the page stacks).
export function pagerSetPlace(book, chapter, chapters) {
  if (!book) return;
  book.__place = { chapter, chapters };
  if (book.__pager) book.__pager.edges();
}

// Cloth and leather colours for the hard cover; a book always gets the same one, picked by its
// title.
const COVER_COLOURS = [
  "#7a2e2e", // oxblood
  "#2f4a6b", // navy
  "#2e5e4a", // bottle green
  "#6b4a2b", // tan leather
  "#4a3566", // aubergine
  "#8a5a1e", // ochre
  "#2c5560", // teal
  "#5c2a3d", // burgundy
  "#3b4a2a", // olive
  "#6e3b22", // rust
  "#28334a", // midnight
  "#5a5245", // taupe
];

export function bookCover(book, title) {
  if (!book) return;
  let h = 2166136261;
  for (const ch of title || "") {
    h ^= ch.codePointAt(0);
    h = Math.imul(h, 16777619);
  }
  book.style.setProperty("--cover", COVER_COLOURS[(h >>> 0) % COVER_COLOURS.length]);
}

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

// The sidebar beside the reader: hidden or shown, remembered on this device.
export function sidebarHidden() {
  try { return localStorage.getItem("duet-sidebar") === "hidden"; } catch (_) { return false; }
}

export function setSidebarHidden(hidden) {
  const app = document.querySelector(".app");
  if (app) app.classList.toggle("no-sidebar", hidden);
  try { localStorage.setItem("duet-sidebar", hidden ? "hidden" : "shown"); } catch (_) {}
}

export function isFullscreen() {
  return !!document.fullscreenElement;
}

// Full screen for the whole app (the browser's own Esc also leaves it).
export async function toggleFullscreen() {
  try {
    if (document.fullscreenElement) await document.exitFullscreen();
    else await document.documentElement.requestFullscreen({ navigationUI: "hide" });
  } catch (_) {}
  return !!document.fullscreenElement;
}

// Whether sentence n is (at least partly) on the page or pages showing now.
export function sentenceOnPage(container, n) {
  const book = container && container.closest(".bookview");
  if (!book || n < 0) return false;
  const b = book.getBoundingClientRect();
  return Array.from(container.querySelectorAll(`.s[data-s="${n}"]`)).some((e) => {
    const r = e.getBoundingClientRect();
    return r.width > 0 && r.right > b.left && r.left < b.right && r.bottom > b.top && r.top < b.bottom;
  });
}

// The word under the pointer, selected (so it's clear which one a right-click menu is about).
export function wordAt(x, y) {
  let node, offset;
  if (document.caretPositionFromPoint) {
    const p = document.caretPositionFromPoint(x, y);
    if (p) { node = p.offsetNode; offset = p.offset; }
  } else if (document.caretRangeFromPoint) {
    const r = document.caretRangeFromPoint(x, y);
    if (r) { node = r.startContainer; offset = r.startOffset; }
  }
  if (!node || node.nodeType !== 3) return "";
  const text = node.data;
  const isWord = (c) => /[\p{L}\p{N}'’-]/u.test(c);
  let a = offset, b = offset;
  while (a > 0 && isWord(text[a - 1])) a--;
  while (b < text.length && isWord(text[b])) b++;
  if (a === b) return "";
  const range = document.createRange();
  range.setStart(node, a);
  range.setEnd(node, b);
  const sel = window.getSelection();
  sel.removeAllRanges();
  sel.addRange(range);
  return text.slice(a, b).replace(/^['’-]+|['’-]+$/g, "");
}

export function copyText(text) {
  if (navigator.clipboard) navigator.clipboard.writeText(text).catch(() => {});
}
