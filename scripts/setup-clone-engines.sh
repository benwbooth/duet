#!/usr/bin/env bash
#
# Optional voice engines for the briefing hosts, each in its own environment beside the
# others (they pin different transformers): VibeVoice (Microsoft's long-form podcast model, from
# the community fork) and Chatterbox (Resemble AI). The voice server finds them there and starts
# them when a host asks for one (Briefing settings -> Hosts -> Engine). Safe to run again.
#
# Usage: scripts/setup-clone-engines.sh [data dir] [vibevoice|chatterbox ...]

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
data="${1:-${DUET_DATA_DIR:-$root/data}}"
shift || true
if [[ $# -gt 0 ]]; then engines=("$@"); else engines=(vibevoice chatterbox); fi
if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1; then
  torch_index="https://download.pytorch.org/whl/cu128"
else
  torch_index="https://download.pytorch.org/whl/rocm7.1"
fi
gpu_torch() {
  uv pip install --python "$1" --index-url "$torch_index" --reinstall-package torch --reinstall-package torchaudio torch torchaudio
}

for e in "${engines[@]}"; do
  venv="$data/$e-venv"
  printf '\n\033[1m== %s (%s)\033[0m\n' "$e" "$venv"
  case "$e" in
    vibevoice)
      [[ -d "$venv" ]] || uv venv --python 3.12 "$venv"
      uv pip install --python "$venv/bin/python" --index-url "$torch_index" torch torchaudio
      uv pip install --python "$venv/bin/python" "vibevoice @ git+https://github.com/vibevoice-community/VibeVoice" soundfile "huggingface_hub[cli]"
      gpu_torch "$venv/bin/python"
      "$venv/bin/hf" download microsoft/VibeVoice-1.5B
      ;;
    chatterbox)
      # (Its pinned numpy has no wheels for Python 3.12.)
      [[ -d "$venv" ]] || uv venv --python 3.11 "$venv"
      uv pip install --python "$venv/bin/python" --index-url "$torch_index" torch torchaudio
      uv pip install --python "$venv/bin/python" chatterbox-tts soundfile "huggingface_hub[cli]"
      gpu_torch "$venv/bin/python"
      "$venv/bin/hf" download ResembleAI/chatterbox
      ;;
    *) echo "unknown engine $e" >&2; exit 2 ;;
  esac
  "$venv/bin/python" -c "import torch; print('$e torch', torch.__version__, 'GPU', torch.cuda.is_available())"
done
echo "Done."
