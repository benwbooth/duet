// Saves, for every page of four chapters, the live page and the picture the page curl uses of
// it (sweep/liveC_P.png and sweep/picC_P.png); they should be identical. Needs duet on :8484.
import { spawn } from "node:child_process";
import { writeFileSync, mkdirSync, rmSync } from "node:fs";
rmSync("sweep", { recursive: true, force: true }); mkdirSync("sweep");
const chrome = spawn("google-chrome-stable", ["--headless=new", "--remote-debugging-port=9335", "--window-size=1100,720",
  "--user-data-dir=/tmp/duet-sweep-profile", "--no-first-run", "--hide-scrollbars", "about:blank"], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let tabs; for (let i = 0; i < 50; i++) { try { tabs = await (await fetch("http://127.0.0.1:9335/json")).json(); break; } catch { await sleep(200); } }
const ws = new WebSocket(tabs.find((t) => t.type === "page").webSocketDebuggerUrl); await new Promise((r) => (ws.onopen = r));
let id = 0; const waits = new Map();
ws.onmessage = (m) => { const msg = JSON.parse(m.data); if (msg.id && waits.has(msg.id)) { waits.get(msg.id)(msg.result); waits.delete(msg.id); } };
const send = (method, params = {}) => new Promise((r) => { const i = ++id; waits.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const ev = async (e) => (await send("Runtime.evaluate", { expression: e, awaitPromise: true, returnByValue: true })).result?.value;
await send("Page.enable"); await send("Page.navigate", { url: "http://localhost:8484/#/read/1/read" }); await sleep(8000);
const info = await ev(`(async()=>{document.querySelector(".reader").classList.add("chrome-hidden"); const b=document.querySelector(".bookview"); const r=b.getBoundingClientRect(); return {pages:b.__pager.pages, x:r.left, y:r.top, w:r.width, h:r.height}})()`);
console.log(info);
for (let ch = 0; ch < 4; ch++) {
 const pages = await ev(`document.querySelector(".bookview").__pager.pages`);
 for (let p = 0; p < pages; p++) {
  const pic = await ev(`(async()=>{const b=document.querySelector(".bookview"); const pg=b.__pager; pg.go(${p},false); await new Promise(r=>setTimeout(r,400)); pg.snaps.clear(); const [c]=await pg.pictures([${p}*pg.width]); return c.toDataURL()})()`);
  const shot = await send("Page.captureScreenshot", { format: "png", clip: { x: info.x, y: info.y, width: info.w, height: info.h, scale: 1 } });
  writeFileSync(`sweep/live${ch}_${p}.png`, Buffer.from(shot.data, "base64"));
  writeFileSync(`sweep/pic${ch}_${p}.png`, Buffer.from(pic.split(",")[1], "base64"));
 }
 await ev(`(async()=>{const pg=document.querySelector(".bookview").__pager; pg.go(pg.pages-1,false); await new Promise(r=>setTimeout(r,300)); pg.h.onNext(); await new Promise(r=>setTimeout(r,2500)); document.querySelector(".reader").classList.add("chrome-hidden"); return 1})()`);
}
ws.close(); chrome.kill();
