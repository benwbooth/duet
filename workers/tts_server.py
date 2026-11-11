#!/usr/bin/env python3
"""An OpenAI-compatible text-to-speech server for duet.

POST /v1/audio/speech {"input", "voice", "instructions"?, "response_format"?}
POST /v1/audio/script {"pieces": [{"input", "voice", "instructions"?}], "pause"?, "response_format"?}
POST /v1/audio/transcriptions (multipart "file"): speech recognition with faster-whisper, so the
microphone works without a separate server.
POST /v1/sync-window {"book", "audio", "track", "start", "duration", "near"?}: listens to a short
stretch of an audiobook and finds where it is in the book, for syncing while you listen.

Two jobs, two models:
  * Casting. The first time a voice name is used, Qwen3-TTS VoiceDesign creates the voice from
    its description ("instructions", e.g. "gravelly old sailor, Cornish accent, slow and warm")
    and saves a reference clip to voices/<name>.wav. Drop your own clip there to use it instead.
  * Acting. Lines are performed by cloning that reference with the acting engine:
      --engine fish  Fish Audio Drama 3 or S2-Pro through a fish-speech API server (best acting;
                     honours inline directions like "[whispering, close to tears] Don't go.")
      --engine qwen  Qwen3-TTS Base (lighter; directions are stripped)
    A request can pick another engine for its line ("engine": "vibevoice" or "chatterbox"):
    workers/clone_voice.py, run in that engine's own environment beside the voices folder
    (vibevoice-venv, chatterbox-venv), clones the same reference clip. One big model has the
    card at a time: starting one stops the others.
  * A real recording instead of a designed voice: /v1/voices with "sample" (a file or URL) cuts
    up to about 20 seconds of its speech into voices/<name>.wav, with what's said in it.

A fish-speech checkpoint usually wants the whole GPU, so with --fish-dir this server runs
fish-speech's api_server itself and stops it while VoiceDesign is casting. With only
--fish-url it uses a server you run elsewhere (e.g. Drama 3 on a 48 GB NVIDIA box).

    tts_server.py --engine fish --fish-dir ~/src/fish-speech --fish-checkpoint checkpoints/s2-pro
"""

import os

# On AMD GPUs, MIOpen otherwise benchmarks every convolution for each new input length, which takes
# longer than the speech itself (each sentence is a new length). Fish inherits this.
os.environ.setdefault("MIOPEN_FIND_MODE", "FAST")

import argparse
import contextlib
import gc
import io
import json
import os
import re
import signal
import subprocess
import threading
import errno
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# A reference line with a range of sounds and some feeling, so clones have something to work with.
# What each voice's reference clip says. Fish copies the clip's delivery as well as its voice, so
# a calm, even line made every character calm and even (S2-Pro's own notes: emotion is
# "flattened by the reference"): this one is lively, with surprise, a question and a laugh.
REFERENCE_LINE = (
    "Oh, you will never guess what happened! I opened the little door, and there it was, the most "
    "wonderful garden I had ever seen. Can you believe it? I laughed and laughed, and then, very "
    "quietly, I crept inside."
)
DIRECTION = re.compile(r"\[[^\]]*\]\s*")

args = None
models = {}


class Turns:
    """The card, one generation at a time, with a way to the front of the queue: what someone
    is waiting on right now (the reading companion's answer, trying a voice) goes next, ahead
    of the full cast's lines for further on. (With a plain Lock the full cast's steady stream
    of lines kept taking the card while an answer waited behind them.)"""

    def __init__(self):
        self.cond = threading.Condition()
        self.held = False
        self.urgent_waiting = 0

    def __enter__(self):
        with self.cond:
            while self.held or self.urgent_waiting:
                self.cond.wait()
            self.held = True
        return self

    def __exit__(self, *exc):
        with self.cond:
            self.held = False
            self.cond.notify_all()
        return False

    @contextlib.contextmanager
    def first(self):
        with self.cond:
            self.urgent_waiting += 1
            try:
                while self.held:
                    self.cond.wait()
            finally:
                self.urgent_waiting -= 1
            self.held = True
        try:
            yield
        finally:
            self.__exit__()


lock = Turns()


def free_gpu():
    gc.collect()
    try:
        import torch

        torch.cuda.empty_cache()
    except Exception:
        pass


last_used = {"qwen": 0.0, "fish": 0.0, "whisper": 0.0, "vibevoice": 0.0, "chatterbox": 0.0}


def touch(what):
    last_used[what] = time.monotonic()


def free_ollama():
    """Asks a local Ollama to drop the models it holds on the graphics card, to make room."""
    if not args.free_ollama:
        return
    base = args.ollama_url.rstrip("/")
    try:
        with urllib.request.urlopen(base + "/api/ps", timeout=2) as r:
            running = json.load(r).get("models", [])
        for m in running:
            req = urllib.request.Request(
                base + "/api/generate", data=json.dumps({"model": m["name"], "keep_alive": 0}).encode(),
                headers={"Content-Type": "application/json"},
            )
            urllib.request.urlopen(req, timeout=10).read()
            print(f"asked Ollama to unload {m['name']} to make room", flush=True)
    except Exception:
        pass  # no Ollama here, or nothing loaded


def reaper():
    """Unloads whatever hasn't been used for --idle-minutes, so the GPU and memory are free for
    everything else on the computer. Each one loads again on its next use."""
    limit = args.idle_minutes * 60
    while limit > 0:
        time.sleep(30)
        now = time.monotonic()
        with lock:
            if models and now - last_used["qwen"] > limit:
                print("unloading the voice design models (idle)", flush=True)
                unload_qwen()
            if drawing_now() and now - last_used.get("drawing", now) > 300:
                print("stopping the picture drawing (idle)", flush=True)
                stop_drawing()
            if fish.proc and now - last_used["fish"] > max(limit, args.fish_idle_minutes * 60):
                print("stopping Fish Audio (idle)", flush=True)
                fish.stop()
            for name, c in cloners.items():
                if c.running() and now - last_used[name] > limit:
                    print(f"stopping {name} (idle)", flush=True)
                    c.stop()
        with listen_lock:
            if whisper and now - last_used["whisper"] > limit:
                print("unloading speech recognition (idle)", flush=True)
                whisper.clear()
                from common import release_gpu_whisper

                release_gpu_whisper()
                free_gpu()


def qwen(kind):
    """Loads a Qwen3-TTS checkpoint on first use ("design", "base" or "custom")."""
    touch("qwen")
    if kind not in models:
        free_ollama()
        import torch
        from qwen_tts import Qwen3TTSModel

        path = {"design": args.design_model, "base": args.base_model, "custom": args.custom_model}[kind]
        models[kind] = Qwen3TTSModel.from_pretrained(path, device_map=args.device, dtype=torch.bfloat16)
    return models[kind]


def qwen_gpu(kind):
    """A Qwen3-TTS checkpoint, stopping our Fish first: they can't share the card."""
    if args.engine == "fish" and args.fish_dir:
        fish.stop()
    stop_cloners()
    return qwen(kind)


def unload_qwen():
    models.clear()
    free_gpu()


class Fish:
    """A fish-speech API server: one we start and stop, or one already running at --fish-url."""

    def __init__(self):
        self.proc = None
        self.starting_since = None
        self.start_lock = threading.Lock()
        # Set when a start is called off (a voice to make first): not a broken install.
        self.cancelled = False

    def expected_start(self):
        """How long starting is likely to take: as long as last time (the first start ever tunes
        its kernels for the GPU, which takes longest)."""
        try:
            with open(os.path.join(args.voices, ".fish-start.json")) as f:
                return float(json.load(f)["seconds"])
        except (OSError, ValueError, KeyError):
            return 360.0

    def healthy(self):
        try:
            with urllib.request.urlopen(args.fish_url.rstrip("/") + "/v1/health", timeout=3) as r:
                return r.status == 200
        except Exception:
            return False

    def start(self):
        self.cancelled = False
        # One start at a time: two at once (the warm-up and a line asking together) both
        # loaded the model, ran the card out of memory and fought over the port.
        with self.start_lock:
            touch("fish")
            if self.healthy():
                return
            try:
                self._start()
            finally:
                self.starting_since = None

    def _start(self):
        # Fish needs most of a 24 GB card: everything else on it makes way first (the voice
        # designer, the picture drawing, speech recognition and the aligner, and Ollama's chat
        # model), or it runs out of memory while starting.
        stop_drawing()
        stop_cloners()
        free_ollama()
        unload_qwen()
        with listen_lock:
            whisper.clear()
            from common import release_gpu_whisper

            release_gpu_whisper()
        free_gpu()
        if not args.fish_dir:
            raise RuntimeError(f"no fish-speech server at {args.fish_url}")
        # One of ours that isn't answering (stuck, or still dying): gone first, port and memory.
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        host_port = args.fish_url.split("://", 1)[-1].rstrip("/")
        cmd = [
            args.fish_python, os.path.join(os.path.dirname(os.path.abspath(__file__)), "fish_launch.py"),
            "tools/api_server.py", "--listen", host_port,
            "--llama-checkpoint-path", args.fish_checkpoint, "--device", args.device,
        ]
        if args.fish_decoder:
            cmd += ["--decoder-checkpoint-path", args.fish_decoder]
        if not args.fish_no_compile:
            # Compiled, generation runs faster than real time; the first start takes a few minutes longer.
            cmd += ["--compile"]
        print("starting", " ".join(cmd), flush=True)
        self.starting_since = time.time()
        # Kernels autotuned for this GPU (measured on a 7900 XTX: 0.78x real time with the default
        # compile, 1.05-1.2x tuned; CUDA/HIP graphs crash under ROCm). The tuning is cached on disk,
        # so only the very first start pays for it.
        env = dict(os.environ)
        env.setdefault("DUET_FISH_COMPILE", "max-autotune-no-cudagraphs")
        # 8-bit weights: about 2x real time against 1.4x at full precision, with the same words
        # (checked by transcribing both). DUET_FISH_INT8=0 for full precision.
        env.setdefault("DUET_FISH_INT8", "1")
        self.proc = subprocess.Popen(cmd, cwd=os.path.expanduser(args.fish_dir), env=env)
        for _ in range(1200):
            if self.healthy():
                # How long this took, so the next start can show its progress against it.
                try:
                    with open(os.path.join(args.voices, ".fish-start.json"), "w") as f:
                        json.dump({"seconds": time.time() - self.starting_since}, f)
                except OSError:
                    pass
                self.starting_since = None
                return
            if self.proc.poll() is not None:
                if self.cancelled:
                    raise FishCancelled("fish-speech start called off")
                raise RuntimeError("fish-speech server exited while starting")
            time.sleep(1)
        raise RuntimeError("fish-speech server didn't come up")

    def cancel_start(self):
        """Calls off a start under way, at once, without waiting for the lock its caller holds:
        a voice that has to be made first would otherwise wait out the whole start (minutes, and
        longest the first time) only to stop Fish again straight after."""
        p = self.proc
        if self.starting_since is not None and p is not None and p.poll() is None:
            print("Fish: start called off (a voice is wanted first)", flush=True)
            self.cancelled = True
            p.terminate()

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.proc = None

    def tts(self, text, ref_wav, ref_text):
        import ormsgpack
        import soundfile as sf

        self.start()
        touch("fish")
        with open(ref_wav, "rb") as f:
            audio = f.read()
        body = ormsgpack.packb({
            "text": text,
            "references": [{"audio": audio, "text": ref_text}],
            "format": "wav",
            "temperature": args.temperature,
            "top_p": args.top_p,
            "max_new_tokens": 0,
            "use_memory_cache": "on",
            "streaming": False,
        })
        headers = {"content-type": "application/msgpack"}
        if args.fish_key:
            headers["authorization"] = f"Bearer {args.fish_key}"
        req = urllib.request.Request(args.fish_url.rstrip("/") + "/v1/tts?format=msgpack", data=body, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=900) as r:
                data = r.read()
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"fish-speech: {e.read()[:300]!r}") from e
        wav, sr = sf.read(io.BytesIO(data), dtype="float32")
        return (wav.mean(axis=1) if wav.ndim > 1 else wav), sr


class FishCancelled(RuntimeError):
    pass


fish = Fish()


class Cloner:
    """VibeVoice or Chatterbox, in its own environment (workers/clone_voice.py), started on the
    first line asked of it and kept running until idle or the card's wanted."""

    def __init__(self, name):
        self.name = name
        self.proc = None

    def python(self):
        return getattr(args, f"{self.name}_python")

    def available(self):
        return os.path.exists(self.python())

    def running(self):
        return self.proc is not None and self.proc.poll() is None

    def start(self):
        if self.running():
            return
        if not self.available():
            raise RuntimeError(f"{self.name} isn't set up ({self.python()} is missing)")
        # The whole card for it: Fish, the designers, the pictures, the other cloner, Ollama.
        fish.stop()
        stop_drawing()
        stop_cloners(keep=self.name)
        unload_qwen()
        free_ollama()
        free_gpu()
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "clone_voice.py")
        cmd = [self.python(), script, "--engine", self.name]
        model = getattr(args, f"{self.name}_model")
        if model:
            cmd += ["--model", model]
        print("starting", " ".join(cmd), flush=True)
        env = dict(os.environ)
        env.setdefault("PYTHONUNBUFFERED", "1")
        self.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1, env=env)
        for line in self.proc.stdout:
            if line.startswith("{") and json.loads(line).get("ready"):
                return
        self.proc = None
        raise RuntimeError(f"{self.name} exited while loading")

    def stop(self):
        if self.running():
            self.proc.terminate()
            try:
                self.proc.wait(30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.proc = None
        free_gpu()

    def tts(self, text, ref_wav):
        import tempfile

        import soundfile as sf

        touch(self.name)
        self.start()
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            out = f.name
        try:
            self.proc.stdin.write(json.dumps({"text": text, "ref": ref_wav, "out": out}) + "\n")
            self.proc.stdin.flush()
            for line in self.proc.stdout:
                if not line.startswith("{"):
                    continue
                msg = json.loads(line)
                if "error" in msg:
                    raise RuntimeError(f"{self.name}: {msg['error']}")
                print(f"{self.name}: {msg['audio']:.1f} s of speech in {msg['seconds']:.1f} s", flush=True)
                touch(self.name)
                wav, sr = sf.read(out, dtype="float32")
                return (wav.mean(axis=1) if wav.ndim > 1 else wav), sr
            self.proc = None
            raise RuntimeError(f"{self.name} stopped")
        finally:
            with contextlib.suppress(OSError):
                os.unlink(out)


cloners = {"vibevoice": Cloner("vibevoice"), "chatterbox": Cloner("chatterbox")}


def stop_cloners(keep=None):
    for name, c in cloners.items():
        if name != keep:
            c.stop()


def slug(name):
    return re.sub(r"[^a-z0-9_-]+", "-", name.lower()).strip("-") or "voice"


def design(voice, desc, language):
    """A new reference clip for a voice, from its description, written to voices/<voice>.wav.

    workers/design_voice.py does it, in its own environment (OmniVoice for the accents it knows,
    Maya1 for any other, best of a few takes by an accent classifier). Without that
    environment, Qwen3-TTS VoiceDesign (which only ever sounds American) makes one take.
    Returns what made it."""
    import soundfile as sf

    wav_path = os.path.join(args.voices, slug(voice) + ".wav")
    if os.path.exists(args.design_python):
        stop_drawing()
        if args.engine == "fish" and args.fish_dir:
            fish.stop()  # the designers need the card
        stop_cloners()
        unload_qwen()
        job = json.dumps({"description": desc, "text": REFERENCE_LINE, "out": wav_path})
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "design_voice.py")
        r = subprocess.run([args.design_python, script, job], stdout=subprocess.PIPE, text=True)
        if r.returncode == 0 and os.path.exists(wav_path):
            try:
                return json.loads(r.stdout.strip().splitlines()[-1])
            except Exception:
                return {}
        print(f"designing {voice} failed; using Qwen3-TTS VoiceDesign", flush=True)
    wavs, sr = qwen_gpu("design").generate_voice_design(text=REFERENCE_LINE, instruct=desc, language=language)
    sf.write(wav_path, wavs[0], sr)
    return {"engine": "qwen3-voicedesign"}


def reference(voice, description, language):
    """The reference clip for a voice, casting it the first time."""
    wav_path = os.path.join(args.voices, slug(voice) + ".wav")
    meta_path = wav_path[:-4] + ".json"
    if not os.path.exists(wav_path):
        import soundfile as sf

        os.makedirs(args.voices, exist_ok=True)
        custom = qwen_gpu("custom") if args.custom_model else None
        made = {}
        if custom is not None and voice.lower() in [s.lower() for s in custom.get_supported_speakers()]:
            wavs, sr = custom.generate_custom_voice(text=REFERENCE_LINE, speaker=voice.lower(), language=language, instruct="")
            sf.write(wav_path, wavs[0], sr)
        else:
            desc = description or "A warm, clear, expressive audiobook narrator."
            print(f"designing {voice}", flush=True)
            made = design(voice, desc, language)
        with open(meta_path, "w") as f:
            json.dump({"name": voice, "description": description, "text": REFERENCE_LINE, **made}, f)
        if args.engine == "fish":
            unload_qwen()
    ref_text = REFERENCE_LINE
    if os.path.exists(meta_path):
        with open(meta_path) as f:
            ref_text = json.load(f).get("text", REFERENCE_LINE)
    return wav_path, ref_text


def from_sample(voice, sample):
    """A voice's reference clip from a real recording (a file or a URL): mono 24 kHz, its
    leading silence trimmed, cut after the last whole sentence within about 20 seconds, and
    what's said in it (Fish needs the words of its reference)."""
    import numpy as np
    import soundfile as sf

    from common import whisper as load_whisper

    os.makedirs(args.voices, exist_ok=True)
    wav_path = os.path.join(args.voices, slug(voice) + ".wav")
    r = subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-i", os.path.expanduser(sample), "-t", "60",
         "-af", "silenceremove=start_periods=1:start_threshold=-45dB:start_silence=0.1,loudnorm=I=-20:TP=-2",
         "-f", "f32le", "-ac", "1", "-ar", "24000", "pipe:1"],
        capture_output=True,
    )
    if r.returncode != 0 or not r.stdout:
        raise RuntimeError(f"couldn't read the voice sample: {r.stderr.decode(errors='replace')[-300:]}")
    audio = np.frombuffer(r.stdout, dtype="float32")
    # (16 kHz for Whisper.)
    heard = np.interp(np.arange(0, len(audio), 1.5), np.arange(len(audio)), audio).astype("float32")
    with listen_lock:
        touch("whisper")
        model, _ = load_whisper(args.listen_model, "cpu" if args.listen_cpu else "cuda")
        segments = list(model.transcribe(heard, vad_filter=True, language="en")[0])
    if not segments:
        raise RuntimeError("no speech heard in the voice sample")
    # Whole segments up to about 20 s (at least the first).
    keep = [segments[0]]
    for seg in segments[1:]:
        if seg.end > 22:
            break
        keep.append(seg)
    end = min(keep[-1].end + 0.25, len(audio) / 24000, 30.0)
    clip = audio[: int(end * 24000)]
    text = " ".join(seg.text.strip() for seg in keep).strip()
    sf.write(wav_path, clip, 24000)
    with open(wav_path[:-4] + ".json", "w") as f:
        json.dump({"name": voice, "sample": sample, "text": text, "engine": "sample", "seconds": round(end, 2)}, f)
    print(f"{voice}: {end:.1f} s from {sample}", flush=True)


# When Fish can't start (a broken install), Qwen3-TTS reads instead until this time.
fish_broken_until = 0.0


def tighten(wav, sr, edge=0.04, longest=0.2):
    """VibeVoice and Chatterbox leave dead air in their takes: up to most of a second before a
    line starts, and stretches of true silence mid-line that play as hiccups. Trims the ends to
    `edge` seconds and shortens any silence inside to `longest`."""
    import numpy as np

    hop = max(1, int(sr * 0.01))
    n = len(wav) // hop
    if n < 3:
        return wav
    frames = np.abs(wav[: n * hop]).reshape(n, hop).max(axis=1)
    quiet = frames < 10 ** (-48 / 20)
    voiced = np.flatnonzero(~quiet)
    if not len(voiced):
        return wav
    keep_edge, keep_gap = int(edge / 0.01), int(longest / 0.01)
    first, last = max(0, voiced[0] - keep_edge), min(n, voiced[-1] + 1 + keep_edge)
    out, run = [], 0
    for i in range(first, last):
        run = run + 1 if quiet[i] else 0
        # Inside a long silence, only its first `longest` is kept.
        if run <= keep_gap or i >= voiced[-1]:
            out.append(wav[i * hop : (i + 1) * hop])
    return np.concatenate(out) if out else wav


def synthesize(text, voice, description, language, engine=None):
    global fish_broken_until
    ref_wav, ref_text = reference(voice, description, language)
    if engine in cloners:
        wav, sr = cloners[engine].tts(text, ref_wav)
        return tighten(wav, sr), sr
    if args.engine == "fish" and time.time() >= fish_broken_until:
        try:
            return fish.tts(text, ref_wav, ref_text)
        except FishCancelled:
            raise
        except Exception as e:
            fish.stop()
            fish_broken_until = time.time() + 600
            print(f"fish-speech failed ({e}); using Qwen3-TTS for the next 10 minutes", flush=True)
    plain = DIRECTION.sub("", text).strip() or text
    wavs, sr = qwen("base").generate_voice_clone(text=plain, language=language, ref_audio=ref_wav, ref_text=ref_text)
    return wavs[0], sr


whisper = {}


def transcribe(audio, model_name):
    """Speech recognition for the microphone: Whisper on the graphics card when there is one
    (NVIDIA or AMD), otherwise faster-whisper on the CPU."""
    touch("whisper")
    import tempfile

    from common import load_audio, whisper as load_whisper

    with tempfile.NamedTemporaryFile(suffix=".audio") as f:
        f.write(audio)
        f.flush()
        samples = load_audio(f.name)
    with listen_lock:
        key = "dictation:" + model_name
        if key not in whisper:
            whisper[key], _ = load_whisper(model_name, "cpu" if args.listen_cpu else "cuda")
        segments, _ = whisper[key].transcribe(samples, vad_filter=True)
        return " ".join(seg.text.strip() for seg in segments).strip()


listen_lock = threading.Lock()
books = {}


def book_index(path):
    """A book's words and phrase index, loaded once per book (reloaded if the file changes)."""
    import align

    mtime = os.path.getmtime(path)
    cached = books.get(path)
    if cached and cached[0] == mtime:
        return cached[1], cached[2]
    with open(path, encoding="utf-8") as f:
        bw = align.book_words(json.load(f))
    idx = align.index_book([w for w, _, _ in bw])
    books[path] = (mtime, bw, idx)
    return bw, idx


# The pictures being drawn (a subprocess), so listening can take the card from it.
drawing = None


def stop_drawing():
    """Listening comes first (the reader is waiting on it): pictures being drawn give up the
    card, and carry on later from where they got to."""
    global drawing
    p = drawing
    if p is not None and p.poll() is None:
        print("pictures: drawing stopped (the card's wanted)", flush=True)
        p.terminate()
        try:
            p.wait(20)
        except subprocess.TimeoutExpired:
            p.kill()
        free_gpu()


def drawing_now():
    return drawing is not None and drawing.poll() is None


# The card, taken in turns: the reader's audio (lining the recording up with the book) and the
# pictures never run on it at the same moment. Running both at once on ROCm gave garbage (a
# picture of blue blocks on black; the aligner's scores out of range), and so did freezing the
# drawing mid-picture. Pictures are drawn one at a time; listening waits for the picture being
# drawn (a few seconds) and none starts while anyone's listening.
card = threading.Condition()
listening = 0
drawing_one = False


@contextlib.contextmanager
def audio_first():
    """The reader's audio comes before the pictures: no new picture starts while this runs."""
    global listening
    with card:
        listening += 1
        while drawing_one:
            card.wait()
    try:
        yield
    finally:
        with card:
            listening -= 1
            card.notify_all()


def draw(body):
    """Pictures from the drawing process, which is kept running between batches (Z-Image takes a
    minute or more to load); started when needed, and stopped when idle or the card's wanted."""
    global drawing
    touch("drawing")
    if body.get("model") != "z-image":
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "illustrate.py")
        r = subprocess.run([args.design_python, script, json.dumps(body)], stdout=subprocess.PIPE, text=True)
        return [json.loads(l) for l in r.stdout.splitlines() if l.startswith("{")] if r.returncode == 0 else None
    if not drawing_now():
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "illustrate.py")
        drawing = subprocess.Popen([args.design_python, script, "--serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
        line = drawing.stdout.readline()
        if not line:
            drawing = None
            return None
    global drawing_one
    proc = drawing
    made = []
    for job in body.get("jobs", []):
        # (Each picture in its turn: after anyone listening.)
        with card:
            while listening:
                card.wait()
            drawing_one = True
        try:
            proc.stdin.write(json.dumps({**body, "jobs": [job]}) + "\n")
            proc.stdin.flush()
            for line in proc.stdout:
                if not line.startswith("{"):
                    continue
                msg = json.loads(line)
                if msg.get("done"):
                    touch("drawing")
                    break
                if "out" in msg:
                    made.append(msg)
            else:
                return None
        except (BrokenPipeError, OSError, ValueError):
            return None
        finally:
            with card:
                drawing_one = False
                card.notify_all()
    return made


def listen_aligner():
    """The forced aligner, on the card (taking turns with the pictures)."""
    from common import aligner

    return aligner()


def listen_model():
    """Whisper for finding where in the book the audio is: on the graphics card when there is
    one. Pictures being drawn there wait their turn while it listens (see audio_first), rather than the
    listening moving to the processor, which left the reader waiting on the audio while the
    pictures had the card. (It's small: it fits beside the drawing.) Exact word times come from
    the forced aligner, not from Whisper."""
    from common import whisper as load_whisper

    key = "live:" + args.listen_model
    if key not in whisper:
        whisper[key], _ = load_whisper(args.listen_model, "cpu" if args.listen_cpu else "cuda")
    return whisper[key]


def sync_window(body):
    with audio_first():
        return _sync_window(body)


def _sync_window(body):
    """Hears a stretch of audio, finds it in the book, and times every word of the sentences it
    covers: Whisper says roughly what's being read; the book's own text for those sentences is
    then lined up against the audio by the forced aligner."""
    import align
    from common import aligner, load_audio_window, words

    bw, idx = book_index(body["book"])
    start, duration = float(body["start"]), float(body.get("duration", 30.0))
    track = body.get("track", 0)
    first, last = {}, {}
    for i, (_, ch, si) in enumerate(bw):
        first.setdefault((ch, si), i)
        last[(ch, si)] = i
    # A sentence longer than the stretch would never fit in one: listen to more until one does.
    while True:
        samples = load_audio_window(body["audio"], start, duration)
        with listen_lock:
            touch("whisper")
            segments, _ = listen_model().transcribe(samples, vad_filter=False, language="en")
        heard = [(t, track, start, start + duration) for seg in segments for t in words(seg.text)]
        if not heard:
            return {"spans": [], "near": body.get("near"), "heard": 0}
        mapping = align.align(bw, heard, near=int(body.get("near") or 0), idx=idx)
        if not mapping:
            return {"spans": [], "near": body.get("near"), "heard": len(heard)}
        # Where it was heard: the matched words, less any stray match far from the rest.
        matched = sorted(mapping)
        mid = matched[len(matched) // 2]
        # (And only in the chapter most of it was heard in: a heading read out at the start of a
        # track can match the contents page.)
        matched = [m for m in matched if abs(m - mid) < 6 * len(heard) + 40 and bw[m][1] == bw[mid][1]]
        lo, hi = matched[0], matched[-1]
        # The sentences heard, whole: from the one the first matched word is in to the one the
        # last is in, leaving out one cut off by either end of the stretch (the next stretch
        # starts where this one's sentences end, so it gets it whole).
        order = []
        for i in range(first[bw[lo][1:]], last[bw[hi][1:]] + 1):
            key = bw[i][1:]
            if not order or order[-1] != key:
                order.append(key)
        if order and first[order[0]] < lo - 4 and start > 0.5:
            order = order[1:]
        # The last sentence counts as cut off unless it was heard to its last word or so: words
        # the aligner had to fit into no audio would get made-up times.
        cut = bool(order) and last[order[-1]] > hi + 1
        whole = sum(last[k] - first[k] + 1 for k in order[:-1])
        if cut and whole < 0.5 * len(heard) and duration < 150:
            # Little of it whole: a long sentence runs past its end (and the next stretch would
            # start no further on). Listen further, to the end of it.
            duration = min(150.0, duration * 2)
            continue
        # (A cut-off last sentence is still lined up, so the one before it ends where the audio
        # says; its own times are left out.)
        tail = order[-1:] if cut else []
        if cut:
            order = order[:-1]
        break
    if not order:
        return {"spans": [], "near": max(mapping), "heard": len(heard)}
    span_words = [bw[i][0] for key in order for i in range(first[key], last[key] + 1)]
    # (Of a cut-off last sentence, only the words that were actually heard.)
    span_words += [bw[i][0] for key in tail for i in range(first[key], min(last[key], hi) + 1)]
    with listen_lock:
        times = listen_aligner().align(samples, span_words)
    spans, k = [], 0
    for ch, si in order:
        n = last[(ch, si)] - first[(ch, si)] + 1
        ts = times[k:k + n]
        k += n
        known = [t for t in ts if t]
        if not known:
            continue
        s0, e0 = known[0][0] + start, known[-1][1] + start
        word_at = {j: (track, t[0] + start) for j, t in enumerate(ts) if t}
        spans.append({"chapter": ch, "sentence": si, "track": track, "start": round(s0, 3), "end": round(e0, 3),
                      "words": align.word_times(word_at, n, track, s0, e0)})
    return {"spans": spans, "near": max(mapping), "heard": len(heard)}


def align_words(body):
    # (A performed line's words, timed while the next lines are performed: in turn with Fish,
    # not beside it. Run together they slowed Fish by a fifth, 51 tokens a second to 40, and
    # two models computing at once on this card has given garbage before.)
    with lock, audio_first():
        return _align_words(body)


def _align_words(body):
    """When each word of `text` starts in the recording at `audio` (a performed line): the words
    as the reader counts them ([\\w']+, apostrophes trimmed), timed by the forced aligner, the
    ones it can't spell (numbers) placed between their neighbours."""
    import align
    from common import WORD, aligner, load_audio

    words = [w.strip("'") for w in WORD.findall(body["text"]) if w.strip("'")]
    samples = load_audio(body["audio"])
    with listen_lock:
        touch("whisper")
        times = listen_aligner().align(samples, words)
    heard = {i: (0, t[0]) for i, t in enumerate(times) if t}
    end = len(samples) / 16000
    return {"words": align.word_times(heard, len(words), 0, 0.0, end)}


def track_words(body):
    with audio_first():
        return _track_words(body)


def _track_words(body):
    """Transcribes a whole track with word timings, for making a book from an audiobook:
    segments of {start, end, text, words: [[word, start, end]]}. Whisper writes the text (on the
    graphics card when there is one); each segment's words are then timed exactly by the forced
    aligner. Cached next to align.py's."""
    import align
    from common import aligner, load_audio, load, save

    path = body["audio"]
    cached = align.cache_path(path, "segments-aligned:" + args.listen_model)
    if os.path.exists(cached):
        return {"segments": load(cached)}
    samples = load_audio(path)
    rate = 16000
    with listen_lock:
        touch("whisper")
        segments, _ = listen_model().transcribe(samples, vad_filter=True, language="en")
        out = []
        for seg in segments:
            text = seg.text.strip()
            if not text:
                continue
            # A little either side, as Whisper's segment edges are rough.
            a = max(0.0, seg.start - 0.6)
            b = min(len(samples) / rate, seg.end + 0.6)
            ws = text.split()
            times = listen_aligner().align(samples[int(a * rate):int(b * rate)], ws)
            timed = [[w, round(t[0] + a, 3), round(t[1] + a, 3)] for w, t in zip(ws, times) if t]
            out.append({"start": round(timed[0][1] if timed else seg.start, 3), "end": round(timed[-1][2] if timed else seg.end, 3),
                        "text": text, "words": timed})
    save(cached, out)
    return {"segments": out}


def multipart(content_type, body):
    """Fields of a multipart/form-data body: name -> bytes."""
    from email.parser import BytesParser
    from email.policy import HTTP

    msg = BytesParser(policy=HTTP).parsebytes(f"Content-Type: {content_type}\r\n\r\n".encode() + body)
    return {part.get_param("name", header="content-disposition"): part.get_payload(decode=True) for part in msg.iter_parts()}


def encode(wav, sr, fmt):
    import soundfile as sf

    buf = io.BytesIO()
    sf.write(buf, wav, sr, format="WAV")
    if fmt == "wav":
        return buf.getvalue(), "audio/wav"
    codec = {"mp3": ["-f", "mp3", "-b:a", "128k"], "opus": ["-f", "ogg", "-c:a", "libopus"], "flac": ["-f", "flac"]}.get(fmt, ["-f", "mp3"])
    out = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", "pipe:0", *codec, "pipe:1"], input=buf.getvalue(), capture_output=True, check=True)
    return out.stdout, {"opus": "audio/ogg", "flac": "audio/flac"}.get(fmt, "audio/mpeg")


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.rstrip("/") in ("/v1/status", "/status"):
            since = fish.starting_since
            body = {"fish_starting": since is not None, "fish_ready": since is None and fish.proc is not None and fish.proc.poll() is None}
            if since is not None:
                body.update(elapsed=time.time() - since, expected=fish.expected_start())
            return self.reply(200, json.dumps(body).encode(), "application/json")
        self.reply(404, b'{"error":"not found"}', "application/json")

    def reply(self, code, data, mime):
        self.send_response(code)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        path = self.path.rstrip("/")
        try:
            raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            if path in ("/v1/audio/transcriptions", "/audio/transcriptions"):
                fields = multipart(self.headers.get("Content-Type", ""), raw)
                model_name = (fields.get("model") or b"large-v3-turbo").decode()
                text = transcribe(fields["file"], model_name.rsplit("/", 1)[-1].removeprefix("faster-whisper-"))
                return self.reply(200, json.dumps({"text": text}).encode(), "application/json")
            body = json.loads(raw)
            if path in ("/v1/warm", "/warm"):
                # Someone's about to want the full cast: have Fish loaded (in the background) and
                # keep it from being unloaded as idle while they're there.
                touch("fish")
                if body.get("start", True) and not fish.healthy() and fish.starting_since is None and time.time() >= fish_broken_until:
                    # The pictures give up the card at once (they carry on later from where they
                    # got to) rather than Fish waiting for their whole batch: someone's about to
                    # listen, and that came out at two minutes before the voices even began.
                    stop_drawing()

                    def start():
                        global fish_broken_until
                        try:
                            with lock:
                                # (The full cast comes before the pictures: they don't both fit.)
                                stop_drawing()
                                fish.start()
                        except FishCancelled:
                            pass
                        except Exception as e:
                            # (The lines asked for next go to Qwen3-TTS straight away, rather
                            # than waiting minutes on another start that fails the same way.)
                            fish_broken_until = time.time() + 600
                            print(f"warming fish: {e}; using Qwen3-TTS for the next 10 minutes", flush=True)
                    threading.Thread(target=start, daemon=True).start()
                return self.reply(200, b'{"ok":true}', "application/json")
            if path in ("/v1/align-words", "/align-words"):
                return self.reply(200, json.dumps(align_words(body)).encode(), "application/json")
            if path in ("/v1/track-words", "/track-words"):
                return self.reply(200, json.dumps(track_words(body)).encode(), "application/json")
            if path in ("/v1/sync-window", "/sync-window"):
                return self.reply(200, json.dumps(sync_window(body)).encode(), "application/json")
            language = body.get("language", "English")
            if path in ("/v1/shutdown", "/shutdown"):
                self.reply(200, b'{"ok":true}', "application/json")
                fish.stop()
                stop_cloners()
                threading.Thread(target=lambda: os._exit(0), daemon=True).start()
                return
            if path in ("/v1/illustrate", "/illustrate"):
                # A book's pictures (Stable Diffusion XL, in the designer's environment): the
                # card is cleared for it first, as for designing a voice.
                with lock:
                    if args.engine == "fish" and args.fish_dir:
                        fish.stop()
                    stop_cloners()
                    unload_qwen()
                    # (Speech recognition stays: it's small, and the reader's audio is lined up
                    # with it while the pictures pause.)
                    free_ollama()
                    free_gpu()
                    made = draw(body)
                if made is None:
                    return self.reply(503, json.dumps({"error": "drawing stopped"}).encode(), "application/json")
                return self.reply(200, json.dumps({"made": made}).encode(), "application/json")
            if path in ("/v1/voices", "/voices"):
                # Cast voices ahead of time, so a long recording never switches models midway.
                # (Making one needs the card: a Fish start under way is called off first rather
                # than waited for.)
                if any(v.get("replace") or not os.path.exists(os.path.join(args.voices, slug(v["voice"]) + ".wav")) for v in body.get("voices", [])):
                    fish.cancel_start()
                with lock:
                    for v in body.get("voices", []):
                        if v.get("replace"):
                            # Cast again: the old clip goes, and a new one is made from the description.
                            for ext in (".wav", ".json"):
                                old = os.path.join(args.voices, slug(v["voice"]) + ext)
                                if os.path.exists(old):
                                    os.replace(old, old + ".old")
                        if v.get("sample"):
                            from_sample(v["voice"], v["sample"])
                        else:
                            reference(v["voice"], v.get("instructions"), language)
                return self.reply(200, b'{"ok":true}', "application/json")
            if path in ("/v1/audio/script", "/audio/script"):
                # Several voices in one clip (a sentence with dialogue and narration), for the live full cast.
                import numpy as np

                out, rate = [], None
                with lock:
                    for p in body.get("pieces", []):
                        wav, sr = synthesize(p["input"], p.get("voice", "narrator"), p.get("instructions"), language, p.get("engine", body.get("engine")))
                        wav = np.asarray(wav, dtype="float32")
                        if rate is None:
                            rate = sr
                        elif sr != rate:
                            x = np.linspace(0, len(wav), int(len(wav) * rate / sr), endpoint=False)
                            wav = np.interp(x, np.arange(len(wav)), wav).astype("float32")
                        out += [wav, np.zeros(int(0.12 * rate), dtype="float32")]
                rate = rate or 24000
                out.append(np.zeros(int(float(body.get("pause", 0.3)) * rate), dtype="float32"))
                data, mime = encode(np.concatenate(out), rate, body.get("response_format", "mp3"))
                return self.reply(200, data, mime)
            if path not in ("/v1/audio/speech", "/audio/speech"):
                return self.send_error(404)
            # One generation at a time on the GPU; someone waiting on this one goes first.
            with (lock.first() if body.get("priority") else lock):
                wav, sr = synthesize(body["input"], body.get("voice", "narrator"), body.get("instructions"), language, body.get("engine"))
            data, mime = encode(wav, sr, body.get("response_format", "mp3"))
            self.reply(200, data, mime)
        except Exception as e:  # report to the client instead of dropping the connection
            import traceback

            traceback.print_exc()
            self.reply(500, json.dumps({"error": str(e)}).encode(), "application/json")


def compile_caches():
    """Somewhere writable, and kept, for the GPU kernels Fish (and the others) compile. Triton's
    default is ~/.triton and Inductor's /tmp: in the duet container the home folder isn't
    writable (Fish failed every start with "Permission denied: '/home/ben/.triton'", after
    minutes of loading, and the full cast fell back to Qwen3-TTS) and /tmp goes with each
    deploy (so every start would tune its kernels all over again). ~/.cache is kept; failing
    that, beside the voices."""
    candidates = [os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache"),
                  os.path.join(os.path.dirname(os.path.abspath(args.voices)), "cache")]
    for base in candidates:
        try:
            os.makedirs(os.path.join(base, "triton"), exist_ok=True)
            os.makedirs(os.path.join(base, "torchinductor"), exist_ok=True)
            probe = os.path.join(base, "triton", ".writable")
            with open(probe, "w"):
                pass
            os.unlink(probe)
        except OSError:
            continue
        os.environ.setdefault("TRITON_CACHE_DIR", os.path.join(base, "triton"))
        os.environ.setdefault("TORCHINDUCTOR_CACHE_DIR", os.path.join(base, "torchinductor"))
        print(f"compiled kernels kept in {base}", flush=True)
        return


def main():
    global args
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8880)
    ap.add_argument("--voices", default="voices")
    ap.add_argument("--device", default="cuda:0", help="ROCm builds of torch also call the GPU cuda:0")
    ap.add_argument("--engine", choices=["fish", "qwen"], default="fish")
    # Not 8080: that's qBittorrent's usual port.
    ap.add_argument("--fish-url", default="http://127.0.0.1:8881")
    ap.add_argument("--fish-key", default="")
    ap.add_argument("--fish-dir", default="", help="fish-speech checkout; when set, this server starts and stops it")
    ap.add_argument("--fish-python", default="python")
    ap.add_argument("--fish-checkpoint", default="checkpoints/s2-pro", help="checkpoints/s2-pro (24 GB, ROCm or CUDA) or checkpoints/drama-3 (48 GB NVIDIA)")
    ap.add_argument("--fish-no-compile", action="store_true", help="skip torch.compile (faster start, slower speech)")
    ap.add_argument("--fish-decoder", default="", help="codec path, e.g. checkpoints/s2-pro/codec.pth")
    ap.add_argument("--listen-model", default="small.en", help="faster-whisper model for syncing while you listen")
    ap.add_argument("--listen-cpu", action="store_true", help="speech recognition on the CPU even when there's a GPU")
    ap.add_argument("--temperature", type=float, default=0.8)
    ap.add_argument("--top-p", type=float, default=0.8)
    ap.add_argument("--design-model", default="Qwen/Qwen3-TTS-12Hz-1.7B-VoiceDesign")
    ap.add_argument("--design-python", default="", help="python of the voice designer environment (default: design-venv beside --voices)")
    ap.add_argument("--base-model", default="Qwen/Qwen3-TTS-12Hz-1.7B-Base")
    ap.add_argument("--custom-model", default="", help="Qwen3-TTS CustomVoice, to allow its built-in speaker names")
    ap.add_argument("--vibevoice-python", default="", help="python of the VibeVoice environment (default: vibevoice-venv beside --voices)")
    ap.add_argument("--vibevoice-model", default="", help="default microsoft/VibeVoice-1.5B")
    ap.add_argument("--chatterbox-python", default="", help="python of the Chatterbox environment (default: chatterbox-venv beside --voices)")
    ap.add_argument("--chatterbox-model", default="")
    ap.add_argument("--idle-minutes", type=float, default=5, help="unload models unused for this long (0: never)")
    # Fish takes two or three minutes to start, so it's kept longer: coming back to the book
    # within the half hour, the first line plays in seconds. (Anything else that wants the card,
    # the pictures, a voice designer, Ollama, still has it stopped at once.)
    ap.add_argument("--fish-idle-minutes", type=float, default=30, help="stop Fish after this long unused")
    ap.add_argument("--ollama-url", default="http://127.0.0.1:11434")
    ap.add_argument("--no-free-ollama", dest="free_ollama", action="store_false",
                    help="don't ask Ollama to unload its models before loading voice models")
    args = ap.parse_args()
    if not args.design_python:
        args.design_python = os.path.join(os.path.dirname(os.path.abspath(args.voices)), "design-venv", "bin", "python")
    for name in ("vibevoice", "chatterbox"):
        if not getattr(args, f"{name}_python"):
            setattr(args, f"{name}_python", os.path.join(os.path.dirname(os.path.abspath(args.voices)), f"{name}-venv", "bin", "python"))
    compile_caches()
    print(f"duet TTS ({args.engine}) on http://{args.host}:{args.port}/v1", flush=True)
    threading.Thread(target=reaper, daemon=True).start()

    # Take the Fish server down too, however this one is stopped, rather than leave it holding the GPU.
    
    def stop(signum, frame):
        fish.stop()
        stop_cloners()
        os._exit(0)

    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, stop)
    # When duet restarts, the old voice server may still be letting go of the port: wait for it.
    for attempt in range(60):
        try:
            httpd = ThreadingHTTPServer((args.host, args.port), Handler)
            break
        except OSError as e:
            if e.errno != errno.EADDRINUSE or attempt == 59:
                raise
            time.sleep(1)
    try:
        httpd.serve_forever()
    finally:
        fish.stop()


if __name__ == "__main__":
    main()
