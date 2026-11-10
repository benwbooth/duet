// Browser bits that are simpler in JavaScript: the microphone, highlighting, scrolling.

let recorder = null;
let chunks = [];

export function micSupported() {
  return !!(navigator.mediaDevices && window.MediaRecorder);
}

// Transcribes what's been said so far while recording, so the words appear as you talk:
// every half second, the recording so far (if it's grown) goes to the server, one request at a time.
let partial = null;

async function transcribeChunks(type) {
  const ext = type.includes("mp4") ? "m4a" : type.includes("ogg") ? "ogg" : "webm";
  const form = new FormData();
  form.append("audio", new Blob(chunks, { type }), `speech.${ext}`);
  const resp = await fetch("/api/assistant/transcribe", { method: "POST", body: form });
  const body = await resp.json();
  if (!resp.ok) throw new Error(body.error || `HTTP ${resp.status}`);
  return body;
}

// Gets speech recognition loaded (it's unloaded when idle, and loading takes a while) before
// the microphone is used: a moment of silence to transcribe.
let warmedAt = 0;
export function warmSpeech() {
  if (Date.now() - warmedAt < 60000) return;
  warmedAt = Date.now();
  const n = 8000, buf = new ArrayBuffer(44 + n * 2), v = new DataView(buf);
  const str = (o, t) => [...t].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
  str(0, "RIFF"); v.setUint32(4, 36 + n * 2, true); str(8, "WAVEfmt "); v.setUint32(16, 16, true);
  v.setUint16(20, 1, true); v.setUint16(22, 1, true); v.setUint32(24, 16000, true); v.setUint32(28, 32000, true);
  v.setUint16(32, 2, true); v.setUint16(34, 16, true); str(36, "data"); v.setUint32(40, n * 2, true);
  const form = new FormData();
  form.append("audio", new Blob([buf], { type: "audio/wav" }), "warm.wav");
  fetch("/api/assistant/transcribe", { method: "POST", body: form }).catch(() => {});
}

export async function startRecording(onPartial, onLevel) {
  // Waiting on a permission prompt nobody can see would look like the button doing nothing.
  const stream = await Promise.race([
    navigator.mediaDevices.getUserMedia({ audio: true }),
    new Promise((_, no) => setTimeout(() => no(new Error("no answer from the microphone permission prompt")), 15000)),
  ]);
  chunks = [];
  recorder = new MediaRecorder(stream);
  recorder.ondataavailable = (e) => { if (e.data.size) chunks.push(e.data); };
  recorder.start(500);
  const live = { busy: null, sent: 0, stopped: false, error: null };
  partial = live;
  // How loud it is, for the button to show it's hearing you.
  try {
    const ctx = new AudioContext();
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 512;
    ctx.createMediaStreamSource(stream).connect(analyser);
    const buf = new Float32Array(analyser.fftSize);
    live.ctx = ctx;
    const meter = () => {
      if (live.stopped) return;
      analyser.getFloatTimeDomainData(buf);
      let sum = 0;
      for (const v of buf) sum += v * v;
      if (onLevel) onLevel(Math.min(1, Math.sqrt(sum / buf.length) * 6));
      requestAnimationFrame(meter);
    };
    meter();
  } catch (_) {}
  const tick = async () => {
    if (live.stopped) return;
    if (chunks.length > live.sent && recorder) {
      live.sent = chunks.length;
      live.busy = transcribeChunks(recorder.mimeType || "audio/webm")
        .then((text) => { if (!live.stopped && onPartial) onPartial(text); })
        .catch((e) => { live.error = e; });
      await live.busy;
      live.busy = null;
    }
    if (!live.stopped) live.timer = setTimeout(tick, 500);
  };
  live.timer = setTimeout(tick, 900);
}

// Stops recording and returns the whole transcript from the server.
export async function stopRecording() {
  if (!recorder) return "";
  if (partial) {
    partial.stopped = true;
    clearTimeout(partial.timer);
    if (partial.ctx) partial.ctx.close().catch(() => {});
    if (partial.busy) await partial.busy;
    partial = null;
  }
  const done = new Promise((resolve) => (recorder.onstop = resolve));
  recorder.stop();
  await done;
  recorder.stream.getTracks().forEach((t) => t.stop());
  const type = recorder.mimeType || "audio/webm";
  recorder = null;
  return transcribeChunks(type);
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
    // (A sentence that runs over from the page before is already where it's being read: the
    // read-along turns the page when the words being said reach the next one.)
    if (!sentenceOnPage(container, n) && !browsing(pager.book)) pager.showElement(spans[0], true);
    return;
  }
  const r = spans[0].getBoundingClientRect();
  if (r.top < 80 || r.bottom > window.innerHeight - 160) {
    spans[0].scrollIntoView({ behavior: document.documentElement.classList.contains("eink") ? "auto" : "smooth", block: "center" });
  }
}

// The sentence at the top of the screen (or page), so "resume" and the assistant know where you are.
export function visibleSentence(container) {
  if (!container) return 0;
  const pager = pagerOf(container);
  // (Page pictures: the page showing, which is where a comic or PDF is kept.)
  if (!container.querySelector(".s") && container.querySelector(".img-page")) return Math.max(0, visiblePage(container));
  if (pager) return pager.firstSentence();
  for (const e of container.querySelectorAll(".s")) {
    if (e.getBoundingClientRect().bottom > 70) return Number(e.dataset.s);
  }
  return 0;
}

export function scrollToSentence(container, n) {
  const e = container && (container.querySelector(`.s[data-s="${n}"]`) || container.querySelector(`.img-page[data-p="${n}"]`));
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
  const bubble = ev.target.closest(".bubble");
  if (bubble) return { page: Number(bubble.dataset.p), line: Number(bubble.dataset.b) };
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
// Bands a turning page is cut into across its roll (plus the flat parts either side of it).
const BANDS = 14;

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
uniform vec2 u_pad;    // room drawn round the book, so a page lifting off it isn't cut off
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
  vec2 full = u_view + 2.0 * u_pad;
  // Depth spread over as much of the range as the paper can use (it rises at most a page's
  // width): squeezed into a sliver of it, the two sides of a tight roll, a pixel or two apart,
  // came out at the same depth and broke each other up in dashes along the roll's edge.
  gl_Position = vec4((scr.x + u_pad.x) / full.x * 2.0 - 1.0, 1.0 - (scr.y + u_pad.y) / full.y * 2.0, -P.z / (2.2 * max(u_leaf.y, 1.0)), 1.0);
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
uniform vec2 u_L0;
uniform vec2 u_dir;
uniform float u_R;
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
  // How far round the roll this point of paper is, worked out here for every pixel rather than
  // at the mesh's corners: interpolated between corners, a roll lying almost flat and twisted
  // across the mesh shaded in a sawtooth of grey and white bands.
  float s = dot(v_pos - u_L0, u_dir);
  float theta = clamp(s / max(u_R, 0.001), 0.0, 3.14159265);
  float c = cos(theta);
  // A roll only a few pixels across turns its shading into a dark band thinner than a pixel,
  // which the screen can only draw as broken dashes: the tighter the roll, the softer it's shaded.
  float soft = smoothstep(3.0, 24.0, u_R);
  bool front = gl_FrontFacing == (u_flip < 0.5);
  if (front) {
    vec3 col = texture2D(u_front, uv).rgb;
    float shade = 0.42 * pow(1.0 - max(c, 0.0), 1.5) * soft;
    gl_FragColor = vec4(outline(col * (1.0 - shade)), 1.0);
  } else {
    vec3 col;
    if (u_ghost > 0.5) {
      // A single page's back: the page itself, faintly, through the paper.
      col = mix(u_paper, texture2D(u_front, uv).rgb, 0.08);
    } else {
      col = texture2D(u_back, vec2((2.0 * u_hinge - v_pos.x) / u_view.x, uv.y)).rgb;
    }
    float dark = 0.4 * pow(max(0.0, 1.0 + c) / 2.0, 1.5) * mix(0.35, 1.0, soft);
    // (The highlight fades as the roll gets too tight to show it: thinner than a few pixels it
    // only shimmers.)
    float gloss = 0.3 * exp(-pow((theta - 2.2) / 0.41, 2.0)) * smoothstep(2.0, 10.0, u_R);
    gl_FragColor = vec4(outline(col * (1.0 - dark) + gloss), 1.0);
  }
}`;

let sharedCurl = null;

// Takes whatever the curl canvas is showing off the screen.
function uncover() {
  // (And whatever was kept for a turn into the next chapter that didn't happen.)
  document.querySelectorAll(".curl-cover, .carried").forEach((e) => e.remove());
  document.querySelectorAll(".bookview.turning").forEach((b) => { if (!b.__pager || !b.__pager.busy) b.classList.remove("turning"); });
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
uniform vec2 u_pad;
varying vec2 v_pos;
void main() {
  vec2 X = vec2(u_rect.x + a_uv.x * u_rect.y, a_uv.y * u_rect.z);
  v_pos = X;
  vec2 full = u_view + 2.0 * u_pad;
  gl_Position = vec4((X.x + u_pad.x) / full.x * 2.0 - 1.0, 1.0 - (X.y + u_pad.y) / full.y * 2.0, 0.0, 1.0);
}`;

const FLAT_FS = `
precision highp float;
uniform sampler2D u_tex;
uniform vec2 u_view;
uniform vec4 u_sh;     // shadow: edge point, direction away from the turning page
uniform vec3 u_shp;    // reach, strength, on
uniform vec2 u_fold;   // the spine's x, strength (0: no fold)
varying vec2 v_pos;
void main() {
  vec3 col = texture2D(u_tex, v_pos / u_view).rgb;
  if (u_fold.y > 0.0) {
    // The same shading as the book's fold (styles.css): darkest at the spine, gone 40px out.
    float d = abs(v_pos.x - u_fold.x) / 40.0;
    float f = d < 0.3 ? mix(0.16, 0.06, d / 0.3) : mix(0.06, 0.0, clamp((d - 0.3) / 0.7, 0.0, 1.0));
    col *= 1.0 - f * u_fold.y;
  }
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
    // WebGL 2 where there is one: it can make smaller copies (mipmaps) of a page picture of any
    // size, so the page's curled edge, where the picture is squeezed into a few pixels, is
    // smoothed rather than speckled with moiré.
    const opts = { antialias: true, alpha: true, premultipliedAlpha: true };
    const gl = this.canvas.getContext("webgl2", opts) || this.canvas.getContext("webgl", opts);
    if (!gl) throw new Error("no WebGL");
    this.gl = gl;
    this.mips = typeof WebGL2RenderingContext !== "undefined" && gl instanceof WebGL2RenderingContext;
    this.aniso = gl.getExtension("EXT_texture_filter_anisotropic");
    // Without multisampling (some drivers give none), edges the paper turns side-on to are
    // drawn as jagged dashes: drawn at twice the resolution instead, and smoothed down.
    this.ss = (gl.getParameter(gl.SAMPLES) || 0) >= 2 ? 1 : 2;
    console.info(`duet page curl: WebGL ${this.mips ? 2 : 1}, ${gl.getParameter(gl.SAMPLES) || 0}x multisampling, ${this.ss}x resolution`);
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
    for (const n of ["u_rect", "u_view", "u_pad", "u_tex", "u_sh", "u_shp", "u_fold"]) this.f[n] = gl.getUniformLocation(flat, n);
    this.flatLoc = gl.getAttribLocation(flat, "a_uv");
    gl.useProgram(prog);
    // A fine grid over the sheet: fine enough that the roll looks round from any angle, even
    // laid almost down near the spine, where the roll is only a few pixels across (a coarser
    // grid drew its edge there as a staircase of grey slivers). About 24,000 corners: nothing
    // to a graphics card, and still within 16-bit indices.
    const NX = 200, NY = 120, verts = [], idx = [];
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
    for (const n of ["u_leaf", "u_view", "u_pad", "u_L0", "u_dir", "u_R", "u_squash", "u_front", "u_back", "u_ghost", "u_flip", "u_hinge", "u_paper", "u_edge"])
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
    if (this.mips) {
      gl.generateMipmap(gl.TEXTURE_2D);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
    } else {
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    }
    // (Squeezed one way only, as across the curl: sharp along it, smooth across it.)
    if (this.aniso) {
      const max = gl.getParameter(this.aniso.MAX_TEXTURE_MAX_ANISOTROPY_EXT) || 1;
      gl.texParameterf(gl.TEXTURE_2D, this.aniso.TEXTURE_MAX_ANISOTROPY_EXT, Math.min(16, max));
    }
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  // The canvas floats over the whole page (not inside the book, whose surroundings would clip
  // it), over the book and a margin round it: a page curling up toward you reaches out past the
  // book's edges, in front of everything.
  attach(book) {
    this.book = book;
    if (this.canvas.parentNode !== document.body) document.body.appendChild(this.canvas);
  }

  size(W, H) {
    const dpr = window.devicePixelRatio || 1;
    const px = Math.round(W * 0.12), py = Math.round(H * 0.3);
    this.pad = [px, py];
    const cw = W + 2 * px, ch = H + 2 * py;
    const r = this.book ? this.book.getBoundingClientRect() : { left: 0, top: 0 };
    this.canvas.style.width = `${cw}px`;
    this.canvas.style.height = `${ch}px`;
    this.canvas.style.left = `${r.left - px}px`;
    this.canvas.style.top = `${r.top - py}px`;
    if (this.canvas.width !== Math.round(cw * dpr * this.ss) || this.canvas.height !== Math.round(ch * dpr * this.ss)) {
      this.canvas.width = Math.round(cw * dpr * this.ss);
      this.canvas.height = Math.round(ch * dpr * this.ss);
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
    gl.uniform2f(f.u_pad, ...(this.pad || [0, 0]));
    gl.uniform1i(f.u_tex, unit);
    gl.uniform2f(f.u_fold, ...(this.fold || [0, 0]));
    if (shadow) {
      gl.uniform4f(f.u_sh, shadow.edge.x, shadow.edge.y, shadow.dir.x, shadow.dir.y);
      gl.uniform3f(f.u_shp, shadow.reach, shadow.strength, 1);
    } else {
      gl.uniform3f(f.u_shp, 1, 0, 0);
    }
    // (Flat pages leave no depth behind: the turning page is always in front of them. Where it
    // lay almost flat it sat at nearly their depth, and the two fought pixel by pixel, the page
    // underneath showing through the turning one's edge in slivers.)
    gl.depthMask(false);
    gl.drawElements(gl.TRIANGLES, this.count, gl.UNSIGNED_SHORT, 0);
    gl.depthMask(true);
  }

  // Just a picture of the book, flat (keeping the old page on screen while the next chapter loads).
  cover(picture, W, H) {
    this.fold = null;
    this.texture(0, picture);
    this.size(W, H);
    this.clear();
    this.flatRect(0, W, H, W, 0, null);
    this.canvas.style.visibility = "visible";
    this.canvas.style.opacity = "1";
  }

  draw(p) {
    const gl = this.gl, u = this.u;
    this.fold = p.fold || null;
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
    gl.uniform2f(u.u_pad, ...(this.pad || [0, 0]));
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

// A copy of the page at `offset` (and the one after it), cut down from the whole chapter so it
// lays out quickly, but breaking its lines exactly as the live page does.
function pageCopy(article, offset, W, H) {
  const clone = article.cloneNode(true);
  // (The read-along highlight stays: it's on the page, so it's on the copy.)
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
  // Numbered lists keep their numbers (the items before the page are cut away).
  for (const ol of clone.querySelectorAll("ol")) {
    let n = ol.hasAttribute("start") ? parseInt(ol.getAttribute("start"), 10) || 1 : 1;
    const step = ol.hasAttribute("reversed") ? -1 : 1;
    for (const li of ol.children) {
      if (li.tagName !== "LI") continue;
      if (li.hasAttribute("value")) n = parseInt(li.getAttribute("value"), 10) || n;
      li.setAttribute("value", String(n));
      n += step;
    }
  }
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
  // ...and stops two pages later: only what can show (much quicker to draw), but with the text
  // carrying on past the page, so its last lines break as they do live (widows and orphans).
  const end = pageStart(article, offset + 2 * W);
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
  return clone;
}

// A picture of the book as it looks with `article` scrolled to `offset`: the page drawn through
// an SVG foreignObject with the reader's own styles, so it matches the real page.
// Whether the browser can draw an element exactly as it renders it into a canvas (HTML in
// Canvas, Chrome 15x+). With it, a page's picture for the graphics card is the browser's own
// rendering of the page, so the curl can run on the GPU and still look exactly like the page.
const canDrawElements = typeof CanvasRenderingContext2D !== "undefined" && "drawElementImage" in CanvasRenderingContext2D.prototype;

// A picture of the page at `offset`, rendered by the browser itself: a copy of the laid-out
// chapter (and the page's running head, number, outline and fold) inside a canvas that lays out
// its children without showing them, drawn into it once the browser has painted it.
async function elementSnapshot(pager, offset, other) {
  if (document.fonts) await document.fonts.ready;
  // (The page's exact size, fractions of a pixel included: on a scaled display the rounded
  // clientWidth is a little off, and text laid out a fraction narrower can break a line, and
  // so a page, differently.)
  // (Computed sizes: layout pixels, unlike a bounding box, which a transform or zoom scales.)
  const W = parseFloat(getComputedStyle(pager.article).width) || pager.width;
  const H = parseFloat(getComputedStyle(pager.book).height) || pager.book.clientHeight;
  const dpr = window.devicePixelRatio || 1;
  const cv = document.createElement("canvas");
  cv.setAttribute("layoutsubtree", "");
  cv.setAttribute("content", "drawable");
  cv.className = "page-shot";
  cv.width = Math.round(W * dpr);
  cv.height = Math.round(H * dpr);
  cv.style.width = `${W}px`;
  cv.style.height = `${H}px`;
  const sheet = document.createElement("div");
  sheet.setAttribute("drawable", "");
  sheet.className = "page-shot-sheet";
  sheet.style.width = `${W}px`;
  sheet.style.height = `${H}px`;
  // (The page's paper is the book's own background: the text is antialiased against it.)
  sheet.style.background = getComputedStyle(pager.book).backgroundColor;
  // (Or, with `other`, a page of the neighbouring chapter: its text laid out just as this one's.)
  const copy = other ? pager.article.cloneNode(false) : pager.article.cloneNode(true);
  if (other) copy.innerHTML = other.html;
  copy.querySelectorAll("img[loading]").forEach((e) => e.removeAttribute("loading"));
  copy.style.width = `${W}px`;
  copy.style.transform = other ? "none" : `translateX(${-offset}px)`;
  sheet.appendChild(copy);
  cv.appendChild(sheet);
  pager.book.appendChild(cv);
  let furniture;
  if (other) {
    await Promise.all([...copy.querySelectorAll("img")].map((img) => (img.complete ? null : img.decode().catch(() => {}))));
    const pages = Math.max(1, Math.round(sheet.scrollWidth / W));
    offset = other.last ? (pages - 1) * W : 0;
    copy.style.transform = `translateX(${-offset}px)`;
    furniture = pager.furniture(offset, true);
    // Its own running head and numbers (the chapter's title isn't known here: left blank).
    const rh = furniture.querySelector(".rh-chapter");
    if (rh) rh.textContent = "";
    const pageEl = furniture.querySelector(".folio-page");
    if (pageEl) {
      const p = Math.round(offset / W) + 1, b = pager.book.__bookPages;
      // (The neighbouring chapter's place in the whole book: just after this one, or just before.)
      const base = b ? b.base + (other.chapterStep > 0 ? pager.pages : -pages) : 0;
      pageEl.textContent = folioText(b ? { __bookPages: { base, total: b.total } } : null, p, pages);
    }
    const chEl = furniture.querySelector(".folio-chapter");
    if (chEl) chEl.textContent = chEl.textContent.replace(/\d+/, (n) => String(Number(n) + other.chapterStep));
  } else {
    furniture = pager.furniture(offset, true);
  }
  sheet.appendChild(furniture);
  // The read-along's paragraph and word tints go on the copy too (they're drawn over ranges of
  // the live text, which the copy doesn't share).
  const tinted = [];
  const st = other ? null : along.st;
  if (st && window.CSS && CSS.highlights) {
    const first = copy.querySelector(`.s[data-s="${st.n}"]`);
    const block = first && first.closest(ALONG_BLOCK);
    const para = CSS.highlights.get("duet-para");
    if (block && para) {
      const r = document.createRange();
      r.selectNodeContents(block);
      para.add(r);
      tinted.push([para, r]);
    }
    const word = CSS.highlights.get("duet-word");
    const w = st.shown >= 0 ? sentenceWords(copy, st.n)[st.shown] : null;
    if (word && w) {
      word.add(w);
      tinted.push([word, w]);
    }
  }
  try {
    await Promise.all([...copy.querySelectorAll("img")].map((img) => (img.complete ? null : img.decode().catch(() => {}))));
    await new Promise((resolve, reject) => {
      let done = false;
      const draw = () => {
        if (done) return;
        done = true;
        try {
          const ctx = cv.getContext("2d");
          // (Given its size in canvas pixels: drawn with no size, an element comes out at the
          // screen's own scale on top of the canvas's, so a scaled display would see it zoomed.)
          ctx.drawElementImage(sheet, 0, 0, cv.width, cv.height);
          resolve();
        } catch (e) {
          reject(e);
        }
      };
      cv.addEventListener("paint", draw, { once: true });
      // (If no paint event comes, it's been laid out and painted by now anyway.)
      setTimeout(draw, 1200);
    });
  } finally {
    cv.remove();
    for (const [h, r] of tinted) h.delete(r);
  }
  return cv;
}

async function snapshotPage(pager, article, offset) {
  if (document.fonts) await document.fonts.ready;
  const W = pager.width, H = pager.book.clientHeight;
  const reader = pager.book.closest(".reader") || pager.book.parentElement;
  if (pageCssText === null) {
    pageCssText = await fetch("styles.css").then((r) => r.text()).catch(() => "");
  }
  const clone = pageCopy(article, offset, W, H);
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
      pageEl.textContent = folioText(pager.book, Math.round(offset / W) + 1, pager.pages);
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
// The smallest blocks of a chapter: what a copy of a page shows or hides.
const LEAF = "p, h1, h2, h3, h4, h5, h6, li, dt, dd, figure, img, hr, pre, table, blockquote > :not(p), .img-page";

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
    // The curl runs on the graphics card when the browser can hand it its own rendering of the
    // page (see elementSnapshot); otherwise pages turn as live copies of the page itself, since
    // a picture redrawn another way never quite matches it (type, highlight, layout).
    this.noGL = !canDrawElements;
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
        // A page dragged over into this chapter has landed already: its picture just gives way
        // to the real page.
        if (carry.landed) frame(() => uncover());
        else if (carry.spread === this.spread) this.turn(carry.dir, carry);
        else uncover();
      }, 0);
    }
  }

  destroy() {
    this.resize.disconnect();
    document.removeEventListener("keydown", this.onKey);
    if (this.listening) this.listening.abort();
    // (Not what's kept on screen for a turn into the next chapter: the next pager plays it.)
    this.book.querySelectorAll(".curl-layer:not(.carried), .page3d:not(.carried), .page-warp, .edge-hint").forEach((e) => e.remove());
    // The shared curl canvas stays in the book; it's left showing only while a turn into the
    // next chapter is waiting to play (the old page over the new one).
    if (sharedCurl && !this.book.__carry && sharedCurl.book === this.book) uncover();
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
    // Text beside a half-width picture needs room for a decent line (about 22 ems): in narrower
    // columns pictures stand on their own between the paragraphs instead.
    {
      const fs = parseFloat(getComputedStyle(this.article).fontSize) || 18;
      const col = (this.width - (this.spread ? 4 : 2) * pad) / (this.spread ? 2 : 1);
      this.article.classList.toggle("narrow-cols", col * 0.54 < fs * 22);
    }
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
    this.dropScenes();
    this.shownCache = new Map();
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

  // A soft tint over the page margin under the pointer: there's a page to turn there.
  edgeHint(side) {
    let el = this.book.querySelector(":scope > .edge-hint");
    if (!el) {
      el = document.createElement("div");
      el.className = "edge-hint";
      this.book.appendChild(el);
    }
    const can = side === "right" ? this.page + 1 < this.pages || !!this.h.onNext : side === "left" ? this.page > 0 || !!this.h.onPrev : false;
    el.style.width = `${this.marginBand()}px`;
    el.dataset.side = can ? side : "";
  }

  // The picture of the neighbouring chapter's first page (dir 1) or last (dir -1), for dragging
  // a page over the end of this one; made in idle time (see warm), null until then.
  neighbourPic(dir) {
    const n = this.neighbours && this.neighbours[dir];
    return n && n.key === this.lookKey() && n.html === (this.book.__neighbours || {})[dir] ? n.canvas : null;
  }

  async makeNeighbourPic(dir) {
    const html = (this.book.__neighbours || {})[dir];
    if (!html || !canDrawElements) return;
    this.neighbours = this.neighbours || {};
    const have = this.neighbours[dir];
    if (have && have.html === html && have.key === this.lookKey()) return;
    const entry = { html, key: this.lookKey(), canvas: null };
    this.neighbours[dir] = entry;
    try {
      entry.canvas = await elementSnapshot(this, 0, { html, last: dir < 0, chapterStep: dir });
    } catch (e) {
      console.warn("neighbouring page picture failed:", e);
    }
  }

  // The book's size, colours and type (what any page's picture depends on).
  lookKey() {
    const reader = this.book.closest(".reader");
    return `${this.width}x${this.book.clientHeight}|${reader ? reader.dataset.theme : ""}|${reader ? reader.getAttribute("style") : ""}`;
  }

  // What a page picture depends on besides its place: size, colours and type.
  snapKey() {
    const reader = this.book.closest(".reader");
    const now = this.article.querySelector(".s.now");
    // (And the word being read aloud, so a turn shows the page as it is at that moment.)
    const word = along.st && along.st.container === this.article ? `${along.st.n}:${along.st.shown}` : "";
    return `${this.width}x${this.book.clientHeight}|${reader ? reader.dataset.theme : ""}|${reader ? reader.getAttribute("style") : ""}|${now ? now.dataset.s : ""}|${word}`;
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

  // A picture's key without the sentence and word being read aloud: while the recording plays the word moves
  // on several times a second, and a picture a word or two behind looks the same mid-turn. (Held
  // to the exact word, every picture went stale while it was being made, and the read-along's
  // page turns happened with no animation.)
  static steady(key) {
    // (And without the sentence being read: the recording reaches the next one just as often
    // right at the page's end.)
    return key.split("|").slice(0, -2).join("|");
  }

  snap(offset) {
    const s = this.snaps.get(offset);
    return s && s.canvas && s.sig === this.pageSig(offset) && Pager.steady(s.key) === Pager.steady(this.snapKey()) ? s.canvas : null;
  }

  // Pictures of these pages (by offset), making any that are missing.
  async pictures(offsets) {
    const key = this.snapKey();
    const out = [];
    for (const offset of offsets) {
      let entry = this.snaps.get(offset);
      const sig = this.pageSig(offset);
      // (Made again when the page itself has changed, or the word read aloud has, unless a
      // turn is waiting on it: then one a word behind will do.)
      const stale = entry && (Pager.steady(entry.key) !== Pager.steady(key) || (entry.key !== key && !this.busy));
      if (!entry || stale || entry.sig !== sig) {
        entry = { key, sig, canvas: null };
        entry.promise = elementSnapshot(this, offset).then((c) => (entry.canvas = c));
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
    if (!this.curl) return;
    clearTimeout(this.warmTimer);
    if (this.noGL) {
      // Lays out the strips for the next turn either way, a little at a time while it's quiet.
      const sigmas = this.spread ? [1, -1] : [1];
      const step = (deadline) => {
        if (this.dead || this.busy) return (this.warmTimer = setTimeout(() => this.warm(), 400));
        const budget = Math.max(8, Math.min(30, deadline && deadline.timeRemaining ? deadline.timeRemaining() : 12));
        for (const sg of sigmas) if (!this.fillScene(this.scene(sg), budget)) return idle(step);
        if (this.spread) this.stillLayer(0, this.page * this.width).remove();
        // ...and works out what shows on this page and the ones either side.
        for (const p of [this.page - 1, this.page, this.page + 1]) if (p >= 0 && p < this.pages) this.shownOn(p * this.width);
      };
      const idle = (cb) => (window.requestIdleCallback ? requestIdleCallback(cb, { timeout: 500 }) : setTimeout(cb, 30));
      this.warmTimer = setTimeout(() => idle(step), 150);
      return;
    }
    this.warmTimer = setTimeout(async () => {
      for (const p of [this.page, this.page + 1, this.page - 1]) {
        if (this.dead || this.noGL || p < 0 || p >= this.pages) continue;
        await this.pictures([p * this.width]);
        await new Promise((r) => (window.requestIdleCallback ? requestIdleCallback(r, { timeout: 300 }) : setTimeout(r, 30)));
      }
      // At either end of the chapter, the neighbouring chapter's page too.
      if (!this.dead && this.page + 1 >= this.pages) await this.makeNeighbourPic(1);
      if (!this.dead && this.page === 0) await this.makeNeighbourPic(-1);
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
    this.book.__userTurn = performance.now();
    this.atEnd = false;
    if (this.busy) { this.queued = 1; return; }
    if (this.page + 1 < this.pages) this.turn(1);
    else if (this.h.onNext) this.carryInto(1, this.h.onNext);
  }

  prev() {
    this.book.__userTurn = performance.now();
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
    const kept = this.article.cloneNode(true);
    kept.__furniture = this.furniture(this.page * this.width);
    this.book.__carry = {
      dir, snap: kept, at: this.page * this.width, spread: this.spread,
      picture: this.snap(this.page * this.width), key: this.lookKey(),
    };
    if (this.noGL && this.curl) {
      // Keep this page on screen (a copy of it) while the next chapter loads underneath, so the
      // new chapter doesn't show before the page has turned away.
      const at = this.page * this.width;
      const cover = this.layer("curl-cover carried", this.copy(at, kept));
      this.book.appendChild(cover);
      this.book.classList.add("turning");
      const carry = this.book.__carry;
      carry.cover = cover;
      // This chapter's strips, laid out already, turn the page over in the next chapter: aimed at
      // this page now, while it can still be measured. (One page turning back is the next
      // chapter's page, so those are made there.)
      const sigma = this.spread && dir < 0 ? -1 : 1;
      if (this.spread || dir > 0) {
        const scene = this.scene(sigma);
        this.fillScene(scene);
        this.aimScene(scene, at, at, !this.spread);
        for (const st of scene.strips) for (const fc of [st.front, st.back]) {
          fc.src = kept;
          fc.furnAt = undefined;
        }
        scene.el.classList.add("carried");
        this.scenes.delete(sigma);
        carry.scene = scene;
      }
      if (this.spread) {
        const still = this.stillLayer(dir > 0 ? 0 : this.width / 2, at);
        still.el.classList.add("carried");
        still.el.querySelectorAll(".furniture-copy").forEach((e) => e.remove());
        still.el.appendChild(kept.__furniture.cloneNode(true));
        this.still = null;
        carry.still = still.el;
      }
      setTimeout(() => {
        if (this.book.__carry !== carry) return;
        delete this.book.__carry;
        uncover();
        for (const e of [carry.scene && carry.scene.el, carry.still]) if (e) e.remove();
        this.book.classList.remove("turning");
      }, 6000);
    }
    // Keep this page on screen (as a picture) while the next chapter loads underneath, so the
    // new chapter doesn't show before the page has turned away.
    const pic = this.book.__carry.picture;
    if (pic && !this.noGL) {
      try {
        if (!sharedCurl || sharedCurl.lost) sharedCurl = new CurlGL(this.book);
        sharedCurl.attach(this.book);
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
    // (The highlight stays: the copy is the page exactly as it is on screen.) Cut down to the
    // page, as the whole chapter copied a few dozen times over takes too long to lay out.
    let c;
    if (from) {
      c = from.cloneNode(true);
      c.style.width = `${this.width}px`;
      c.style.transform = `translateX(${-offset}px)`;
    } else {
      c = pageCopy(this.article, offset, this.width, this.book.clientHeight);
    }
    // Lazy pictures in a fresh copy would start blank and pop in.
    c.querySelectorAll("img[loading]").forEach((e) => e.removeAttribute("loading"));
    const page = document.createDocumentFragment();
    // A kept page from the chapter just left has its own running head and number.
    page.append(c, from && from.__furniture ? from.__furniture.cloneNode(true) : this.furniture(offset));
    return page;
  }

  // The running head and page number as they read on the page at `offset` (they're beside the
  // text, not in it, so a copy of the page needs its own).
  // `flat`: for a page picture the curl turns, which is a flat sheet: no curved outline or fold
  // shading (they belong to the open book, not the page, and turned with it they looked like a
  // crease printed on the paper).
  furniture(offset, flat) {
    const out = document.createDocumentFragment();
    // The page's curved outline and (two pages) the fold down the middle go with it too.
    const warp = flat ? null : this.book.querySelector(":scope > .page-warp");
    if (warp) {
      const c = warp.cloneNode(true);
      c.classList.add("furniture-copy");
      out.appendChild(c);
    }
    if (this.spread && !flat) {
      const fold = document.createElement("div");
      fold.className = "fold-copy furniture-copy";
      out.appendChild(fold);
    }
    for (const sel of [":scope > .running-head", ":scope > .folio"]) {
      const el = this.book.querySelector(sel);
      if (!el) continue;
      const c = el.cloneNode(true);
      c.classList.add("furniture-copy");
      const pageEl = c.querySelector(".folio-page");
      if (pageEl) {
        pageEl.textContent = folioText(this.book, Math.round(offset / this.width) + 1, this.pages);
      }
      out.appendChild(c);
    }
    return out;
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
  begin(dir, carry, cross) {
    const t = this.beginGL(dir, carry, cross);
    if (cross) return t;
    if (t) return t;
    // The strips are only for browsers without the curl; elsewhere a missing picture means no
    // animation this time (the caller changes the page directly) rather than a different one.
    return this.noGL ? this.begin3d(dir, carry) : null;
  }

  // The curl on the graphics card, when pictures of both sides of the turning page are ready.
  beginGL(dir, carry, cross) {
    if (this.noGL) return null;
    const W = this.width, H = this.book.clientHeight, half = W / 2;
    const target = carry ? this.page : this.page + dir;
    const cur = carry ? carry.at : this.page * W;
    // (The kept page's picture fits if the book is the same size and look; what's highlighted
    // in the next chapter has nothing to do with it.)
    const curPic = carry ? (carry.key === this.lookKey() ? carry.picture : null) : this.snap(cur);
    // Dragged over the end of the chapter: the page beyond is the neighbouring chapter's.
    const nextPic = cross ? this.neighbourPic(dir) : this.snap(target * W);
    if (!curPic || !nextPic) return null;
    try {
      // One WebGL context for the whole app (browsers allow only a few), moved into this book.
      if (!sharedCurl || sharedCurl.lost) sharedCurl = new CurlGL(this.book);
      this.gl = sharedCurl;
      this.gl.attach(this.book);
    } catch (e) {
      console.warn("no WebGL page curl:", e);
      this.noGL = true;
      return null;
    }
    this.busy = true;
    // The canvas draws the whole book for the length of the turn (the turning page, the page it
    // uncovers and the facing page), from pictures; the live page underneath doesn't change
    // until the turn has landed, so nothing can show through early.
    const t = { dir, target, W, H, layers: [], gl: true, cross: !!cross };
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
    const target = carry ? this.page : this.page + dir;
    const next = target * W;
    const t = { dir, target, W, H, layers: [] };
    // Which page each face shows: the turning page's front, and its back (the next page where it
    // lands once turned, with two pages; this page seen through the paper, with one).
    let fOff, bOff, ghost = false;
    if (!this.spread) {
      // One page: forward, this page peels off to the left; back, the previous one is laid back down.
      t.leaf = [0, W];
      t.hinge = 0;
      t.single = true;
      t.C0x = W;
      fOff = bOff = dir > 0 ? cur : next;
      if (dir > 0 && !carry) t.placeTarget = true;
      ghost = true;
      t.reverse = dir < 0;
    } else {
      // Two pages: the right page turns over the spine (its back is the next left page), or
      // backwards, the left page turns over to the right (its back is the previous right page).
      t.leaf = dir > 0 ? [half, W] : [0, half];
      t.hinge = half;
      t.C0x = dir > 0 ? W : 0;
      fOff = cur;
      bOff = next;
      if (!carry) t.placeTarget = true;
    }
    t.sigma = t.C0x > t.hinge ? 1 : -1;
    // The strips: kept laid out between turns (a chapter's worth of text copied a few dozen times
    // takes a while to lay out); a page kept from the chapter just left gets its own.
    // (Turning into a new chapter, the faces showing the kept page are copies of that.)
    let scene;
    if (carry && carry.scene && carry.scene.key === this.sceneKey()) {
      // The last chapter's strips, aimed at its page already; the faces showing this chapter
      // are copies of just its page (quick to lay out).
      scene = carry.scene;
      if (this.spread) for (const st of scene.strips) this.pageFace(st.back, next, false);
    } else if (carry && t.single && dir < 0) {
      scene = this.makeScene(t.sigma);
      for (const st of scene.strips) {
        this.pageFace(st.front, next, false);
        this.pageFace(st.back, next, true);
      }
    } else if (carry) {
      const keptFront = !(t.single && dir < 0);
      scene = this.makeScene(t.sigma, keptFront ? carry.snap : this.article, keptFront && t.single ? carry.snap : this.article);
      this.aimScene(scene, fOff, bOff, ghost);
    } else {
      scene = this.scene(t.sigma);
      this.fillScene(scene);
      this.aimScene(scene, fOff, bOff, ghost);
    }
    t.strips = scene.strips;
    t.scene = scene.el;
    t.ghost = ghost;
    // A fine light-grey line round the turning page's free edges (not the bound one), so it reads
    // clearly against the page beneath it.
    const hingeLeft = t.hinge <= t.leaf[0] + 0.5;
    for (const st of scene.strips) for (const fc of [st.front, st.back]) {
      // (The back of a page that turns over is laid out mirrored about the spine.)
      const mirrored = fc === st.back && !ghost;
      const r = fc.rim.style;
      r.left = `${mirrored ? 2 * t.hinge - t.leaf[1] : t.leaf[0]}px`;
      r.width = `${t.leaf[1] - t.leaf[0]}px`;
      r.borderLeftWidth = hingeLeft !== mirrored ? "0" : "1px";
      r.borderRightWidth = hingeLeft !== mirrored ? "1px" : "0";
    }
    t.keepScene = !carry;
    // The page facing the turning one stays put (with two pages); with one page turning back
    // into the next chapter, the kept page lies underneath.
    if (this.spread) {
      t.layers.push(carry
        ? (carry.still || this.halfLayer(dir > 0 ? 0 : half, this.copy(cur, carry.snap)))
        : this.stillLayer(dir > 0 ? 0 : half, cur));
    } else if (dir < 0 && carry) {
      // The kept page lies underneath: the copy that's been covering the book.
      const under = carry.cover || this.layer("curl-under", this.copy(cur, carry.snap));
      under.classList.remove("curl-cover");
      under.classList.add("curl-under");
      t.layers.push(under);
    }
    // The live running head and page number make way for copies that read right for each page
    // while it turns: on the turning page itself, and on the page it uncovers.
    this.book.classList.add("turning");
    const underAt = !this.spread && dir < 0 && !carry ? cur : next;
    const furn = this.layer("curl-furniture", this.furniture(underAt));
    if (this.spread) furn.style.clipPath = `inset(0 ${W - t.leaf[1]}px 0 ${t.leaf[0]}px)`;
    t.layers.push(furn);
    // The page underneath darkens in the turning page's shadow.
    t.under = this.layer("curl-shadow");
    t.layers.push(t.under);
    for (const l of t.layers) if (!(l.el || l).isConnected) this.book.appendChild(l.el || l);
    // (The kept strips stay where they are in the book: moved, they'd be laid out all over again.
    // They're stacked above the other layers by z-index.)
    if (scene.el.parentNode !== this.book) this.book.appendChild(scene.el);
    scene.el.classList.remove("idle");
    for (const e of [scene.el, carry && carry.still]) if (e) e.classList.remove("carried");
    // Done with: a kept sheet is hidden away for the next turn, a one-off one goes.
    t.sceneLayer = t.keepScene ? { remove: () => scene.el.classList.add("idle") } : scene.el;
    t.sceneEl = scene.el;
    t.layers.push(t.sceneLayer);
    uncover();
    return t;
  }

  // A sheet of strips hinged at the spine, turning over to the left (sigma 1) or to the right
  // (-1): each strip turns a little further than the one before, so the page bends along its
  // width as it swings over, the free edge leading and the bound edge lying flat until the page
  // has to lift from it. Every face holds a whole copy of the chapter (made from `src`, or later
  // by fillScene), shifted to show the right page.
  makeScene(sigma, frontSrc, backSrc) {
    const W = this.width, H = this.book.clientHeight;
    const el = document.createElement("div");
    el.className = "page3d curl3d";
    const face = (cls) => {
      const f = document.createElement("div");
      f.className = cls;
      f.style.width = `${W}px`;
      f.style.height = `${H}px`;
      // Everything on the sheet, in the sheet's own coordinates (for a back face, reflected
      // across the lift line so the face itself is never drawn flipped over).
      const sheet = document.createElement("div");
      sheet.className = "cb-sheet";
      const content = document.createElement("div");
      content.className = "p3-content";
      const slice = document.createElement("div");
      slice.className = "p3-slice";
      slice.style.width = `${W}px`;
      content.appendChild(slice);
      const shade = document.createElement("div");
      shade.className = "curl-shade";
      const rim = document.createElement("div");
      rim.className = "curl-rim";
      sheet.append(content, shade, rim);
      f.appendChild(sheet);
      return { f, sheet, content, slice, shade, rim, x: 0 };
    };
    // Bands across the sheet, parallel to the line it lifts along: the flat part still lying on
    // the book, the bands bent round the roll, and the part lying flat again back up beyond it.
    const strips = [];
    for (let i = 0; i < BANDS; i++) {
      const front = face("cb-face");
      const back = face("cb-face cb-back");
      el.append(front.f, back.f);
      strips.push({ front, back });
    }
    const scene = { el, strips, sigma, key: this.sceneKey() };
    if (frontSrc) {
      for (const st of strips) {
        this.fillFace(st.front, frontSrc);
        this.fillFace(st.back, backSrc);
      }
      scene.full = true;
    }
    return scene;
  }

  // Which way a face shows its page: a front as it lies; the back of a page that turns over to
  // become the facing page mirrored about the spine (so it reads right once over); the back of a
  // single page is the page itself, faintly, through the paper.
  faceSide(fc, isBack, ghost) {
    // (The mirroring of a turned-over page is done by the face's own 3D placement, never inside
    // it: type drawn mirrored comes out lighter, and would change as the page lands.)
    fc.slice.style.transform = "";
    if (isBack) fc.f.classList.toggle("ghosted", !!ghost);
  }

  // What the kept strips depend on: the page size and one page or two (the text is the chapter's).
  sceneKey() {
    return `${this.width}x${this.book.clientHeight}|${this.spread}`;
  }

  scene(sigma) {
    this.scenes = this.scenes || new Map();
    let sc = this.scenes.get(sigma);
    if (!sc || sc.key !== this.sceneKey()) {
      if (sc) sc.el.remove();
      sc = this.makeScene(sigma);
      sc.el.classList.add("idle");
      this.book.appendChild(sc.el);
      this.scenes.set(sigma, sc);
    }
    return sc;
  }

  // Forgets the kept strips (the chapter was laid out again).
  dropScenes() {
    if (this.scenes) for (const sc of this.scenes.values()) sc.el.remove();
    this.scenes = new Map();
    this.still = null;
  }

  fillFace(fc, src) {
    const c = (src || this.article).cloneNode(true);
    c.querySelectorAll("img[loading]").forEach((e) => e.removeAttribute("loading"));
    c.style.width = `${this.width}px`;
    fc.slice.replaceChildren(c);
    fc.page = c;
    fc.src = src || this.article;
    fc.furniture = null;
    fc.furnAt = undefined;
  }

  // Points a face at one page of this chapter with a copy of just that page (made fresh).
  // `ghost`: the back of a single page, this page seen through the paper.
  pageFace(fc, offset, ghost) {
    const c = pageCopy(this.article, offset, this.width, this.book.clientHeight);
    c.querySelectorAll("img[loading]").forEach((e) => e.removeAttribute("loading"));
    const furn = this.furniture(offset);
    fc.furniture = [...furn.childNodes];
    fc.slice.replaceChildren(c, furn);
    fc.page = c;
    fc.src = this.article;
    fc.furnAt = offset;
    this.faceSide(fc, fc.f.classList.contains("cb-back"), ghost);
  }

  // Copies the chapter into any face still without it: all of them now (`budget` 0), or within
  // `budget` ms (laying out in idle time).
  fillScene(scene, budget) {
    if (scene.full) return true;
    const until = performance.now() + (budget || Infinity);
    for (const st of scene.strips) {
      for (const fc of [st.front, st.back]) {
        if (fc.page) continue;
        if (performance.now() > until) return false;
        this.fillFace(fc);
        // Laid out now, while it's quiet, not when the page starts to turn.
        if (budget) fc.page.getBoundingClientRect();
      }
    }
    scene.full = true;
    return true;
  }

  // Which of the chapter's blocks show on the page at `offset` (by their order in the chapter).
  // A copy hides the rest: they keep their place in the layout, but drawing a whole chapter for
  // every strip is what makes a turn slow to start.
  shownOn(offset) {
    this.shownCache = this.shownCache || new Map();
    const hit = this.shownCache.get(offset);
    if (hit && hit.key === this.sceneKey()) return hit.shown;
    const left = this.article.getBoundingClientRect().left;
    const shown = [...this.article.querySelectorAll(LEAF)].map((el) =>
      [...el.getClientRects()].some((r) => r.right - left > offset - 2 && r.left - left < offset + this.width + 2));
    this.shownCache.set(offset, { key: this.sceneKey(), shown });
    return shown;
  }

  showOnly(copy, offset) {
    if (copy.__shownAt === offset) return;
    const shown = this.shownOn(offset);
    copy.querySelectorAll(LEAF).forEach((el, i) => (el.style.visibility = shown[i] ? "" : "hidden"));
    copy.__shownAt = offset;
  }

  // Points every face of the strips at its page, with the highlight as it is now.
  aimScene(scene, fOff, bOff, ghost) {
    const nowIds = [...this.article.querySelectorAll(".s.now")].map((e) => e.dataset.s);
    const nowId = nowIds.join(",");
    for (const st of scene.strips) {
      for (const [fc, off, isBack] of [[st.front, fOff, false], [st.back, bOff, true]]) {
        const c = fc.page;
        if (!c) continue;
        this.faceSide(fc, isBack, ghost);
        c.style.transform = `translateX(${-off}px)`;
        if (fc.src === this.article) this.showOnly(c, off);
        if (c.__now !== nowId) {
          c.querySelectorAll(".s.now").forEach((e) => e.classList.remove("now"));
          for (const id of nowIds) c.querySelectorAll(`.s[data-s="${id}"]`).forEach((e) => e.classList.add("now"));
          c.__now = nowId;
        }
        // This page's running head and number, on the face (only where the face shows them).
        if (fc.furnAt !== off) {
          if (fc.furniture) fc.furniture.forEach((e) => e.remove());
          // (A page kept from the chapter just left has its own.)
          const furn = fc.src.__furniture ? fc.src.__furniture.cloneNode(true) : this.furniture(off);
          fc.furniture = [...furn.childNodes];
          fc.slice.appendChild(furn);
          fc.furnAt = off;
        }
      }
    }
  }

  // The facing page that stays put while the other turns: a kept copy too.
  stillLayer(x, offset) {
    if (!this.still || this.still.key !== this.sceneKey()) {
      if (this.still) this.still.el.remove();
      const el = this.layer("curl-static");
      const c = this.article.cloneNode(true);
      c.querySelectorAll("img[loading]").forEach((e) => e.removeAttribute("loading"));
      c.style.width = `${this.width}px`;
      el.appendChild(c);
      el.classList.add("idle");
      this.book.appendChild(el);
      this.still = { el, page: c, key: this.sceneKey() };
    }
    const { el, page } = this.still;
    page.style.transform = `translateX(${-offset}px)`;
    this.showOnly(page, offset);
    page.querySelectorAll(".s.now").forEach((e) => e.classList.remove("now"));
    for (const now of this.article.querySelectorAll(".s.now")) page.querySelectorAll(`.s[data-s="${now.dataset.s}"]`).forEach((e) => e.classList.add("now"));
    el.querySelectorAll(".furniture-copy").forEach((e) => e.remove());
    el.appendChild(this.furniture(offset));
    el.style.clipPath = `inset(0 ${this.width - x - this.width / 2}px 0 ${x}px)`;
    el.classList.remove("idle");
    // Taken off the page with the turn's other layers, but kept for the next one.
    return { remove: () => el.classList.add("idle"), el, keep: true };
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
    const F = { x: t.single ? t.hinge : 2 * t.hinge - t.C0x, y };
    return t.reverse ? [F, C] : [C, F];
  }

  pose(t, P) {
    if (t.gl) this.poseGL(t, P);
    else this.poseCurl(t, P);
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
    // (The open book's fold down the middle, on the pages lying flat: the pictures leave it out,
    // as the turning sheet mustn't carry it.)
    const dark = (this.book.closest(".reader") || {}).dataset?.theme === "dark";
    const fold = this.spread ? [W / 2, dark ? 0.35 : 1] : null;
    const flat = { x0: t.leaf[0], w, W, H, hinge: t.hinge, ghost: !!t.ghost, paper: t.paper, squash: 1, under: true, still: t.still, fold };
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
  // Where the page is held (P) sets the curl, worked out as on the graphics card (see poseGL):
  // a lift line, and a cylinder of radius R the sheet rolls over beyond it. The sheet is drawn
  // in bands parallel to the line, each a copy of the page clipped to its band and placed in 3D
  // on the chord of the roll it spans, so the bands meet edge to edge.
  curlGeometry(t, P) {
    const { W, H } = t;
    const C = { x: t.C0x, y: t.C.y };
    const w = t.leaf[1] - t.leaf[0];
    const hp = { x: t.hinge, y: C.y };
    let vx = P.x - hp.x, vy = P.y - hp.y;
    const r = Math.hypot(vx, vy);
    if (r > w) { vx *= w / r; vy *= w / r; }
    P = { x: hp.x + vx, y: hp.y + vy };
    let d = Math.hypot(P.x - C.x, P.y - C.y);
    if (d < 0.5) return null;
    let n = { x: (P.x - C.x) / d, y: (P.y - C.y) / d };
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
    const now = performance.now();
    if (t.lastP) {
      const speed = Math.hypot(P.x - t.lastP.x, P.y - t.lastP.y) / Math.max(1, now - t.lastT);
      t.flex = (t.flex || 0) * 0.8 + Math.min(w * 0.12, speed * 22) * 0.2;
    }
    t.lastP = P;
    t.lastT = now;
    const most = (2 * toSpine - d) / Math.PI;
    let R = Math.min(w * 0.35, 8 + d * 0.12 + w * 0.18 * Math.sin(Math.PI * turned) + (t.flex || 0));
    const tail = t.single ? Math.max(0, Math.min(1, (turned - 0.7) / 0.3)) : 0;
    // (Landed, the roll closes to nothing, so the sheet lies exactly on the facing page and the
    // copy on it matches the page underneath to the pixel when it's swapped for the real one.)
    R = Math.max(0.001, Math.min(most, R + (most - R) * tail));
    const sC = (d + Math.PI * R) / 2;
    const L0 = { x: C.x + n.x * sC, y: C.y + n.y * sC };
    const upright = t.single ? Math.max(0, Math.min(1, (turned - 0.8) / 0.2)) : 0;
    const p = Math.min(1, Math.abs(P.x - C.x) / span);
    return { L0, u: { x: -n.x, y: -n.y }, R, squash: 1 - 0.98 * upright, turned, progress: t.reverse ? 1 - p : p };
  }

  poseCurl(t, P) {
    const { W, H } = t;
    const g = this.curlGeometry(t, P);
    const bands = t.strips;
    const leaf = [[t.leaf[0], 0], [t.leaf[1], 0], [t.leaf[1], H], [t.leaf[0], H]];
    const show = (fc, on) => { if (fc.on !== on) { fc.f.style.visibility = on ? "" : "hidden"; fc.on = on; } };
    if (!g) {
      // Lying flat: the whole sheet as it rests, front up.
      bands.forEach((b, i) => {
        show(b.back, false);
        show(b.front, i === 0);
        if (i === 0) {
          b.front.f.style.transform = "none";
          b.front.f.style.clipPath = `polygon(${leaf.map(([x, y]) => `${x}px ${y}px`).join(",")})`;
          b.front.shade.style.background = "none";
        }
      });
      t.under.style.opacity = "0";
      t.progress = 0;
      this.revealUnder(t);
      return;
    }
    const { L0, u, R, squash } = g;
    t.progress = g.progress;
    const roll = Math.PI * R;
    const c = u.x * L0.x + u.y * L0.y;
    const h = t.hinge;
    // Band edges along the sheet (s, the distance past the lift line): the flat part, the roll
    // in equal chords, and the far flat part (out past any corner).
    const nRoll = BANDS - 2;
    const far = 4 * (W + H);
    const edges = [-far, 0];
    for (let i = 1; i <= nRoll; i++) edges.push((roll * i) / nRoll);
    edges.push(far);
    // Where a point s along the sheet ends up: across (x) and up (z).
    const at = (s) => (s <= 0 ? [s, 0] : s < roll ? [R * Math.sin(s / R), R * (1 - Math.cos(s / R))] : [roll - s, 2 * R]);
    // The leaf clipped to a <= s <= b.
    const clip = (a, b) => {
      let poly = leaf;
      for (const [k, sign] of [[a, 1], [b, -1]]) {
        const out = [];
        const f = ([x, y]) => sign * (u.x * x + u.y * y - c - k);
        for (let i = 0; i < poly.length; i++) {
          const p0 = poly[i], p1 = poly[(i + 1) % poly.length];
          const f0 = f(p0), f1 = f(p1);
          if (f0 >= 0) out.push(p0);
          if ((f0 >= 0) !== (f1 >= 0)) {
            const m = f0 / (f0 - f1);
            out.push([p0[0] + (p1[0] - p0[0]) * m, p0[1] + (p1[1] - p0[1]) * m]);
          }
        }
        poly = out;
        if (!poly.length) break;
      }
      return poly;
    };
    const frontShade = (th) => 0.42 * (1 - Math.max(0, Math.cos(th))) ** 1.5;
    const backTone = (th) => {
      const dark = 0.4 * (Math.max(0, 1 + Math.cos(th)) / 2) ** 1.5;
      const gloss = 0.3 * Math.exp(-(((th - 2.2) / 0.41) ** 2));
      if (Math.abs(gloss - dark) < 0.004) return "transparent";
      return gloss > dark ? `rgba(255,255,255,${(gloss - dark).toFixed(3)})` : `rgba(0,0,0,${(dark - gloss).toFixed(3)})`;
    };
    for (let i = 0; i < BANDS; i++) {
      const b = bands[i];
      const s0 = edges[i], s1 = edges[i + 1];
      // (Each band reaches a little into its neighbours, so no hairline seam shows between them.)
      // (A band the roll has closed to nothing is left out: drawn as a hairline turned edge-on,
      // it would have the browser resample the whole sheet, softening it.)
      // (The overlap is measured on screen: a band turned nearly edge-on needs more of it.)
      const chord = s1 <= 0 || s0 >= roll ? 1 : Math.abs(Math.cos(Math.max(0, Math.min(Math.PI, (s0 + s1) / 2 / R))));
      const lap = Math.min(8, 1.2 / Math.max(0.15, chord));
      const poly = s1 - s0 > 0.5 ? clip(s0 - lap, s1 + lap) : [];
      if (poly.length < 3) {
        show(b.front, false);
        show(b.back, false);
        continue;
      }
      // The band's chord: x and z change linearly with s between its two edges.
      const a0 = at(Math.max(0, Math.min(roll, s0))), a1 = at(Math.max(0, Math.min(roll, s1)));
      let kx, kz, x0, z0, sRef;
      if (s1 <= 0) { kx = 1; kz = 0; x0 = 0; z0 = 0; sRef = 0; }
      else if (s0 >= roll) { kx = -1; kz = 0; x0 = 0; z0 = 2 * R; sRef = roll; }
      else { kx = (a1[0] - a0[0]) / (s1 - s0); kz = (a1[1] - a0[1]) / (s1 - s0); x0 = a0[0]; z0 = a0[1]; sRef = s0; }
      // X -> X + u((kx - 1) s + x0 - sRef kx) + z (kz s + z0 - sRef kz), with s = u.X - c,
      // then (one page, at the end) squashed toward the spine.
      const A = -(kx - 1) * c + x0 - sRef * kx, B = -kz * c + z0 - sRef * kz;
      const q = s1 <= 0 ? 1 : squash;
      let M = [
        [q * (1 + u.x * (kx - 1) * u.x), q * u.x * (kx - 1) * u.y, 0, q * u.x * A + h * (1 - q)],
        [u.y * (kx - 1) * u.x, 1 + u.y * (kx - 1) * u.y, 0, u.y * A],
        [kz * u.x, kz * u.y, 1, B],
        [0, 0, 0, 1],
      ];
      // Which side faces you: the front while the band's slope across is still forward. A back
      // face is drawn reflected across the lift line and placed by M after that reflection, so
      // it reads the right way round without the element itself being turned over.
      const front = kx > 0;
      const fc = front ? b.front : b.back;
      // Painted from the bottom up: the flat page, then round the roll as it rises, then the part
      // lying back over the top (one place in the stack per band; bands only ever overlap their
      // neighbours by the hair that hides the seam).
      fc.f.style.zIndex = String(i + 1);
      show(b.front, front);
      show(b.back, !front);
      let poly2 = poly;
      let gU = u, gL = L0;
      if (!front && !t.ghost) {
        // The back of a page turning over: its content lies mirrored about the spine, and that
        // mirroring is folded into the placement (M after the mirror), which then keeps the face
        // the right way up: drawn plainly, and exactly the page once it has landed.
        const Mh = [[-1, 0, 0, 2 * h], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]];
        M = M.map((row) => [0, 1, 2, 3].map((j) => row.reduce((acc, v, k) => acc + v * Mh[k][j], 0)));
        poly2 = poly.map(([x, y]) => [2 * h - x, y]);
        gU = { x: -u.x, y: u.y };
        gL = { x: 2 * h - L0.x, y: L0.y };
        if (fc.sheet.style.transform) fc.sheet.style.transform = "";
      } else if (!front) {
        const Rl = [
          [1 - 2 * u.x * u.x, -2 * u.x * u.y, 0, 2 * c * u.x],
          [-2 * u.x * u.y, 1 - 2 * u.y * u.y, 0, 2 * c * u.y],
          [0, 0, 1, 0],
          [0, 0, 0, 1],
        ];
        M = M.map((row) => [0, 1, 2, 3].map((j) => row.reduce((acc, v, k) => acc + v * Rl[k][j], 0)));
        poly2 = poly.map(([x, y]) => [x - 2 * u.x * (u.x * x + u.y * y - c), y - 2 * u.y * (u.x * x + u.y * y - c)]);
        fc.sheet.style.transform = `matrix(${Rl[0][0].toFixed(5)},${Rl[1][0].toFixed(5)},${Rl[0][1].toFixed(5)},${Rl[1][1].toFixed(5)},${Rl[0][3].toFixed(3)},${Rl[1][3].toFixed(3)})`;
      } else if (fc.sheet.style.transform) {
        fc.sheet.style.transform = "";
      }
      const mat = [0, 1, 2, 3].flatMap((j) => [0, 1, 2, 3].map((i) => M[i][j]));
      // Lying flat (at rest, or landed), a face goes exactly where the page is: any hair of
      // depth or fraction of a pixel would have it filtered, a shade softer than the page.
      const flat = Math.abs(M[0][0] - 1) < 1e-3 && Math.abs(M[1][1] - 1) < 1e-3 && Math.abs(M[0][1]) < 1e-3 &&
        Math.abs(M[1][0]) < 1e-3 && Math.abs(M[2][0]) < 1e-3 && Math.abs(M[2][1]) < 1e-3 && Math.abs(M[2][3]) < 0.05;
      fc.f.style.transform = flat
        ? `translate(${Math.round(M[0][3])}px, ${Math.round(M[1][3])}px)`
        : `matrix3d(${mat.map((v) => +v.toFixed(5)).join(",")})`;
      // The outline shows while the page is in the air, not once it lies flat at either end.
      const rimA = Math.min(1, 5 * Math.sin(Math.PI * Math.max(0, Math.min(1, g.progress)))).toFixed(2);
      if (fc.rim.style.opacity !== rimA) fc.rim.style.opacity = rimA;
      fc.f.style.clipPath = `polygon(${poly2.map(([x, y]) => `${x.toFixed(2)}px ${y.toFixed(2)}px`).join(",")})`;
      // Light: as on the graphics card, by the angle round the roll at each edge of the band.
      const th0 = Math.max(0, Math.min(Math.PI, s0 / R)), th1 = Math.max(0, Math.min(Math.PI, s1 / R));
      if (s1 <= 0) fc.shade.style.background = "none";
      else if (front) {
        fc.shade.style.background = lineGradient(W, H, u, L0, [[s0, `rgba(0,0,0,${frontShade(th0).toFixed(3)})`], [s1, `rgba(0,0,0,${frontShade(th1).toFixed(3)})`]]);
      } else {
        fc.shade.style.background = s0 >= roll ? backTone(Math.PI) : lineGradient(W, H, gU, gL, [[s0, backTone(th0)], [s1, backTone(th1)]]);
      }
    }
    // The sheet's shadow on the page it uncovers, deepest just past the roll.
    const lift = Math.sin(Math.PI * Math.min(1, g.turned));
    const reach = Math.min(170, 30 + roll * 0.5 + 40 * lift);
    const strength = 0.4 * lift + 0.06;
    t.under.style.opacity = "1";
    t.under.style.background = lineGradient(W, H, u, { x: L0.x + u.x * R, y: L0.y + u.y * R }, [
      [-2, "rgba(0,0,0,0)"], [0, `rgba(0,0,0,${strength.toFixed(3)})`], [reach, "rgba(0,0,0,0)"],
    ]);
    t.under.style.clipPath = `inset(0 ${W - t.leaf[1]}px 0 ${t.leaf[0]}px)`;
    this.revealUnder(t);
  }

  animate(t, from, to, ms, arc) {
    return new Promise((resolve) => {
      const start = performance.now();
      const ease = (x) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2);
      const step = (now) => {
        // (A frame's timestamp can be a little before the animation started: never run it backwards.)
        const x = Math.max(0, Math.min(1, (now - start) / ms));
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
    if (t.cross && done) {
      // Into the neighbouring chapter: the landed picture stays on screen while it's laid out,
      // and gives way to it (see the constructor).
      const carry = { landed: true, dir: t.dir };
      this.book.__carry = carry;
      setTimeout(() => { if (this.book.__carry === carry) { delete this.book.__carry; uncover(); } }, 6000);
      this.book.classList.remove("turning");
      t.dir > 0 ? this.h.onNext() : this.h.onPrev();
      return;
    }
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
    // The live running head and number have caught up (the reader redraws them straight away),
    // so the copies go in the same frame the sheet comes to rest: nothing lingers.
    await Promise.resolve();
    t.layers.forEach((e) => e.remove());
    this.book.classList.remove("turning");
    this.busy = false;
    if (this.hover) this.hover();
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

  // The page curl runs only on the graphics card (see elementSnapshot); a browser that can't
  // do that turns pages without an animation.
  get curl() {
    return canDrawElements && this.book.__curl !== false && !document.documentElement.classList.contains("eink");
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

  // How far in from the book's outer edges a click or grab turns the page: the blank margin,
  // stopping just short of the text.
  marginBand() {
    const pad = parseFloat(this.article.style.getPropertyValue("--page-pad")) || 40;
    return Math.max(18, pad - 6);
  }

  bindPointer() {
    // Every listener goes when this pager does (the book itself stays for the next chapter's).
    this.listening = new AbortController();
    const on = (type, fn, opts) =>
      this.book.addEventListener(type, fn, { ...(typeof opts === "boolean" ? { capture: opts } : opts || {}), signal: this.listening.signal });
    let start = null;
    let drag = null;
    // Where a mouse can take hold of the page: the outer part of the page being turned (the rest
    // of the page selects text as usual). A finger can take hold anywhere.
    const grabZone = (e) => {
      if (e.pointerType === "touch") return true;
      // The page's outer margins (and a little into them from the text), like the edge of a page
      // you'd pinch to turn.
      const x = e.clientX - this.book.getBoundingClientRect().left;
      const band = this.marginBand();
      return x > this.width - band || x < band;
    };
    on("pointerdown", (e) => {
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
    // The hand and margin tint where the mouse is; worked out again when a turn ends, so they
    // come back without the mouse having to move. (Kept on the book: a turn into the next
    // chapter ends in the next chapter's pager.)
    this.hover = () => {
      const m = this.book.__mouse;
      if (!m || this.busy) {
        this.book.classList.remove("grab-edge");
        this.edgeHint("");
        return;
      }
      const on = grabZone(m);
      this.book.classList.toggle("grab-edge", this.curl && on);
      this.edgeHint(on ? (m.clientX - this.book.getBoundingClientRect().left < this.width / 2 ? "left" : "right") : "");
    };
    on("pointerleave", (e) => {
      if (e.pointerType !== "mouse") return;
      this.book.__mouse = null;
      this.hover();
    });
    on("pointermove", (e) => {
      if (e.pointerType === "mouse") this.book.__mouse = { clientX: e.clientX, clientY: e.clientY, pointerType: "mouse" };
      if (!start) {
        if (e.pointerType === "mouse") this.hover();
        return;
      }
      const dx = e.clientX - start.x;
      // (Measured once per drag: reading the layout while the sheet is being restyled every move
      // would make the browser work it all out again each time.)
      const rect = (drag && drag.rect) || this.book.getBoundingClientRect();
      if (!drag) {
        if (!this.curl || this.busy || !start.grab || Math.abs(dx) < 6 || Math.abs(dx) < Math.abs(e.clientY - start.y)) return;
        const dir = dx < 0 ? 1 : -1;
        const atEdge = (dir > 0 && this.page + 1 >= this.pages) || (dir < 0 && this.page === 0);
        const neighbour = atEdge && (dir > 0 ? this.h.onNext : this.h.onPrev) && this.neighbourPic(dir);
        if (atEdge && !neighbour) {
          // At the chapter's edge with the next chapter's page not ready to show: a pull far
          // enough turns into it when you let go (never before).
          start.cross = dir;
          start.crossDx = dx;
          window.getSelection().removeAllRanges();
          return;
        }
        window.getSelection().removeAllRanges();
        if (!this.noGL && (!this.snap(this.page * this.width) || (!atEdge && !this.snap((this.page + dir) * this.width)))) {
          // Pictures still being made: take hold once they're ready (a moment).
          this.pictures(atEdge ? [this.page * this.width] : [this.page * this.width, (this.page + dir) * this.width]);
          return;
        }
        drag = this.begin(dir, null, atEdge ? dir : 0);
        if (!drag) return;
        this.book.__userTurn = performance.now();
        const y = Math.max(0, Math.min(drag.H, start.y - rect.top));
        drag.C = { x: drag.C0x, y };
        drag.G = { x: start.x - rect.left, y };
        drag.rect = rect;
        const [rest] = this.ends(drag, y);
        drag.from = rest;
        this.book.classList.add("grab-edge");
        try { this.book.setPointerCapture(e.pointerId); } catch (_) {}
      }
      let P = { x: drag.from.x + dx, y: drag.from.y + (e.clientY - start.y) };
      if (!drag.reverse) {
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
      // (The browser already delivers mouse moves once a frame.)
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
    on("wheel", (e) => {
      if (Math.abs(e.deltaX) <= Math.abs(e.deltaY)) return;
      e.preventDefault();
      // A mouse's sideways wheel clicks in steps a fraction of a second apart: each step is a
      // whole turn, played out (following it like a finger made it stop and start, half turned
      // and fading, at every step). A trackpad's smooth swipe moves the page with the fingers.
      const stepped = e.deltaMode !== 0 || (Math.abs(e.deltaX) >= 60 && Number.isInteger(e.deltaX) && !swipe);
      if (stepped) {
        const now = performance.now();
        if (this.busy || now - (this.wheelTurnAt || 0) < 350) return;
        this.wheelTurnAt = now;
        e.deltaX > 0 ? this.next() : this.prev();
        return;
      }
      if (!swipe) {
        if (this.busy || !this.curl) return;
        const dir = e.deltaX > 0 ? 1 : -1;
        const atEdge = (dir > 0 && this.page + 1 >= this.pages) || (dir < 0 && this.page === 0);
        // (Over the end of the chapter, the page beyond is the neighbouring chapter's, when it's
        // ready; otherwise the swipe turns into that chapter once the fingers stop.)
        if (atEdge && !((dir > 0 ? this.h.onNext : this.h.onPrev) && this.neighbourPic(dir))) {
          swipe = { cross: dir, total: 0 };
        } else {
          if (!this.noGL && (!this.snap(this.page * this.width) || (!atEdge && !this.snap((this.page + dir) * this.width)))) {
            this.pictures(atEdge ? [this.page * this.width] : [this.page * this.width, (this.page + dir) * this.width]);
            return;
          }
          const t = this.begin(dir, null, atEdge ? dir : 0);
          if (!t) return;
          this.book.__userTurn = performance.now();
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
      // (The fingers have stopped, or lifted: a pause in the events a little longer than a
      // trackpad leaves between them.)
      swipe.timer = setTimeout(endSwipe, 300);
    }, { passive: false });
    on("pointerleave", () => {
      if (!drag) this.book.classList.remove("grab-edge");
      this.edgeHint("");
    });
    on("pointerup", release);
    on("pointercancel", release);
    // Double-click selects a word (the browser's own), drag selects any stretch of text, and a
    // triple-click selects the whole sentence (rather than the paragraph).
    on("click", (e) => {
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
    on(
      "click",
      (e) => {
        if (this.swallowClick) {
          this.swallowClick = false;
          e.stopPropagation();
          e.preventDefault();
          return;
        }
        if (e.target.closest("a[data-href]")) return;
        // Only the blank outer margin turns the page: a click on the text, however near the
        // edge, is for the words.
        const x = e.clientX - this.book.getBoundingClientRect().left;
        const edge = this.marginBand();
        const onEdge = x < edge || x > this.width - edge;
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
// The neighbouring chapters' text (HTML), so a page can be dragged over into them.
export function pagerSetNeighbours(book, next, prev) {
  book.__neighbours = { 1: next || null, [-1]: prev || null };
  const p = book.__pager;
  if (p && !p.dead) p.warm();
}

// Page numbers across the whole book: each chapter laid out once, out of sight, exactly as the
// page is now (same size, columns and type), and its pages counted. Kept per look, so a
// different size or type is counted afresh.
const chapterPages = new Map();
export function countChapterPages(book, chapter, html) {
  const p = book && book.__pager;
  if (!p || p.dead) return 0;
  const key = p.lookKey();
  let counts = chapterPages.get(key);
  if (!counts) chapterPages.set(key, (counts = new Map()));
  if (counts.has(chapter)) return counts.get(chapter);
  const copy = p.article.cloneNode(false);
  copy.innerHTML = html;
  copy.removeAttribute("id");
  copy.style.position = "absolute";
  copy.style.left = "0";
  copy.style.top = "0";
  copy.style.visibility = "hidden";
  copy.style.pointerEvents = "none";
  copy.style.transform = "none";
  copy.setAttribute("aria-hidden", "true");
  p.book.appendChild(copy);
  const n = Math.max(1, Math.round(copy.scrollWidth / p.width));
  copy.remove();
  counts.set(chapter, n);
  return n;
}

// Pictures drawn while the chapter's open, put in as they arrive: each before the paragraph its
// sentence starts in, and the pages laid out again round the place being read.
// Whether a picture before this sentence's paragraph would sit too close to another: two
// floats side by side squeeze the text between them into a sliver. At least two paragraphs (and
// a few lines) of text between pictures.
function crowded(container, span) {
  let block = span.closest("p, blockquote, li, div");
  if (!block || block === container) return false;
  let n = 0, prev = block.previousElementSibling;
  let chars = 0;
  while (prev && n < 12) {
    if (prev.matches("figure.illus")) return true;
    chars += prev.textContent.length;
    if (chars > 2000) return false;
    prev = prev.previousElementSibling;
    n++;
  }
  // (And the one after it.)
  const next = block.nextElementSibling;
  return !!(next && next.matches("figure.illus"));
}

export function addPictures(container, list) {
  if (!container) return 0;
  if (typeof list === "string") list = JSON.parse(list);
  let added = 0;
  for (const p of list || []) {
    if (container.querySelector(`figure.illus[data-for="${p.sentence}"]`)) continue;
    const span = container.querySelector(`.s[data-s="${p.sentence}"]`);
    if (!span) continue;
    // Not on the page being read: it would move the words under the reader's eyes. (It goes in
    // once they've turned past.)
    if (sentenceOnPage(container, p.sentence)) continue;
    if (crowded(container, span)) continue;
    let block = span.closest("p, blockquote, li, h1, h2, h3, h4, h5, h6, div");
    if (!block || !container.contains(block) || block === container) block = span;
    const fig = document.createElement("figure");
    fig.className = "illus";
    fig.dataset.for = p.sentence;
    const img = document.createElement("img");
    img.src = p.url;
    img.alt = p.caption || "";
    img.width = 1216;
    img.height = 832;
    // (Picture and caption in one unbreakable box: a float split at a column's foot left its
    // caption at the top of the next column, over the text there.)
    const inner = document.createElement("span");
    inner.className = "illus-in";
    const cap = document.createElement("span");
    cap.className = "illus-cap";
    cap.textContent = p.caption || "";
    inner.append(img, cap);
    fig.append(inner);
    block.parentNode.insertBefore(fig, block);
    added++;
  }
  if (added) {
    const pager = pagerOf(container);
    if (pager) pager.relayoutSoon();
  }
  return added;
}

// Where this chapter's pages start in the whole book, and how many the book has (0: not known
// yet), for the page numbers on the page and on its pictures.
export function pagerSetBookPages(book, base, total) {
  if (!book) return;
  book.__bookPages = total > 0 ? { base, total } : null;
}

// "Page 41 of 213 · 3 pages left in chapter" (or within the chapter, until the book is counted).
function folioText(book, p, n) {
  // (Manga is laid out last page first.)
  if (book && book.__rtl) p = n - p + 1;
  const left = n - p;
  const rest = left === 0 ? "last page in chapter" : left === 1 ? "1 page left in chapter" : `${left} pages left in chapter`;
  const b = book && book.__bookPages;
  return b ? `Page ${b.base + p} of ${b.total} · ${rest}` : `Page ${p} of ${n} · ${rest}`;
}

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

// The book's binding: the dominant colour of its cover picture (made dark enough to be a cloth
// or leather binding), or a colour picked from its title when there's no picture.
export function bookCover(book, title, coverUrl) {
  if (!book) return;
  let h = 2166136261;
  for (const ch of title || "") {
    h ^= ch.codePointAt(0);
    h = Math.imul(h, 16777619);
  }
  book.style.setProperty("--cover", COVER_COLOURS[(h >>> 0) % COVER_COLOURS.length]);
  if (!coverUrl) return;
  const img = new Image();
  img.crossOrigin = "anonymous";
  img.onload = () => {
    const c = dominantColour(img);
    if (c) book.style.setProperty("--cover", c);
  };
  img.src = coverUrl;
}

// The colour that most of a picture is (ignoring white margins and black borders), counting
// colourful pixels for more than grey ones; then set as a binding would be: deep, not garish.
function dominantColour(img) {
  try {
    const S = 48;
    const cv = document.createElement("canvas");
    cv.width = S;
    cv.height = S;
    const ctx = cv.getContext("2d", { willReadFrequently: true });
    ctx.drawImage(img, 0, 0, S, S);
    const px = ctx.getImageData(0, 0, S, S).data;
    const bins = new Map();
    for (let i = 0; i < px.length; i += 4) {
      const [r, g, b] = [px[i], px[i + 1], px[i + 2]];
      const max = Math.max(r, g, b), min = Math.min(r, g, b);
      const l = (max + min) / 510;
      if (l > 0.93 || l < 0.05) continue;
      const sat = max === min ? 0 : (max - min) / (255 - Math.abs(max + min - 255));
      const key = ((r >> 5) << 6) | ((g >> 5) << 3) | (b >> 5);
      const e = bins.get(key) || { w: 0, r: 0, g: 0, b: 0 };
      const w = 0.25 + sat;
      e.w += w; e.r += r * w; e.g += g * w; e.b += b * w;
      bins.set(key, e);
    }
    let best = null;
    for (const e of bins.values()) if (!best || e.w > best.w) best = e;
    if (!best) return null;
    let [r, g, b] = [best.r / best.w, best.g / best.w, best.b / best.w].map((v) => v / 255);
    // To HSL, deepen, back.
    const max = Math.max(r, g, b), min = Math.min(r, g, b);
    let H = 0, Sat = 0, L = (max + min) / 2;
    if (max !== min) {
      const d = max - min;
      Sat = L > 0.5 ? d / (2 - max - min) : d / (max + min);
      H = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
      H *= 60;
    }
    L = Math.min(0.4, Math.max(0.2, L * 0.7));
    Sat = Math.min(0.62, Sat);
    return `hsl(${H.toFixed(0)} ${(Sat * 100).toFixed(0)}% ${(L * 100).toFixed(0)}%)`;
  } catch {
    return null;
  }
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

// E-ink display mode: black-and-white chrome, no animations (no page curl), pictures still in
// colour. Remembered on this device; until you choose, it's on for screens that say they
// redraw slowly (e-ink), off elsewhere.
export function einkOn() {
  let saved = null;
  try { saved = localStorage.getItem("duet-eink"); } catch (_) {}
  if (saved === "on" || saved === "off") return saved === "on";
  return !!(window.matchMedia && matchMedia("(update: slow)").matches);
}

export function setEink(on) {
  document.documentElement.classList.toggle("eink", on);
  try { localStorage.setItem("duet-eink", on ? "on" : "off"); } catch (_) {}
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

// ---------------------------------------------------------------------------------------------
// Read-along: while the recording plays, the paragraph being read is tinted softly, the sentence
// (the .s.now highlight) more, and the word being said most. The word follows the recording
// frame by frame, from the sync's word times when it has them (or spread over the sentence by
// length when it doesn't). Drawn with the CSS Custom Highlight API, so the text isn't touched.
const WORD_RE = /[\p{L}\p{N}_']+/gu;
const ALONG_BLOCK = "p, li, blockquote, dd, h1, h2, h3, h4, h5, h6, figcaption, div:not(.page)";
const along = { st: null, running: false };

function sentenceWords(container, n) {
  const ranges = [];
  for (const el of container.querySelectorAll(`.s[data-s="${n}"]`)) {
    const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      for (const m of node.data.matchAll(WORD_RE)) {
        // (Only what the sync counts as a word: not a lone apostrophe.)
        const lead = m[0].length - m[0].replace(/^'+/, "").length;
        const core = m[0].replace(/^'+|'+$/g, "");
        if (!core) continue;
        const r = document.createRange();
        r.setStart(node, m.index + lead);
        r.setEnd(node, m.index + lead + core.length);
        ranges.push(r);
      }
    }
  }
  return ranges;
}

// One frame of the read-along. (A failure in one frame mustn't stop it for good: it went on
// believing it was running, so no word was shown again until the page was reloaded.)
function alongTick() {
  try {
    alongFrame();
  } catch (e) {
    console.warn("read-along:", e);
    const st = along.st;
    if (st && !st.audio.paused) requestAnimationFrame(alongTick);
    else along.running = false;
  }
}

function alongFrame() {
  const st = along.st;
  if (!st || !st.container.isConnected || !window.CSS || !CSS.highlights) {
    along.running = false;
    return;
  }
  const t = st.audio.currentTime;
  let i = -1;
  if (t >= st.start - 0.05 && t <= st.end + 0.3) {
    if (st.times && st.times.length === st.words.length) {
      for (let k = 0; k < st.times.length && st.times[k] <= t + 0.03; k++) i = k;
    } else if (st.words.length) {
      // By length: longer words take longer to say.
      const f = (t - st.start) / Math.max(0.01, st.end - st.start);
      let acc = 0;
      for (let k = 0; k < st.weights.length; k++) {
        acc += st.weights[k];
        if (f * st.total < acc) { i = k; break; }
      }
      if (i < 0) i = st.words.length - 1;
    }
  }
  if (i !== st.shown) {
    st.shown = i;
    if (i >= 0 && st.words[i]) {
      const h = new Highlight(st.words[i]);
      h.priority = 2;
      CSS.highlights.set("duet-word", h);
      // Reading on over the page's end: turn to where the word is.
      const pager = st.follow && !st.audio.paused && pagerOf(st.container);
      if (pager && !pager.busy && !browsing(pager.book)) {
        const r = st.words[i].getBoundingClientRect();
        const b = pager.book.getBoundingClientRect();
        if (r.width > 0 && r.left >= b.right) {
          const span = st.words[i].startContainer.parentElement;
          if (span) pager.showElement(span, true);
        }
      }
    }
    else CSS.highlights.delete("duet-word");
  }
  if (st.audio.paused) {
    along.running = false;
    return;
  }
  requestAnimationFrame(alongTick);
}

export function readAlong(container, audio, n, start, end, times, follow) {
  if (!window.CSS || !CSS.highlights) return;
  if (!container || n < 0) {
    along.st = null;
    CSS.highlights.delete("duet-word");
    CSS.highlights.delete("duet-para");
    return;
  }
  const words = sentenceWords(container, n);
  const weights = words.map((r) => r.toString().length + 1.5);
  along.st = {
    container, audio, n, start, end, words, weights, shown: -2, follow: !!follow,
    total: weights.reduce((a, b) => a + b, 0),
    times: times && times.length ? Array.from(times) : null,
  };
  // The paragraph (or other block) the sentence is in.
  const first = container.querySelector(`.s[data-s="${n}"]`);
  const block = first && first.closest(ALONG_BLOCK);
  if (block && container.contains(block)) {
    const r = document.createRange();
    r.selectNodeContents(block);
    CSS.highlights.set("duet-para", new Highlight(r));
  } else {
    CSS.highlights.delete("duet-para");
  }
  if (!audio.__along) {
    audio.__along = true;
    audio.addEventListener("play", () => { if (!along.running && along.st) { along.running = true; requestAnimationFrame(alongTick); } });
    audio.addEventListener("seeked", () => { if (along.st) along.st.shown = -2; });
  }
  if (!along.running) {
    along.running = true;
    requestAnimationFrame(alongTick);
  }
}

// Keeps your place when the page is closed or reloaded: `save` is called as the page goes away
// (or out of sight), and sends with `keepalive` so the request outlives the page.
export function onLeave(save) {
  const go = () => save();
  window.addEventListener("pagehide", go);
  const vis = () => { if (document.visibilityState === "hidden") save(); };
  document.addEventListener("visibilitychange", vis);
  return () => { window.removeEventListener("pagehide", go); document.removeEventListener("visibilitychange", vis); };
}

export function putKeepalive(url, json) {
  // (Automated test runs set this so they don't move your place in the book.)
  if (window.__duetNoSave) return;
  fetch(url, { method: "PUT", headers: { "content-type": "application/json" }, body: json, keepalive: true }).catch(() => {});
}

// The first word on the page showing (the paged reader): [sentence, word index in it], so play
// can start there even when the sentence began on the page before.
export function firstWordOnPage(container) {
  const pager = pagerOf(container);
  if (!pager) return [visibleSentence(container), 0];
  const n = pager.firstSentence();
  const b = pager.book.getBoundingClientRect();
  const words = sentenceWords(container, n);
  for (let i = 0; i < words.length; i++) {
    const r = words[i].getBoundingClientRect();
    if (r.width > 0 && r.left >= b.left - 1 && r.right <= b.right + 1) return [n, i];
  }
  return [n, 0];
}

// Turning pages yourself while the recording plays: the page stops following the recording
// for a while, so you can look ahead (or back) without being pulled to where it's reading.
// Starting to play from somewhere ends it at once.
export function browsing(book) {
  return !!book && performance.now() - (book.__userTurn || -1e9) < 20000;
}

export function stopBrowsing(book) {
  if (book) book.__userTurn = 0;
}

// ---------------------------------------------------------------------------------------------
// Comics: the balloons and captions the vision model found on each page, laid over the page
// picture (invisible, until one is being performed or the page is translated), and a guided view
// that moves through the page a panel at a time.

const comicData = new Map();
const comicWanted = new Map();

function imgPage(container, p) {
  return container && container.querySelector(`.img-page[data-p="${p}"]`);
}

// The page picture showing (the first, of two facing), or -1.
export function visiblePage(container) {
  if (!container) return -1;
  const book = container.closest(".bookview");
  const r = book && book.classList.contains("paged") ? book.getBoundingClientRect() : { left: 0, right: window.innerWidth, top: 0, bottom: window.innerHeight };
  let best = -1, bestLeft = Infinity;
  for (const e of container.querySelectorAll(".img-page")) {
    const b = e.getBoundingClientRect();
    if (!b.width || b.right <= r.left + 4 || b.left >= r.right - 4 || b.bottom <= r.top + 4 || b.top >= r.bottom - 4) continue;
    // Facing pages: the one read first (the right one, in manga).
    const rtl = !!(book && book.__rtl);
    const key = rtl ? -b.left : b.top * 10000 + b.left;
    if (key < bestLeft) { bestLeft = key; best = Number(e.dataset.p); }
  }
  return best;
}

// The pages on screen and either side of them.
function pagesNear(container) {
  const all = Array.from(container.querySelectorAll(".img-page"));
  const book = container.closest(".bookview");
  const r = book && book.classList.contains("paged") ? book.getBoundingClientRect() : { left: 0, right: window.innerWidth, top: 0, bottom: window.innerHeight };
  const near = new Set();
  all.forEach((e, i) => {
    const b = e.getBoundingClientRect();
    if (b.width && b.right > r.left && b.left < r.right && b.bottom > r.top && b.top < r.bottom) {
      for (const j of [i - 1, i, i + 1]) if (all[j]) near.add(Number(all[j].dataset.p));
    }
  });
  return Array.from(near);
}

// The bubbles' layer sits exactly over the picture (which is centred and scaled to fit).
function placeLayer(pageEl) {
  const img = pageEl.querySelector("img");
  const layer = pageEl.querySelector(":scope > .bubbles");
  if (!img || !layer) return;
  layer.style.left = `${img.offsetLeft}px`;
  layer.style.top = `${img.offsetTop}px`;
  layer.style.width = `${img.offsetWidth}px`;
  layer.style.height = `${img.offsetHeight}px`;
  fitTranslations(layer);
}

// Translated words fill their balloon as a letterer would: as big as fits.
function fitTranslations(layer) {
  for (const b of layer.querySelectorAll(".bubble.tr")) {
    const span = b.firstElementChild;
    if (!span || !b.clientHeight) continue;
    let lo = 5, hi = Math.max(6, Math.min(40, b.clientHeight * 0.6));
    for (let i = 0; i < 8; i++) {
      const mid = (lo + hi) / 2;
      span.style.fontSize = `${mid}px`;
      if (span.scrollHeight <= b.clientHeight + 1 && span.scrollWidth <= b.clientWidth + 1) lo = mid; else hi = mid;
    }
    span.style.fontSize = `${lo}px`;
  }
}

function drawBubbles(pageEl, data, translate) {
  pageEl.querySelectorAll(":scope > .bubbles").forEach((e) => e.remove());
  const layer = document.createElement("div");
  layer.className = "bubbles";
  const p = pageEl.dataset.p;
  data.lines.forEach((l, i) => {
    const [x0, y0, x1, y1] = l.box;
    if (x1 - x0 <= 0 || y1 - y0 <= 0) return;
    const b = document.createElement("div");
    b.className = `bubble k-${l.kind}`;
    b.dataset.p = p;
    b.dataset.b = i;
    b.style.left = `${x0 * 100}%`;
    b.style.top = `${y0 * 100}%`;
    b.style.width = `${(x1 - x0) * 100}%`;
    b.style.height = `${(y1 - y0) * 100}%`;
    b.title = (l.speaker ? `${l.speaker}: ` : "") + (l.original ? `${l.original}\n${l.text}` : l.text);
    if (translate && l.original) {
      b.classList.add("tr");
      const span = document.createElement("span");
      span.textContent = l.text;
      b.appendChild(span);
    }
    layer.appendChild(b);
  });
  pageEl.appendChild(layer);
  const img = pageEl.querySelector("img");
  if (img) {
    if (!img.complete) img.addEventListener("load", () => placeLayer(pageEl), { once: true });
    if (!pageEl.__ro && window.ResizeObserver) {
      pageEl.__ro = new ResizeObserver(() => placeLayer(pageEl));
      pageEl.__ro.observe(img);
    }
  }
  placeLayer(pageEl);
}

async function comicFetch(asset, p) {
  const key = `${asset}/${p}`;
  if (comicData.has(key)) return comicData.get(key);
  if (!comicWanted.has(key)) {
    comicWanted.set(key, (async () => {
      const r = await fetch(`/api/assets/${asset}/comic/${p}`);
      if (!r.ok) {
        comicWanted.delete(key);
        throw new Error((await r.text()) || `page ${p + 1} couldn't be read`);
      }
      const data = await r.json();
      comicData.set(key, data);
      return data;
    })());
  }
  return comicWanted.get(key);
}

// Lays the balloons over the pages on screen (and either side), reading any not read yet.
// `translate`: translations shown over the balloons. Resolves to an error message, or "".
export async function comicLoad(container, asset, translate) {
  if (!container) return "";
  const book = container.closest(".bookview");
  for (const p of pagesNear(container)) {
    const el = imgPage(container, p);
    if (!el || (el.__drawn === translate && el.querySelector(":scope > .bubbles"))) continue;
    try {
      const data = await comicFetch(asset, p);
      const now = imgPage(container, p);
      if (!now) continue;
      drawBubbles(now, data, translate);
      now.__drawn = translate;
      if (book && book.__comicNow && book.__comicNow[0] === p) comicShow(container, p, book.__comicNow[1], false);
    } catch (e) {
      return String(e.message || e);
    }
  }
  return "";
}

// Takes the balloons off (back to plain pictures).
export function comicClear(container) {
  if (!container) return;
  container.querySelectorAll(".img-page > .bubbles").forEach((e) => e.remove());
  container.querySelectorAll(".img-page").forEach((e) => { e.__drawn = undefined; });
}

export function comicSetRtl(container, rtl) {
  const book = container && container.closest(".bookview");
  if (book) book.__rtl = !!rtl;
}

// Highlights the line being performed (page p, line b; b < 0: none) and turns to its page.
export function comicShow(container, p, b, follow) {
  if (!container) return;
  const book = container.closest(".bookview");
  if (book) book.__comicNow = [p, b];
  container.querySelectorAll(".bubble.now").forEach((e) => e.classList.remove("now"));
  if (b >= 0) container.querySelectorAll(`.bubble[data-p="${p}"][data-b="${b}"]`).forEach((e) => e.classList.add("now"));
  if (panel && panel.container === container) {
    panel.now = [p, b];
    panelMark();
    panelFollow(p, b);
  }
  if (!follow) return;
  const el = imgPage(container, p);
  if (!el) return;
  const pager = pagerOf(container);
  if (pager) {
    const r = el.getBoundingClientRect(), v = pager.book.getBoundingClientRect();
    const showing = r.width > 0 && r.right > v.left + 4 && r.left < v.right - 4;
    if (!showing && !browsing(pager.book)) pager.showElement(el, true);
    return;
  }
  const r = el.getBoundingClientRect();
  if (r.bottom < 60 || r.top > window.innerHeight - 120) el.scrollIntoView({ behavior: "smooth", block: "start" });
}

// --- Panel by panel ---------------------------------------------------------------------------

let panel = null;

function panelBox(data, k) {
  const panels = data && data.panels && data.panels.length ? data.panels : [[0, 0, 1, 1]];
  return panels[Math.max(0, Math.min(panels.length - 1, k))];
}

function panelCount(data) {
  return data && data.panels && data.panels.length ? data.panels.length : 1;
}

function panelFit() {
  if (!panel) return;
  const { stage, img } = panel;
  const VW = panel.root.clientWidth, VH = panel.root.clientHeight;
  const aspect = img.naturalWidth ? img.naturalHeight / img.naturalWidth : 1.5;
  const Sw = 1000, Sh = Sw * aspect;
  stage.style.width = `${Sw}px`;
  stage.style.height = `${Sh}px`;
  const [x0, y0, x1, y1] = panel.whole ? [0, 0, 1, 1] : panelBox(panel.data, panel.k);
  const pw = Math.max(0.05, x1 - x0) * Sw, ph = Math.max(0.05, y1 - y0) * Sh;
  const m = 0.94;
  const scale = Math.min((VW * m) / pw, (VH * m) / ph);
  const tx = VW / 2 - (x0 * Sw + pw / 2) * scale, ty = VH / 2 - (y0 * Sh + ph / 2) * scale;
  stage.style.transform = `translate(${tx}px, ${ty}px) scale(${scale})`;
  placeLayer(stage);
}

async function panelPage(p, k, whole) {
  if (!panel) return;
  panel.p = p;
  panel.k = k;
  panel.whole = !!whole;
  const url = `/api/assets/${panel.asset}/pages/${p}`;
  if (panel.img.getAttribute("src") !== url) {
    panel.stage.querySelectorAll(":scope > .bubbles").forEach((e) => e.remove());
    panel.data = null;
    panel.img.src = url;
    await new Promise((res) => { if (panel.img.complete) res(); else panel.img.onload = panel.img.onerror = () => res(); });
  }
  panel.stage.dataset.p = p;
  panel.label.textContent = `Page ${p + 1}`;
  panelFit();
  try {
    const data = await comicFetch(panel.asset, p);
    if (!panel || panel.p !== p) return;
    const fresh = !panel.data;
    panel.data = data;
    if (fresh) {
      drawBubbles(panel.stage, data, panel.translate);
      panelMark();
    }
    if (k < 0) panel.k = panelCount(data) - 1;
    panelFit();
    panel.label.textContent = `Page ${p + 1} · panel ${panel.k + 1} of ${panelCount(data)}`;
  } catch (e) {
    panel.label.textContent = `Page ${p + 1} (couldn't find its panels: ${e.message || e})`;
  }
}

function panelStep(d) {
  if (!panel) return;
  const n = panelCount(panel.data);
  if (panel.whole && d > 0) return panelPage(panel.p, 0, false);
  const k = panel.k + d;
  if (k >= 0 && k < n && panel.data) return panelPage(panel.p, k, false);
  const p = panel.p + d;
  if (p < 0 || p >= panel.pages) return;
  // A new page is seen whole first, then panel by panel.
  return d > 0 ? panelPage(p, 0, true) : panelPage(p, -1, false);
}

function panelFollow(p, b) {
  if (!panel || b < 0) return;
  const go = () => {
    const line = panel.data && panel.data.lines[b];
    if (line && panel.p === p && (line.panel !== panel.k || panel.whole)) panelPage(p, line.panel, false);
  };
  if (panel.p !== p) panelPage(p, 0, false).then(go); else go();
}

function panelMark() {
  if (!panel) return;
  panel.stage.querySelectorAll(".bubble.now").forEach((e) => e.classList.remove("now"));
  const [p, b] = panel.now || [-1, -1];
  if (p === panel.p && b >= 0) panel.stage.querySelectorAll(`.bubble[data-b="${b}"]`).forEach((e) => e.classList.add("now"));
}

function panelKey(ev) {
  if (!panel) return;
  const fwd = panel.rtl ? "ArrowLeft" : "ArrowRight", back = panel.rtl ? "ArrowRight" : "ArrowLeft";
  if (ev.key === "Escape") panelClose();
  else if (ev.key === fwd || ev.key === "ArrowDown" || ev.key === " " || ev.key === "PageDown") panelStep(1);
  else if (ev.key === back || ev.key === "ArrowUp" || ev.key === "PageUp") panelStep(-1);
  else return;
  ev.preventDefault();
  ev.stopPropagation();
}

export function panelClose() {
  if (!panel) return;
  const { root, onClose, p } = panel;
  window.removeEventListener("keydown", panelKey, true);
  window.removeEventListener("resize", panelFit);
  panel = null;
  root.remove();
  if (onClose) onClose(p);
}

// Opens the guided view at page p: tap the right side (the left, in manga) or press → for the next
// panel. `onClose(page)` is told where it was left.
export function panelOpen(container, asset, p, pages, rtl, translate, onClose) {
  panelClose();
  const root = document.createElement("div");
  root.className = "panel-view";
  root.innerHTML = `<div class="pv-stage img-page"><img alt=""></div><div class="pv-bar"><span class="pv-label"></span><button class="pv-close" title="Back to pages">Done</button></div>`;
  document.body.appendChild(root);
  const stage = root.querySelector(".pv-stage");
  panel = { root, stage, img: stage.querySelector("img"), label: root.querySelector(".pv-label"), container, asset, p, k: 0, pages, rtl, translate, onClose, data: null, whole: false };
  root.querySelector(".pv-close").addEventListener("click", (e) => { e.stopPropagation(); panelClose(); });
  root.addEventListener("click", (e) => {
    if (e.target.closest(".pv-bar")) return;
    const x = e.clientX / root.clientWidth;
    const back = panel.rtl ? x > 0.7 : x < 0.3;
    panelStep(back ? -1 : 1);
  });
  let sx = null;
  root.addEventListener("touchstart", (e) => { sx = e.touches[0].clientX; }, { passive: true });
  root.addEventListener("touchend", (e) => {
    if (sx === null) return;
    const dx = e.changedTouches[0].clientX - sx;
    sx = null;
    if (Math.abs(dx) > 50) {
      e.preventDefault();
      panelStep((dx < 0) !== panel.rtl ? 1 : -1);
    }
  });
  window.addEventListener("keydown", panelKey, true);
  window.addEventListener("resize", panelFit);
  const now = container && container.closest(".bookview") && container.closest(".bookview").__comicNow;
  panelPage(p, 0, false).then(() => { if (now && now[0] === p) panelFollow(now[0], now[1]); });
}
