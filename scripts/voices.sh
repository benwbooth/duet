#!/usr/bin/env bash
#
# Runs duet's local TTS server on :8880: Fish Audio S2-Pro performs the lines, Qwen3-TTS
# VoiceDesign casts new voices. It starts fish-speech itself and pauses it while casting, so
# both fit on one 24 GB GPU. Set up first with scripts/setup-voices.sh.
#
# duet starts this itself (Settings -> Voices); run by hand with the same arguments.
#
# Usage: scripts/voices.sh [fish-speech dir] [workers venv] [voices dir] [port] [idle minutes]

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
data="${DUET_DATA_DIR:-$root/data}"
fish="${1:-$data/fish-speech}"
venv="${2:-$data/venv}"
voices="${3:-$data/voices}"
port="${4:-8880}"
idle="${5:-5}"
cd "$root"
# Model downloads (torch hub, Hugging Face) need certificates; NixOS keeps them here.
if [[ -z "${SSL_CERT_FILE:-}" && -r /etc/ssl/certs/ca-certificates.crt ]]; then
  export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
fi
exec "$venv/bin/python" workers/tts_server.py \
  --port "$port" \
  --idle-minutes "$idle" \
  --engine fish \
  --fish-dir "$fish" \
  --fish-url "http://127.0.0.1:$((port + 1))" \
  --fish-python "$fish/.venv/bin/python" \
  --fish-checkpoint checkpoints/s2-pro \
  --voices "$voices"
