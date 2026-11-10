import init from "/duet_web.js";
// E-ink mode before anything draws (the same rule as einkOn in helpers.js).
try {
  const saved = localStorage.getItem("duet-eink");
  const on = saved === "on" || saved === "off" ? saved === "on" : matchMedia("(update: slow)").matches;
  document.documentElement.classList.toggle("eink", on);
} catch (_) {}
init();

// Live reload under scripts/dev.sh: the server only offers this endpoint in dev mode, and a
// 404 closes the EventSource for good, so production pages never retry.
let version = null;
const live = new EventSource("/api/dev/reload");
live.onmessage = (e) => {
  if (version !== null && e.data !== version) location.reload();
  version = e.data;
};
