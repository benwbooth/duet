#!/usr/bin/env bash
#
# Runs duet's local TTS server on :8880: Fish Audio S2-Pro performs the lines, Qwen3-TTS
# VoiceDesign casts new voices. It starts fish-speech itself and pauses it while casting, so
# both fit on one 24 GB GPU. Set up first with scripts/setup-voices.sh.
#
# Usage: scripts/voices.sh [fish-speech dir]

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fish="${1:-$HOME/src/fish-speech}"
cd "$root"
exec .venv/bin/python workers/tts_server.py \
  --engine fish \
  --fish-dir "$fish" \
  --fish-python "$fish/.venv/bin/python" \
  --fish-checkpoint checkpoints/s2-pro \
  --voices "$root/data/voices"
