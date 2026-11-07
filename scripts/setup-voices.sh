#!/usr/bin/env bash
#
# Sets up the local voice stack on the GPU (AMD ROCm, or NVIDIA CUDA when there's an NVIDIA card):
#   * fish-speech with Fish Audio S2-Pro, the acting model (24 GB)
#   * a venv for the workers and the TTS server (Qwen3-TTS VoiceDesign + Base, and Whisper for
#     speech recognition: on the GPU through PyTorch, faster-whisper on the CPU for word timings)
# Run from Settings -> Voices in the web UI, or by hand. Safe to run again.
#
# Usage: scripts/setup-voices.sh <fish-speech dir> <workers venv dir>

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
data="${DUET_DATA_DIR:-$root/data}"
fish="${1:-$data/fish-speech}"
venv="${2:-$data/venv}"
# PyTorch built for the card: CUDA on NVIDIA, ROCm otherwise (ROCm builds call the GPU "cuda" too).
if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1; then
  torch_index="https://download.pytorch.org/whl/cu128"
else
  torch_index="https://download.pytorch.org/whl/rocm7.1"
fi

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

step "[1/6] fish-speech checkout"
[[ -d "$fish/.git" ]] || git clone https://github.com/fishaudio/fish-speech "$fish"

step "[2/6] fish-speech environment (GPU torch)"
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
# Keep the GPU torch even if a dependency asked for another one.
uv pip install --python .venv/bin/python --index-url "$torch_index" --reinstall-package torch --reinstall-package torchaudio torch torchaudio

step "[3/6] Fish Audio S2-Pro weights"
.venv/bin/hf download fishaudio/s2-pro --local-dir checkpoints/s2-pro

step "[4/6] duet workers environment"
[[ -d "$venv" ]] || uv venv --python 3.12 "$venv"
uv pip install --python "$venv/bin/python" --index-url "$torch_index" torch torchaudio
uv pip install --python "$venv/bin/python" qwen-tts faster-whisper transformers soundfile ormsgpack numpy "huggingface_hub[cli]"
uv pip install --python "$venv/bin/python" --index-url "$torch_index" --reinstall-package torch --reinstall-package torchaudio torch torchaudio

step "[4b/6] voice designer environment (OmniVoice, Maya1; newer transformers than Qwen3-TTS)"
design="$(dirname "$venv")/design-venv"
[[ -d "$design" ]] || uv venv --python 3.12 "$design"
uv pip install --python "$design/bin/python" --index-url "$torch_index" torch torchaudio
uv pip install --python "$design/bin/python" omnivoice snac soundfile accelerate speechbrain "huggingface_hub[cli]"
uv pip install --python "$design/bin/python" --index-url "$torch_index" --reinstall-package torch --reinstall-package torchaudio torch torchaudio
"$design/bin/hf" download k2-fsa/OmniVoice
"$design/bin/hf" download maya-research/maya1
"$design/bin/hf" download hubertsiuzdak/snac_24khz

step "[5/6] Qwen3-TTS weights (fallback reader)"
"$venv/bin/hf" download Qwen/Qwen3-TTS-12Hz-1.7B-VoiceDesign
"$venv/bin/hf" download Qwen/Qwen3-TTS-12Hz-1.7B-Base
# Speech recognition on the GPU (the microphone, transcripts).
"$venv/bin/hf" download openai/whisper-large-v3-turbo

step "[6/6] check"
"$fish/.venv/bin/python" -c "import torch; print('fish torch', torch.__version__, 'GPU', torch.cuda.is_available())"
"$venv/bin/python" -c "import torch, qwen_tts; print('workers torch', torch.__version__, 'GPU', torch.cuda.is_available())"
echo "Done."
