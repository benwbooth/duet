"""Helpers shared by the duet workers."""

import json
import os
import re
import sys
import urllib.request


def progress(fraction, message=""):
    """Reports progress to the duet server (it reads JSON lines on stdout)."""
    print(json.dumps({"progress": round(float(fraction), 4), "message": message}), flush=True)


def log(*args):
    print(*args, file=sys.stderr, flush=True)


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def save(path, data):
    import threading

    # Unique, so two writers of the same file (two requests, say) can't trip over each other.
    tmp = f"{path}.{os.getpid()}.{threading.get_ident()}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f)
    os.replace(tmp, path)


def device(name):
    if name != "auto":
        return name
    try:
        import torch

        return "cuda" if torch.cuda.is_available() else "cpu"
    except ImportError:
        return "cpu"


WORD = re.compile(r"[\w']+", re.UNICODE)


def words(text):
    """Lowercased words, apostrophes kept, for matching transcripts to book text."""
    return [w.lower().strip("'") for w in WORD.findall(text) if w.strip("'")]


def chat(url, model, key, messages, json_mode=False, temperature=0.2):
    """One call to an OpenAI-compatible chat completions endpoint."""
    body = {"model": model, "messages": messages, "temperature": temperature}
    if json_mode:
        body["response_format"] = {"type": "json_object"}
    req = urllib.request.Request(
        url.rstrip("/") + "/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", **({"Authorization": f"Bearer {key}"} if key else {})},
    )
    with urllib.request.urlopen(req, timeout=600) as r:
        content = json.load(r)["choices"][0]["message"]["content"] or ""
    # Reasoning models may put their thinking inline.
    if "</think>" in content:
        content = content.split("</think>", 1)[1]
    return content.strip()


def chat_json(url, model, key, messages):
    text = chat(url, model, key, messages, json_mode=True)
    match = re.search(r"\{.*\}|\[.*\]", text, re.S)
    return json.loads(match.group(0) if match else text)


def whisper(model_name, dev):
    """faster-whisper on the GPU when CTranslate2 supports it (NVIDIA), otherwise the CPU.

    On the CPU the large models are slow for a whole audiobook, and alignment only needs word
    timings for text we already have, so it steps down to a smaller model there.
    """
    from faster_whisper import WhisperModel

    if dev == "cuda":
        try:
            return WhisperModel(model_name, device="cuda", compute_type="float16"), "cuda"
        except Exception as e:  # ROCm torch reports a GPU that CTranslate2 can't use
            log(f"faster-whisper can't use the GPU ({e}); using the CPU")
    if model_name.startswith("large") or model_name.startswith("medium"):
        log(f"using the 'small' speech model on the CPU instead of '{model_name}'")
        model_name = "small"
    return WhisperModel(model_name, device="cpu", compute_type="int8", cpu_threads=0), "cpu"


def load_audio(path, rate=16000):
    """Decodes any audio file to 16 kHz mono float32 with ffmpeg, for faster-whisper.

    Passing the samples directly avoids faster-whisper's own PyAV decoding, which breaks with
    some PyAV versions.
    """
    import subprocess

    import numpy as np

    out = subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-i", path, "-f", "f32le", "-ac", "1", "-ar", str(rate), "pipe:1"],
        capture_output=True,
        check=True,
    )
    return np.frombuffer(out.stdout, dtype=np.float32)


def load_audio_window(path, start, duration, rate=16000):
    """Decodes `duration` seconds from `start` of an audio file (16 kHz mono float32)."""
    import subprocess

    import numpy as np

    out = subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-ss", f"{max(0.0, start):.3f}", "-t", f"{duration:.3f}", "-i", path,
         "-f", "f32le", "-ac", "1", "-ar", str(rate), "pipe:1"],
        capture_output=True,
        check=True,
    )
    return np.frombuffer(out.stdout, dtype=np.float32)
