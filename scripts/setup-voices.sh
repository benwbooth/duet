#!/usr/bin/env bash
#
# Sets up the local voice stack on an AMD (ROCm) GPU:
#   * ~/src/fish-speech with Fish Audio S2-Pro, the acting model (24 GB)
#   * .venv here for the workers and the TTS server (Qwen3-TTS VoiceDesign + Base,
#     faster-whisper)
# Safe to run again; finished steps are skipped.
#
# Usage: scripts/setup-voices.sh [fish-speech dir]

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fish="${1:-$HOME/src/fish-speech}"
torch_index="https://download.pytorch.org/whl/rocm7.1"

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

step "fish-speech checkout"
[[ -d "$fish" ]] || git clone https://github.com/fishaudio/fish-speech "$fish"

step "fish-speech environment (ROCm torch)"
cd "$fish"
[[ -d .venv ]] || uv venv --python 3.12 .venv
# The same packages as its ROCm Dockerfile, without the CUDA torch pin.
uv pip install --python .venv/bin/python --index-url "$torch_index" torch torchaudio
uv pip install --python .venv/bin/python \
  numpy "transformers==5.19.0" "tokenizers==0.23.2" hydra-core natsort einops librosa kui \
  uvicorn loguru pyrootutils resampy "einx[torch]==0.2.2" pydub "modelscope==1.17.1" \
  "opencc-python-reimplemented==0.1.7" silero-vad ormsgpack "tiktoken>=0.8.0" "pydantic==2.9.2" \
  cachetools descript-audio-codec safetensors soundfile vector_quantize_pytorch "huggingface_hub[cli]"
uv pip install --python .venv/bin/python --no-deps -e .
uv pip install --python .venv/bin/python --no-deps --upgrade "protobuf>=4.25,<6.0"
# Keep the ROCm torch even if a dependency asked for another one.
uv pip install --python .venv/bin/python --index-url "$torch_index" --reinstall-package torch --reinstall-package torchaudio torch torchaudio

step "Fish Audio S2-Pro weights"
.venv/bin/hf download fishaudio/s2-pro --local-dir checkpoints/s2-pro

step "duet workers environment"
cd "$root"
[[ -d .venv ]] || uv venv --python 3.12 .venv
uv pip install --python .venv/bin/python --index-url "$torch_index" torch torchaudio
uv pip install --python .venv/bin/python qwen-tts faster-whisper soundfile ormsgpack numpy "huggingface_hub[cli]"
uv pip install --python .venv/bin/python --index-url "$torch_index" --reinstall-package torch --reinstall-package torchaudio torch torchaudio

step "Qwen3-TTS weights (voice casting)"
.venv/bin/hf download Qwen/Qwen3-TTS-12Hz-1.7B-VoiceDesign
.venv/bin/hf download Qwen/Qwen3-TTS-12Hz-1.7B-Base

step "check"
"$fish/.venv/bin/python" -c "import torch; print('fish torch', torch.__version__, 'GPU', torch.cuda.is_available())"
.venv/bin/python -c "import torch, qwen_tts; print('workers torch', torch.__version__, 'GPU', torch.cuda.is_available())"
echo "Done. Start voices with scripts/voices.sh"
