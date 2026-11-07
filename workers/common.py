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


# Speech recognition on the graphics card when PyTorch has one (NVIDIA CUDA or AMD ROCm: ROCm
# builds of PyTorch call it "cuda" too). faster-whisper's engine, CTranslate2, only knows
# NVIDIA, so on AMD it would run on the CPU; this runs Whisper through PyTorch instead.
GPU_WHISPER = "openai/whisper-large-v3-turbo"


class _Word:
    def __init__(self, word, start, end):
        self.word, self.start, self.end = word, start, end


class _Segment:
    def __init__(self, words):
        self.words = words
        self.start = words[0].start
        self.end = words[-1].end
        self.text = "".join(w.word for w in words)


class TorchWhisper:
    """Whisper on PyTorch, answering like faster-whisper's WhisperModel.transcribe:
    (segments, info), each segment with .start, .end, .text and .words."""

    def __init__(self, model_name=GPU_WHISPER, device="cuda:0"):
        import torch
        from transformers import AutoModelForSpeechSeq2Seq, AutoProcessor, pipeline

        if getattr(torch.version, "hip", None):
            # On ROCm, MIOpen searching for the fastest convolution on every new input length
            # costs far more than it saves (as with Fish).
            os.environ.setdefault("MIOPEN_FIND_MODE", "FAST")
            torch.backends.cudnn.enabled = False
        model = AutoModelForSpeechSeq2Seq.from_pretrained(model_name, dtype=torch.float16, low_cpu_mem_usage=True)
        model.to(device)
        processor = AutoProcessor.from_pretrained(model_name)
        self.pipe = pipeline(
            "automatic-speech-recognition", model=model, tokenizer=processor.tokenizer,
            feature_extractor=processor.feature_extractor, dtype=torch.float16, device=device,
        )

    def transcribe(self, samples, word_timestamps=False, vad_filter=False, language=None, **_):
        import numpy as np

        samples = np.asarray(samples, dtype=np.float32)
        # Whisper makes words up out of silence; faster-whisper's VAD would have dropped it.
        if len(samples) < 1600 or (vad_filter and float(np.sqrt(np.mean(samples ** 2))) < 0.004):
            return [], None
        kwargs = {"language": language} if language else {}
        # (Clips under 30 s, like the microphone's, go through in one piece.)
        chunked = {"chunk_length_s": 30, "ignore_warning": True} if len(samples) > 30 * 16000 else {}
        out = self.pipe(
            {"raw": samples, "sampling_rate": 16000}, **chunked,
            # (Word timings keep every attention map, which takes a lot of memory per chunk.)
            batch_size=1 if word_timestamps else 8,
            return_timestamps="word" if word_timestamps else True, generate_kwargs=kwargs,
        )
        chunks = out.get("chunks") or []
        if not word_timestamps:
            segs = [_Segment([_Word(c["text"], c["timestamp"][0] or 0.0, c["timestamp"][1] or c["timestamp"][0] or 0.0)]) for c in chunks]
            if not segs and out.get("text", "").strip():
                segs = [_Segment([_Word(out["text"], 0.0, len(samples) / 16000)])]
            return segs, None
        words = []
        for c in chunks:
            start, end = c["timestamp"]
            start = start or (words[-1].end if words else 0.0)
            words.append(_Word(c["text"], start, end if end is not None else start))
        # Segments end at a sentence's end or a pause, as Whisper's own would.
        segs, cur = [], []
        for i, w in enumerate(words):
            cur.append(w)
            gap = words[i + 1].start - w.end if i + 1 < len(words) else 0
            if w.word.rstrip().endswith((".", "?", "!")) or gap > 1.0 or len(cur) >= 40:
                segs.append(_Segment(cur))
                cur = []
        if cur:
            segs.append(_Segment(cur))
        return segs, None


_gpu_whisper = None
_aligner = None


class ForcedAligner:
    """Lines known text up against the audio, word by word: the MMS forced-alignment model
    (torchaudio) hears which letters are sounding in each 20 ms frame, and the words are fitted
    to that in order. Precise to a frame or two, where Whisper's own word times wander by a few
    tenths of a second. The model runs on the graphics card (ROCm or CUDA); the fitting itself is
    cheap and runs on the CPU (torchaudio has no ROCm kernel for it)."""

    def __init__(self, device="cuda:0"):
        import torch
        import torchaudio

        if not torch.cuda.is_available():
            device = "cpu"
        bundle = torchaudio.pipelines.MMS_FA
        self.device = device
        self.model = bundle.get_model(with_star=True).to(device).eval()
        self.tokenizer = bundle.get_tokenizer()
        self.aligner = bundle.get_aligner()
        self.dictionary = set(bundle.get_dict(star="*"))

    def letters(self, word):
        """A word as the model spells it: lowercase letters and apostrophes, accents dropped."""
        import unicodedata

        w = unicodedata.normalize("NFKD", word.lower())
        return "".join(c for c in w if c in self.dictionary and c != "*")

    def align(self, samples, words, rate=16000):
        """(start, end) in seconds for each word, or None for one it can't spell (a number).
        Audio either side of the text (the end of a sentence before it, the start of one after)
        is allowed for and left out."""
        import numpy as np
        import torch

        spelled = [self.letters(w) for w in words]
        keep = [i for i, w in enumerate(spelled) if w]
        out = [None] * len(words)
        if not keep or len(samples) < rate // 4:
            return out
        with torch.inference_mode():
            wav = torch.from_numpy(np.asarray(samples, dtype=np.float32)).unsqueeze(0).to(self.device)
            emission, _ = self.model(wav)
            emission = emission[0].float().cpu()
        # "*" on each side soaks up speech that isn't in the text.
        tokens = self.tokenizer(["*"] + [spelled[i] for i in keep] + ["*"])
        spans = self.aligner(emission, tokens)
        ratio = wav.size(1) / emission.size(0) / rate
        for i, sp in zip(keep, spans[1:-1]):
            out[i] = (sp[0].start * ratio, sp[-1].end * ratio)
        return out


def aligner():
    """The forced aligner, loaded once (unloaded with speech recognition when idle)."""
    global _aligner
    if _aligner is None:
        _aligner = ForcedAligner()
    return _aligner


def release_gpu_whisper():
    """Lets the card's copy of Whisper (and the aligner) go: they're unloaded when idle."""
    global _gpu_whisper, _aligner
    _gpu_whisper = None
    _aligner = None
_aligner = None


class ForcedAligner:
    """Lines known text up against the audio, word by word: the MMS forced-alignment model
    (torchaudio) hears which letters are sounding in each 20 ms frame, and the words are fitted
    to that in order. Precise to a frame or two, where Whisper's own word times wander by a few
    tenths of a second. The model runs on the graphics card (ROCm or CUDA); the fitting itself is
    cheap and runs on the CPU (torchaudio has no ROCm kernel for it)."""

    def __init__(self, device="cuda:0"):
        import torch
        import torchaudio

        if not torch.cuda.is_available():
            device = "cpu"
        bundle = torchaudio.pipelines.MMS_FA
        self.device = device
        self.model = bundle.get_model(with_star=True).to(device).eval()
        self.tokenizer = bundle.get_tokenizer()
        self.aligner = bundle.get_aligner()
        self.dictionary = set(bundle.get_dict(star="*"))

    def letters(self, word):
        """A word as the model spells it: lowercase letters and apostrophes, accents dropped."""
        import unicodedata

        w = unicodedata.normalize("NFKD", word.lower())
        return "".join(c for c in w if c in self.dictionary and c != "*")

    def align(self, samples, words, rate=16000):
        """(start, end) in seconds for each word, or None for one it can't spell (a number).
        Audio either side of the text (the end of a sentence before it, the start of one after)
        is allowed for and left out."""
        import numpy as np
        import torch

        spelled = [self.letters(w) for w in words]
        keep = [i for i, w in enumerate(spelled) if w]
        out = [None] * len(words)
        if not keep or len(samples) < rate // 4:
            return out
        with torch.inference_mode():
            wav = torch.from_numpy(np.asarray(samples, dtype=np.float32)).unsqueeze(0).to(self.device)
            emission, _ = self.model(wav)
            emission = emission[0].float().cpu()
        # "*" on each side soaks up speech that isn't in the text.
        tokens = self.tokenizer(["*"] + [spelled[i] for i in keep] + ["*"])
        spans = self.aligner(emission, tokens)
        ratio = wav.size(1) / emission.size(0) / rate
        for i, sp in zip(keep, spans[1:-1]):
            out[i] = (sp[0].start * ratio, sp[-1].end * ratio)
        return out


def aligner():
    """The forced aligner, loaded once (unloaded with speech recognition when idle)."""
    global _aligner
    if _aligner is None:
        _aligner = ForcedAligner()
    return _aligner


def gpu_available():
    try:
        import torch

        return torch.cuda.is_available()
    except Exception:
        return False


def whisper(model_name, dev, words=False):
    """Whisper on the graphics card when there is one (see TorchWhisper), otherwise faster-whisper
    on the CPU.

    Word timings (`words`) are the exception on AMD: PyTorch's Whisper gets them by keeping every
    attention map, which on the card measured slower than faster-whisper on the CPU (and takes
    gigabytes more memory), so those stay with faster-whisper unless CTranslate2 has an NVIDIA GPU.

    On the CPU the large models are slow for a whole audiobook, and alignment only needs word
    timings for text we already have, so it steps down to a smaller model there.
    """
    if dev == "cuda" and not words and gpu_available():
        global _gpu_whisper
        try:
            # One copy on the card, whoever asks (the microphone, syncing, transcripts).
            if _gpu_whisper is None:
                _gpu_whisper = TorchWhisper()
            return _gpu_whisper, "cuda"
        except Exception as e:
            log(f"Whisper can't use the GPU ({e}); using the CPU")
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
