#!/usr/bin/env python3
"""An OpenAI-compatible text-to-speech server for duet, built on Qwen3-TTS.

POST /v1/audio/speech {"input", "voice", "instructions"?, "response_format"?}

Voices are designed from a description and then kept consistent by cloning:
  * the first time a voice name is used with "instructions" (a character description such as
    "gravelly old sailor, Cornish accent, slow and warm"), VoiceDesign speaks a reference line in
    that voice and it's saved to voices/<name>.wav;
  * every later request for that name clones the saved reference with the Base model;
  * a built-in CustomVoice speaker name (e.g. "ryan", "aiden") uses that speaker directly.

    tts_server.py [--port 8880] [--voices voices] [--device cuda:0]
"""

import argparse
import io
import json
import os
import re
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REFERENCE_LINE = (
    "It was late in the evening when the letter finally arrived, and I read it twice before I understood what it meant."
)

args = None
models = {}
lock = threading.Lock()


def model(kind):
    """Loads a Qwen3-TTS checkpoint on first use (design, base or custom)."""
    if kind not in models:
        import torch
        from qwen_tts import Qwen3TTSModel

        path = {"design": args.design_model, "base": args.base_model, "custom": args.custom_model}[kind]
        models[kind] = Qwen3TTSModel.from_pretrained(path, device_map=args.device, dtype=torch.bfloat16)
    return models[kind]


def slug(name):
    return re.sub(r"[^a-z0-9_-]+", "-", name.lower()).strip("-") or "voice"


def synthesize(text, voice, instructions, language):
    ref = os.path.join(args.voices, slug(voice) + ".wav")
    if not os.path.exists(ref):
        custom = model("custom") if args.custom_model else None
        if custom is not None and voice.lower() in [s.lower() for s in custom.get_supported_speakers()]:
            wavs, sr = custom.generate_custom_voice(text=text, speaker=voice.lower(), language=language, instruct=instructions or "")
            return wavs[0], sr
        if not instructions:
            instructions = "A clear, warm, neutral audiobook narrator."
        wavs, sr = model("design").generate_voice_design(text=REFERENCE_LINE, instruct=instructions, language=language)
        import soundfile as sf

        os.makedirs(args.voices, exist_ok=True)
        sf.write(ref, wavs[0], sr)
        with open(ref[:-4] + ".json", "w") as f:
            json.dump({"name": voice, "description": instructions, "text": REFERENCE_LINE}, f)
    with open(ref[:-4] + ".json") as f:
        ref_text = json.load(f)["text"]
    wavs, sr = model("base").generate_voice_clone(text=text, language=language, ref_audio=ref, ref_text=ref_text)
    return wavs[0], sr


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
    def do_POST(self):
        if self.path.rstrip("/") not in ("/v1/audio/speech", "/audio/speech"):
            return self.send_error(404)
        try:
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
            with lock:  # one generation at a time on the GPU
                wav, sr = synthesize(body["input"], body.get("voice", "narrator"), body.get("instructions"), body.get("language", "English"))
            data, mime = encode(wav, sr, body.get("response_format", "mp3"))
        except Exception as e:  # report to the client instead of dropping the connection
            msg = json.dumps({"error": str(e)}).encode()
            self.send_response(500)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(msg)))
            self.end_headers()
            self.wfile.write(msg)
            return
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    global args
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8880)
    ap.add_argument("--voices", default="voices")
    ap.add_argument("--device", default="cuda:0")
    ap.add_argument("--design-model", default="Qwen/Qwen3-TTS-12Hz-1.7B-VoiceDesign")
    ap.add_argument("--base-model", default="Qwen/Qwen3-TTS-12Hz-1.7B-Base")
    ap.add_argument("--custom-model", default="Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice", help="empty to disable built-in speakers")
    args = ap.parse_args()
    print(f"duet TTS on http://{args.host}:{args.port}/v1", flush=True)
    ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
