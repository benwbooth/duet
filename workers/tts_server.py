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
import gc
import io
import json
import os
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# A reference line with a range of sounds and some feeling, so clones have something to work with.
REFERENCE_LINE = (
    "It was late in the evening when the letter finally arrived. I read it twice, and then, "
    "very slowly, I began to laugh, because after all those years, it simply said: come home."
)
DIRECTION = re.compile(r"\[[^\]]*\]\s*")

args = None
models = {}
lock = threading.Lock()


def free_gpu():
    gc.collect()
    try:
        import torch

        torch.cuda.empty_cache()
    except Exception:
        pass


last_used = {"qwen": 0.0, "fish": 0.0, "whisper": 0.0}


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
            if fish.proc and now - last_used["fish"] > limit:
                print("stopping Fish Audio (idle)", flush=True)
                fish.stop()
        with listen_lock:
            if whisper and now - last_used["whisper"] > limit:
                print("unloading speech recognition (idle)", flush=True)
                whisper.clear()
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


def unload_qwen():
    models.clear()
    free_gpu()


class Fish:
    """A fish-speech API server: one we start and stop, or one already running at --fish-url."""

    def __init__(self):
        self.proc = None

    def healthy(self):
        try:
            with urllib.request.urlopen(args.fish_url.rstrip("/") + "/v1/health", timeout=3) as r:
                return r.status == 200
        except Exception:
            return False

    def start(self):
        touch("fish")
        if self.healthy():
            return
        free_ollama()
        if not args.fish_dir:
            raise RuntimeError(f"no fish-speech server at {args.fish_url}")
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
        self.proc = subprocess.Popen(cmd, cwd=os.path.expanduser(args.fish_dir), env=os.environ)
        for _ in range(600):
            if self.healthy():
                return
            if self.proc.poll() is not None:
                raise RuntimeError("fish-speech server exited while starting")
            time.sleep(1)
        raise RuntimeError("fish-speech server didn't come up")

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


fish = Fish()


def slug(name):
    return re.sub(r"[^a-z0-9_-]+", "-", name.lower()).strip("-") or "voice"


def reference(voice, description, language):
    """The reference clip for a voice, casting it with VoiceDesign the first time."""
    wav_path = os.path.join(args.voices, slug(voice) + ".wav")
    meta_path = wav_path[:-4] + ".json"
    if not os.path.exists(wav_path):
        import soundfile as sf

        if args.engine == "fish" and args.fish_dir:
            fish.stop()  # VoiceDesign needs the GPU
        custom = qwen("custom") if args.custom_model else None
        if custom is not None and voice.lower() in [s.lower() for s in custom.get_supported_speakers()]:
            wavs, sr = custom.generate_custom_voice(text=REFERENCE_LINE, speaker=voice.lower(), language=language, instruct="")
        else:
            desc = description or "A warm, clear, expressive audiobook narrator."
            wavs, sr = qwen("design").generate_voice_design(text=REFERENCE_LINE, instruct=desc, language=language)
        os.makedirs(args.voices, exist_ok=True)
        sf.write(wav_path, wavs[0], sr)
        with open(meta_path, "w") as f:
            json.dump({"name": voice, "description": description, "text": REFERENCE_LINE}, f)
        if args.engine == "fish":
            unload_qwen()
    ref_text = REFERENCE_LINE
    if os.path.exists(meta_path):
        with open(meta_path) as f:
            ref_text = json.load(f).get("text", REFERENCE_LINE)
    return wav_path, ref_text


# When Fish can't start (a broken install), Qwen3-TTS reads instead until this time.
fish_broken_until = 0.0


def synthesize(text, voice, description, language):
    global fish_broken_until
    ref_wav, ref_text = reference(voice, description, language)
    if args.engine == "fish" and time.time() >= fish_broken_until:
        try:
            return fish.tts(text, ref_wav, ref_text)
        except Exception as e:
            fish.stop()
            fish_broken_until = time.time() + 600
            print(f"fish-speech failed ({e}); using Qwen3-TTS for the next 10 minutes", flush=True)
    plain = DIRECTION.sub("", text).strip() or text
    wavs, sr = qwen("base").generate_voice_clone(text=plain, language=language, ref_audio=ref_wav, ref_text=ref_text)
    return wavs[0], sr


whisper = {}


def transcribe(audio, model_name):
    """faster-whisper on the GPU when CTranslate2 has one (NVIDIA), otherwise the CPU."""
    touch("whisper")
    import tempfile

    from faster_whisper import WhisperModel

    if model_name not in whisper:
        try:
            whisper[model_name] = WhisperModel(model_name, device="cuda", compute_type="float16")
        except Exception:
            whisper[model_name] = WhisperModel(model_name, device="cpu", compute_type="int8")
    with tempfile.NamedTemporaryFile(suffix=".audio") as f:
        f.write(audio)
        f.flush()
        from common import load_audio

        segments, _ = whisper[model_name].transcribe(load_audio(f.name), vad_filter=True)
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


def sync_window(body):
    """Transcribes a stretch of audio and returns the sentences it covers, with times."""
    import align
    from common import load_audio_window, whisper as load_whisper, words

    bw, idx = book_index(body["book"])
    start, duration = float(body["start"]), float(body.get("duration", 30.0))
    samples = load_audio_window(body["audio"], start, duration)
    with listen_lock:
        touch("whisper")
        key = "live:" + args.listen_model
        if key not in whisper:
            whisper[key], _ = load_whisper(args.listen_model, "cpu" if args.listen_cpu else "cuda")
        segments, _ = whisper[key].transcribe(samples, word_timestamps=True, vad_filter=False, language="en")
        heard = []
        for seg in segments:
            for w in seg.words or []:
                for token in words(w.word):
                    heard.append((token, body.get("track", 0), start + w.start, start + w.end))
    if not heard:
        return {"spans": [], "near": body.get("near"), "heard": 0}
    mapping = align.align(bw, heard, near=int(body.get("near") or 0), idx=idx)
    spans = align.spans(bw, heard, mapping)
    near = max(mapping) if mapping else body.get("near")
    # Only sentences actually heard in this stretch (interpolation can't reach outside it).
    spans = [sp for sp in spans if start - 1 <= sp["start"] <= start + duration + 1]
    return {"spans": spans, "near": near, "heard": len(heard)}


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
            if path in ("/v1/sync-window", "/sync-window"):
                return self.reply(200, json.dumps(sync_window(body)).encode(), "application/json")
            language = body.get("language", "English")
            if path in ("/v1/shutdown", "/shutdown"):
                self.reply(200, b'{"ok":true}', "application/json")
                fish.stop()
                threading.Thread(target=lambda: os._exit(0), daemon=True).start()
                return
            if path in ("/v1/voices", "/voices"):
                # Cast voices ahead of time, so a long recording never switches models midway.
                with lock:
                    for v in body.get("voices", []):
                        reference(v["voice"], v.get("instructions"), language)
                return self.reply(200, b'{"ok":true}', "application/json")
            if path in ("/v1/audio/script", "/audio/script"):
                # Several voices in one clip (a sentence with dialogue and narration), for the live full cast.
                import numpy as np

                out, rate = [], None
                with lock:
                    for p in body.get("pieces", []):
                        wav, sr = synthesize(p["input"], p.get("voice", "narrator"), p.get("instructions"), language)
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
            with lock:  # one generation at a time on the GPU
                wav, sr = synthesize(body["input"], body.get("voice", "narrator"), body.get("instructions"), language)
            data, mime = encode(wav, sr, body.get("response_format", "mp3"))
            self.reply(200, data, mime)
        except Exception as e:  # report to the client instead of dropping the connection
            self.reply(500, json.dumps({"error": str(e)}).encode(), "application/json")


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
    ap.add_argument("--listen-cpu", action="store_true", default=True, help="listen on the CPU (CTranslate2 has no ROCm support)")
    ap.add_argument("--temperature", type=float, default=0.7)
    ap.add_argument("--top-p", type=float, default=0.7)
    ap.add_argument("--design-model", default="Qwen/Qwen3-TTS-12Hz-1.7B-VoiceDesign")
    ap.add_argument("--base-model", default="Qwen/Qwen3-TTS-12Hz-1.7B-Base")
    ap.add_argument("--custom-model", default="", help="Qwen3-TTS CustomVoice, to allow its built-in speaker names")
    ap.add_argument("--idle-minutes", type=float, default=5, help="unload models unused for this long (0: never)")
    ap.add_argument("--ollama-url", default="http://127.0.0.1:11434")
    ap.add_argument("--no-free-ollama", dest="free_ollama", action="store_false",
                    help="don't ask Ollama to unload its models before loading voice models")
    args = ap.parse_args()
    print(f"duet TTS ({args.engine}) on http://{args.host}:{args.port}/v1", flush=True)
    threading.Thread(target=reaper, daemon=True).start()

    # Take the Fish server down too, however this one is stopped, rather than leave it holding the GPU.
    import signal

    def stop(signum, frame):
        fish.stop()
        os._exit(0)

    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, stop)
    try:
        ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()
    finally:
        fish.stop()


if __name__ == "__main__":
    main()
