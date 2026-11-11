"""Performs lines in a voice cloned from its reference clip, with VibeVoice or Chatterbox.

Run by tts_server.py in the engine's own environment (data/vibevoice-venv, data/chatterbox-venv:
each pins its own transformers), kept running between lines:

    clone_voice.py --engine vibevoice|chatterbox [--model ...]

reads one job per line on stdin, {"text", "ref", "out"}, writes the line to `out` (a wav) and
answers {"out", "seconds"} (or {"error"}) on stdout. It prints {"ready": true} once the model
is loaded.

  * VibeVoice (Microsoft; the community fork, as Microsoft took its own code down) was made for
    long-form podcast speech: a whole paragraph at once, with natural breaths and pacing.
  * Chatterbox (Resemble AI) is quick and lifelike, a sentence or two at a time: longer text is
    read in pieces and joined.
Neither takes acting directions, so "[warmly]" and the like are dropped.
"""

import argparse
import json
import re
import sys
import time

import numpy as np
import soundfile as sf
import torch

DIRECTION = re.compile(r"\[[^\]]*\]\s*")


def plain(text):
    return re.sub(r"\s+", " ", DIRECTION.sub("", text)).strip() or text


def pieces(text, limit=260):
    """Whole sentences, gathered into pieces no longer than `limit` characters where possible."""
    out, cur = [], ""
    for s in re.split(r"(?<=[.!?…])\s+", text):
        if cur and len(cur) + 1 + len(s) > limit:
            out.append(cur)
            cur = s
        else:
            cur = f"{cur} {s}".strip()
    if cur:
        out.append(cur)
    return out


class VibeVoice:
    def __init__(self, model):
        from vibevoice.modular.modeling_vibevoice_inference import VibeVoiceForConditionalGenerationInference
        from vibevoice.processor.vibevoice_processor import VibeVoiceProcessor

        self.processor = VibeVoiceProcessor.from_pretrained(model)
        self.model = VibeVoiceForConditionalGenerationInference.from_pretrained(
            model, torch_dtype=torch.bfloat16, device_map="cuda", attn_implementation="sdpa"
        )
        self.model.eval()
        self.model.set_ddpm_inference_steps(num_steps=10)
        self.rate = 24000

    def say(self, text, ref):
        inputs = self.processor(
            text=[f"Speaker 1: {text}"], voice_samples=[[ref]], padding=True, return_tensors="pt", return_attention_mask=True
        )
        inputs = {k: v.to("cuda") if torch.is_tensor(v) else v for k, v in inputs.items()}
        with torch.inference_mode():
            out = self.model.generate(
                **inputs, max_new_tokens=None, cfg_scale=1.3, tokenizer=self.processor.tokenizer,
                generation_config={"do_sample": False}, verbose=False,
            )
        return out.speech_outputs[0].float().cpu().numpy().reshape(-1)


class Chatterbox:
    def __init__(self, model):
        from chatterbox.tts import ChatterboxTTS

        self.model = ChatterboxTTS.from_pretrained(device="cuda")
        self.rate = self.model.sr

    def say(self, text, ref):
        gap = np.zeros(int(0.18 * self.rate), dtype="float32")
        out = []
        for p in pieces(text):
            with torch.inference_mode():
                wav = self.model.generate(p, audio_prompt_path=ref, exaggeration=0.5, cfg_weight=0.5)
            out += [wav.float().cpu().numpy().reshape(-1), gap]
        return np.concatenate(out[:-1]) if out else np.zeros(1, dtype="float32")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", choices=["vibevoice", "chatterbox"], required=True)
    ap.add_argument("--model", default="")
    a = ap.parse_args()
    engine = VibeVoice(a.model or "microsoft/VibeVoice-1.5B") if a.engine == "vibevoice" else Chatterbox(a.model)
    print(json.dumps({"ready": True}), flush=True)
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            job = json.loads(line)
            t = time.monotonic()
            wav = engine.say(plain(job["text"]), job["ref"])
            sf.write(job["out"], wav, engine.rate)
            print(json.dumps({"out": job["out"], "seconds": time.monotonic() - t, "audio": len(wav) / engine.rate}), flush=True)
        except Exception as e:
            print(json.dumps({"error": f"{type(e).__name__}: {e}"}), flush=True)


if __name__ == "__main__":
    main()
