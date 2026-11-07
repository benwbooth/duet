#!/usr/bin/env bash
#
# Change -> rebuild -> reload loop.
#
#   * crates/web, crates/common or static files change: the UI is rebuilt and swapped in.
#   * crates/server or crates/common change: the server is rebuilt, and only once the build
#     succeeds is the running server replaced. A failed build leaves the old one running.
#
# Open pages reload themselves (no browser refresh needed): the server runs with DUET_DEV=1,
# which enables /api/dev/reload, and the page reloads when it sees a new server or a new UI.
#
# Usage: scripts/dev.sh [--listen 127.0.0.1:8484] [--data-dir data]
# Server output goes to target/dev-server.log.

set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root" || exit 1

# Re-enter through the dev shell when its tools aren't on PATH.
if ! command -v watchexec >/dev/null || ! command -v wasm-bindgen >/dev/null; then
  exec nix develop "$root" -c "$0" "$@"
fi

export DUET_LISTEN="${DUET_LISTEN:-127.0.0.1:8484}"
export DUET_DATA_DIR="${DUET_DATA_DIR:-$root/data}"
while (( $# )); do
  case "$1" in
    --listen) DUET_LISTEN="$2"; shift ;;
    --data-dir) DUET_DATA_DIR="$2"; shift ;;
    --step-web | --step-server) step="$1" ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
# (The voice server outlives each restart of duet, so the performer stays warm.)
export DUET_DEV=1 DUET_KEEP_VOICES=1 DUET_WEB_DIR="$root/crates/web/dist"
pidfile="$root/target/dev-server.pid"
log="$root/target/dev-server.log"
mkdir -p "$root/target"

stamp() { printf '\033[2m%s\033[0m %s\n' "$(date +%T)" "$*"; }

build_web() {
  stamp "building UI"
  if crates/web/build.sh >/dev/null 2>"$root/target/dev-web.err"; then
    stamp "UI ready"
  else
    cat "$root/target/dev-web.err" >&2
    stamp "UI build failed; keeping the previous UI"
  fi
}

restart_server() {
  stamp "building server"
  if ! cargo build -p duet-server; then
    stamp "server build failed; keeping the running server"
    return
  fi
  if [[ -f "$pidfile" ]] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
    kill "$(cat "$pidfile")"
    while kill -0 "$(cat "$pidfile")" 2>/dev/null; do sleep 0.1; done
  fi
  # Its own session, so watchexec never takes it down along with a build step.
  setsid "$root/target/debug/duet" >>"$log" 2>&1 </dev/null &
  echo $! >"$pidfile"
  stamp "server running on http://$DUET_LISTEN (log: target/dev-server.log)"
}

# One-shot steps that the watchers below call.
case "${step:-}" in
  --step-web) build_web; exit 0 ;;
  --step-server) restart_server; exit 0 ;;
esac

cleanup() {
  trap - EXIT INT TERM
  [[ -f "$pidfile" ]] && kill "$(cat "$pidfile")" 2>/dev/null
  rm -f "$pidfile"
  kill 0 2>/dev/null
}
trap cleanup EXIT INT TERM

[[ -d crates/web/dist ]] || build_web
restart_server

watchexec --quiet --postpone --debounce 300ms --on-busy-update=queue \
  -w crates/web -w crates/common -e rs,js,css,html,svg,webmanifest,toml \
  --ignore 'crates/web/dist*' \
  -- "$0" --step-web &
watchexec --quiet --postpone --debounce 300ms --on-busy-update=queue \
  -w crates/server -w crates/common -e rs,sql,toml \
  -- "$0" --step-server &
wait
