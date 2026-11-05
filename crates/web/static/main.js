import init from "/duet_web.js";
init();

// Live reload under scripts/dev.sh: the server only offers this endpoint in dev mode, and a
// 404 closes the EventSource for good, so production pages never retry.
let version = null;
const live = new EventSource("/api/dev/reload");
live.onmessage = (e) => {
  if (version !== null && e.data !== version) location.reload();
  version = e.data;
};
