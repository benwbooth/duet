// Records every frame of a page turn in a headless Chrome (DevTools screencast), for checking
// turns frame by frame. Needs duet running on :8484.
//   node scripts/record-turn.mjs <out dir> [next|prev]
// Records every compositor frame of a page turn through the DevTools protocol.
import { spawn } from "node:child_process";
import { writeFileSync, rmSync, mkdirSync } from "node:fs";
const out = process.argv[2] || "frames";
const action = process.argv[3] || "next";
rmSync(out, { recursive: true, force: true }); mkdirSync(out);
const chrome = spawn("google-chrome-stable", ["--headless=new", "--remote-debugging-port=9333", "--window-size=1100,720",
  "--user-data-dir=/tmp/duet-record-profile", "--no-first-run", "--hide-scrollbars", "about:blank"], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let tabs;
for (let i = 0; i < 50; i++) { try { tabs = await (await fetch("http://127.0.0.1:9333/json")).json(); break; } catch { await sleep(200); } }
const ws = new WebSocket(tabs.find((t) => t.type === "page").webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0; const waits = new Map(); const frames = [];
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && waits.has(msg.id)) { waits.get(msg.id)(msg.result); waits.delete(msg.id); }
  if (msg.method === "Page.screencastFrame") {
    frames.push({ t: msg.params.metadata.timestamp, data: msg.params.data });
    send("Page.screencastFrameAck", { sessionId: msg.params.sessionId });
  }
};
const send = (method, params = {}) => new Promise((r) => { const i = ++id; waits.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const evalJs = async (expr) => (await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true })).result?.value;
await send("Page.enable"); await send("Runtime.enable");
// Don't move the reader's saved place in the book.
await send("Page.addScriptToEvaluateOnNewDocument", { source: "window.__duetNoSave = true" });
await send("Page.navigate", { url: "http://localhost:8484/#/read/1/read" });
await sleep(8000);
console.log(await evalJs(`(async()=>{const r=document.querySelector('.reader'); r.classList.add('chrome-hidden'); const p=document.querySelector('.bookview').__pager; p.go(2,false); await new Promise(r=>setTimeout(r,1500)); return [p.page,p.pages,p.spread]})()`));
await send("Page.startScreencast", { format: "png", everyNthFrame: 1 });
await sleep(800);
const t0 = Date.now() / 1000;
await evalJs(`(()=>{window.__mark=performance.now(); const p=document.querySelector('.bookview').__pager; p.${action}(); return 1})()`);
await sleep(1800);
await send("Page.stopScreencast");
frames.forEach((f, i) => writeFileSync(`${out}/${String(i).padStart(3, "0")}_${Math.round((f.t - t0) * 1000)}.png`, Buffer.from(f.data, "base64")));
console.log(frames.length, "frames");
ws.close(); chrome.kill();
