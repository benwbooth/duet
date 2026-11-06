#!/usr/bin/env python3
"""Aligns an ebook with its audiobook.

Transcribes the audio with word timestamps (faster-whisper), then matches the transcript words
to the book's words in a moving window, so every sentence the duet server rendered gets a start
and end time. Writes a list of {chapter, sentence, track, start, end}.

    align.py --book book.json --audio audio.json --out sync.json [--model large-v3] [--device auto]
"""

import argparse
import bisect
import difflib

from common import device, load, load_audio, log, progress, save, whisper, words


def book_words(book):
    """Every word of the book with the (chapter, sentence) it belongs to."""
    out = []
    for ch in book["chapters"]:
        for si, sentence in enumerate(ch["sentences"]):
            for w in words(sentence):
                out.append((w, ch["index"], si))
    return out


def transcribe(tracks, model_name, dev):
    model, used = whisper(model_name, dev)
    progress(0.01, f"Listening on the {'graphics card' if used == 'cuda' else 'processor'}")
    total = sum(t["duration"] for t in tracks) or 1.0
    done = 0.0
    out = []  # (word, track, start, end)
    for t in tracks:
        segments, _ = model.transcribe(load_audio(t["path"]), word_timestamps=True, vad_filter=True, language="en")
        for seg in segments:
            for w in seg.words or []:
                for token in words(w.word):
                    out.append((token, t["index"], w.start, w.end))
            progress(0.8 * (done + seg.end) / total, f"Transcribing {t['title']}")
        done += t["duration"]
    return out


def align(book, heard):
    """Maps book word index -> heard word index, walking a window forward through the book."""
    bw = [w for w, _, _ in book]
    hw = [w for w, _, _, _ in heard]
    mapping = {}
    pos = 0  # current place in the book
    chunk = 400  # heard words per step
    for start in range(0, len(hw), chunk):
        part = hw[start : start + chunk]
        # Look a little behind (skipped intros) and well ahead (cut front matter, ads).
        lo = max(0, pos - 200)
        hi = min(len(bw), pos + chunk * 3 + 2000)
        sm = difflib.SequenceMatcher(None, bw[lo:hi], part, autojunk=False)
        last = None
        for a, b, size in sm.get_matching_blocks():
            # Single-word matches of common words are noise.
            if size < 2:
                continue
            for k in range(size):
                mapping[lo + a + k] = start + b + k
            last = lo + a + size
        if last is not None:
            pos = last
        progress(0.8 + 0.15 * start / max(1, len(hw)), "Matching text to audio")
    return mapping


def spans(book, heard, mapping):
    """One span per sentence; sentences with no matched words are interpolated."""
    by_sentence = {}
    for bi, hi in mapping.items():
        _, ch, si = book[bi]
        _, track, s, e = heard[hi]
        cur = by_sentence.get((ch, si))
        if cur is None:
            by_sentence[(ch, si)] = [track, s, e]
        else:
            if (track, s) < (cur[0], cur[1]):
                cur[0], cur[1] = track, s
            if track >= cur[0]:
                cur[2] = max(cur[2], e)
    order = []
    seen = set()
    for _, ch, si in book:
        if (ch, si) not in seen:
            seen.add((ch, si))
            order.append((ch, si))
    out = []
    known = [i for i, k in enumerate(order) if k in by_sentence]
    for i, (ch, si) in enumerate(order):
        if (ch, si) in by_sentence:
            track, s, e = by_sentence[(ch, si)]
        else:
            # Between the neighbouring known sentences, on the earlier one's track.
            j = bisect.bisect_left(known, i)
            if j == 0 or j == len(known):
                continue
            prev = by_sentence[order[known[j - 1]]]
            nxt = by_sentence[order[known[j]]]
            if prev[0] != nxt[0]:
                continue
            track, s, e = prev[0], prev[2], nxt[1]
        out.append({"chapter": ch, "sentence": si, "track": track, "start": round(s, 3), "end": round(max(e, s), 3)})
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--book", required=True)
    ap.add_argument("--audio", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--model", default="large-v3")
    ap.add_argument("--device", default="auto")
    args = ap.parse_args()

    book = book_words(load(args.book))
    tracks = load(args.audio)["tracks"]
    progress(0, "Loading speech model")
    heard = transcribe(tracks, args.model, device(args.device))
    log(f"{len(book)} book words, {len(heard)} heard words")
    mapping = align(book, heard)
    result = spans(book, heard, mapping)
    matched = len(mapping) / max(1, len(book))
    save(args.out, result)
    progress(1, f"Synced {len(result)} sentences ({matched:.0%} of words matched)")


if __name__ == "__main__":
    main()
