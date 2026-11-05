#!/usr/bin/env python3
"""Turns an ebook into a full-cast audiobook.

1. A chat model reads the book chapter by chapter, works out who speaks each line of dialogue,
   and writes a short voice description for every character (and the narrator).
2. Each character's voice is designed from that description by the TTS server (see
   tts_server.py: Qwen3-TTS VoiceDesign, then cloned for consistency).
3. Narration is read by the narrator; quoted speech by whoever says it. One MP3 per chapter.

Because it knows exactly when each sentence is spoken, it also writes the sync map.

    full_cast.py --book book.json --title T --author A --out-dir DIR --cast cast.json --sync sync.json
                 --llm-url URL --llm-model M [--llm-key K]
TTS: $DUET_TTS_URL (OpenAI-compatible /audio/speech).
"""

import argparse
import io
import json
import os
import re
import subprocess
import urllib.request

from common import chat_json, load, log, progress, save

QUOTE = re.compile(r"(“[^”]*”|\"[^\"]*\")")
PAUSE_SENTENCE = 0.35
PAUSE_PARAGRAPH = 0.8
CHUNK = 120  # sentences per attribution request


def attribute(args, chapter, characters):
    """Speaker for every sentence with dialogue in a chapter, plus new character descriptions."""
    speakers = {}
    sentences = chapter["sentences"]
    for start in range(0, len(sentences), CHUNK):
        part = sentences[start : start + CHUNK]
        if not any(QUOTE.search(s) for s in part):
            continue
        numbered = "\n".join(f"{start + i}: {s}" for i, s in enumerate(part))
        known = "\n".join(f"- {n}: {d}" for n, d in characters.items()) or "(none yet)"
        reply = chat_json(args.llm_url, args.llm_model, args.llm_key, [
            {"role": "system", "content": (
                "You prepare books for full-cast audiobook recording. For each numbered sentence containing quoted "
                "speech, name the character speaking. Use one consistent name per character. For any character not "
                "in the known list, write a voice description for a voice actor: gender, approximate age, accent, "
                "pitch, pace and temperament, in one sentence. Reply with JSON: "
                '{"speakers": {"<sentence number>": "<name>"}, "new_characters": {"<name>": "<voice description>"}}'
            )},
            {"role": "user", "content": f"Book: {args.title} by {args.author}\nKnown characters:\n{known}\n\nSentences:\n{numbered}"},
        ])
        for k, v in (reply.get("speakers") or {}).items():
            if str(k).isdigit() and v:
                speakers[int(k)] = v
        for n, d in (reply.get("new_characters") or {}).items():
            characters.setdefault(n, d)
    return speakers


def tts(text, voice, description):
    body = {"model": os.environ.get("DUET_TTS_MODEL", "qwen3-tts"), "input": text, "voice": voice, "instructions": description, "response_format": "wav"}
    req = urllib.request.Request(
        os.environ.get("DUET_TTS_URL", "http://localhost:8880/v1").rstrip("/") + "/audio/speech",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    import soundfile as sf

    with urllib.request.urlopen(req, timeout=600) as r:
        wav, sr = sf.read(io.BytesIO(r.read()), dtype="float32")
    if wav.ndim > 1:
        wav = wav.mean(axis=1)
    return wav, sr


def resample(wav, sr, target):
    if sr == target:
        return wav
    import numpy as np

    x = np.linspace(0, len(wav), int(len(wav) * target / sr), endpoint=False)
    return np.interp(x, np.arange(len(wav)), wav).astype("float32")


def pieces(sentence, speaker):
    """Splits a sentence into (text, who) parts: quotes by the speaker, the rest by the narrator."""
    if speaker is None:
        return [(sentence, None)]
    out = []
    for part in QUOTE.split(sentence):
        if not part.strip():
            continue
        quoted = QUOTE.fullmatch(part.strip()) is not None
        out.append((part.strip().strip("“”\"").strip() if quoted else part.strip(), speaker if quoted else None))
    return [p for p in out if re.search(r"\w", p[0])]


def main():
    ap = argparse.ArgumentParser()
    for a in ("--book", "--title", "--out-dir", "--cast", "--sync", "--llm-url", "--llm-model"):
        ap.add_argument(a, required=True)
    ap.add_argument("--author", default="")
    ap.add_argument("--llm-key", default="")
    ap.add_argument("--device", default="auto")
    args = ap.parse_args()
    import numpy as np
    import soundfile as sf

    book = load(args.book)
    state_dir = os.path.join(args.out_dir, ".duet")
    os.makedirs(state_dir, exist_ok=True)
    cast_path = os.path.join(state_dir, "characters.json")
    characters = load(cast_path) if os.path.exists(cast_path) else {}
    narrator = characters.pop("Narrator", None) or chat_json(args.llm_url, args.llm_model, args.llm_key, [
        {"role": "user", "content": (
            f"Describe the ideal audiobook narrator voice for \"{args.title}\" by {args.author} in one sentence "
            '(gender, age, accent, pitch, pace, tone). Reply as JSON {"description": "..."}'
        )},
    ]).get("description", "A warm, clear, expressive audiobook narrator.")
    prefix = re.sub(r"[^a-z0-9]+", "-", args.title.lower()).strip("-")[:40]

    chapters = [c for c in book["chapters"] if c["sentences"]]
    total = sum(len(c["sentences"]) for c in chapters) or 1
    done = 0
    sync = []
    for track, ch in enumerate(chapters):
        title = ch.get("title") or f"Chapter {track + 1}"
        safe = re.sub(r'[/\\:*?<>|"]', "_", title)
        mp3 = os.path.join(args.out_dir, f"{track + 1:03d} - {safe}.mp3")
        timing_path = os.path.join(state_dir, f"{track:03d}.json")
        if os.path.exists(mp3) and os.path.exists(timing_path):
            sync.extend(load(timing_path))
            done += len(ch["sentences"])
            continue
        progress(done / total, f"Casting {title}")
        speakers = attribute(args, ch, characters)
        save(cast_path, {**characters, "Narrator": narrator})
        audio, timings, sr, t = [], [], 24000, 0.0
        for si, sentence in enumerate(ch["sentences"]):
            start = t
            for text, who in pieces(sentence, speakers.get(si)):
                voice = f"{prefix}-{who or 'narrator'}"
                desc = characters.get(who, narrator) if who else narrator
                wav, wsr = tts(text, voice, desc)
                wav = resample(wav, wsr, sr)
                audio.append(wav)
                t += len(wav) / sr
            timings.append({"chapter": ch["index"], "sentence": si, "track": track, "start": round(start, 3), "end": round(t, 3)})
            gap = PAUSE_PARAGRAPH if sentence.endswith(("”", '"')) else PAUSE_SENTENCE
            audio.append(np.zeros(int(gap * sr), dtype="float32"))
            t += gap
            done += 1
            if si % 10 == 0:
                progress(done / total, f"Recording {title}")
        buf = io.BytesIO()
        sf.write(buf, np.concatenate(audio) if audio else np.zeros(sr, dtype="float32"), sr, format="WAV")
        subprocess.run([
            "ffmpeg", "-loglevel", "error", "-y", "-i", "pipe:0", "-b:a", "96k",
            "-metadata", f"album={args.title}", "-metadata", f"artist={args.author}",
            "-metadata", f"title={title}", "-metadata", f"track={track + 1}", mp3,
        ], input=buf.getvalue(), check=True)
        save(timing_path, timings)
        sync.extend(timings)

    cast = [{"id": 0, "name": "Narrator", "description": narrator, "voice": f"{prefix}-narrator", "is_narrator": True}]
    cast += [{"id": 0, "name": n, "description": d, "voice": f"{prefix}-{n}", "is_narrator": False} for n, d in sorted(characters.items())]
    save(args.cast, cast)
    save(args.sync, sync)
    log(f"{len(cast)} voices, {len(sync)} sentences")
    progress(1, f"Recorded {len(chapters)} chapters with {len(cast)} voices")


if __name__ == "__main__":
    main()
