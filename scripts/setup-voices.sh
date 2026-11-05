#!/usr/bin/env bash
#
# Sets up the local voice stack on an AMD (ROCm) GPU:
#   * fish-speech with Fish Audio S2-Pro, the acting model (24 GB)
#   * a venv for the workers and the TTS server (Qwen3-TTS VoiceDesign + Base, faster-whisper)
# Run from Settings -> Voices in the web UI, or by hand. Safe to run again.
#
# Usage: scripts/setup-voices.sh <fish-speech dir> <workers venv dir>

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
data="${DUET_DATA_DIR:-$root/data}"
fish="${1:-$data/fish-speech}"
venv="${2:-$data/venv}"
torch_index="https://download.pytorch.org/whl/rocm7.1"

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

step "[1/6] fish-speech checkout"
[[ -d "$fish/.git" ]] || git clone https://github.com/fishaudio/fish-speech "$fish"

step "[2/6] fish-speech environment (ROCm torch)"
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
# Newer TensorBoard (pulled in through torch.utils.tensorboard by audiotools) is generated for
# protobuf 6 and refuses to load with the 5.x pinned above.
uv pip install --python .venv/bin/python --no-deps "tensorboard<2.20"
# Keep the ROCm torch even if a dependency asked for another one.
uv pip install --python .venv/bin/python --index-url "$torch_index" --reinstall-package torch --reinstall-package torchaudio torch torchaudio

step "[3/6] Fish Audio S2-Pro weights"
.venv/bin/hf download fishaudio/s2-pro --local-dir checkpoints/s2-pro

step "[4/6] duet workers environment"
[[ -d "$venv" ]] || uv venv --python 3.12 "$venv"
uv pip install --python "$venv/bin/python" --index-url "$torch_index" torch torchaudio
uv pip install --python "$venv/bin/python" qwen-tts faster-whisper soundfile ormsgpack numpy "huggingface_hub[cli]"
uv pip install --python "$venv/bin/python" --index-url "$torch_index" --reinstall-package torch --reinstall-package torchaudio torch torchaudio

step "[5/6] Qwen3-TTS weights (voice casting)"
"$venv/bin/hf" download Qwen/Qwen3-TTS-12Hz-1.7B-VoiceDesign
"$venv/bin/hf" download Qwen/Qwen3-TTS-12Hz-1.7B-Base

step "[6/6] check"
"$fish/.venv/bin/python" -c "import torch; print('fish torch', torch.__version__, 'GPU', torch.cuda.is_available())"
"$venv/bin/python" -c "import torch, qwen_tts; print('workers torch', torch.__version__, 'GPU', torch.cuda.is_available())"
echo "Done."
