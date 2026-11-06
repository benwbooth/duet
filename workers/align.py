#!/usr/bin/env python3
"""Matching heard words to a book's words, for syncing as you listen (tts_server.py's
/v1/sync-window): each stretch of transcript is located in the book through a phrase index, then
matched word by word, giving every sentence heard a start and end time."""

import os
import bisect
import difflib

from common import words


def book_words(book):
    """Every word of the book with the (chapter, sentence) it belongs to."""
    out = []
    for ch in book["chapters"]:
        for si, sentence in enumerate(ch["sentences"]):
            for w in words(sentence):
                out.append((w, ch["index"], si))
    return out


def cache_path(path, model_name):
    """Transcripts are kept next to duet's data, keyed by file, size and modification time, so a
    re-sync (or syncing the same recording with another edition) doesn't listen again."""
    import hashlib

    st = os.stat(path)
    key = hashlib.sha1(f"{os.path.abspath(path)}|{st.st_size}|{st.st_mtime_ns}|{model_name}".encode()).hexdigest()
    base = os.environ.get("DUET_DATA_DIR") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data")
    d = os.path.join(base, "cache", "transcripts")
    os.makedirs(d, exist_ok=True)
    return os.path.join(d, key + ".json")


SHINGLE = 4  # words per phrase in the index
CHUNK = 120  # heard words matched at a time


def index_book(bw):
    """Every 4-word phrase in the book -> where it occurs (very common phrases are dropped)."""
    idx = {}
    for i in range(len(bw) - SHINGLE + 1):
        idx.setdefault(tuple(bw[i : i + SHINGLE]), []).append(i)
    return {k: v for k, v in idx.items() if len(v) <= 20}


def locate(idx, part, near):
    """Where in the book a stretch of heard words starts, by phrase votes; None if it isn't there
    (a recording's intro, credits, or a passage the edition doesn't have)."""
    votes = {}
    for j in range(len(part) - SHINGLE + 1):
        for p in idx.get(tuple(part[j : j + SHINGLE]), ()):
            bucket = (p - j) // 40
            votes[bucket] = votes.get(bucket, 0) + 1
    if not votes:
        return None
    best = max(votes.values())
    if best < 3:
        return None
    # Among strong candidates, prefer the one nearest where the last stretch ended.
    strong = [b for b, v in votes.items() if v >= best * 0.7]
    bucket = min(strong, key=lambda b: abs(b * 40 - near))
    return max(0, bucket * 40)


def align(book, heard, near=0, idx=None):
    """Maps book word index -> heard word index.

    The book's words are indexed by phrase; each stretch of the transcript is located anywhere in
    the book through that index, then matched word by word around that spot. So intros, skipped
    front matter, chapters recorded out of order or missing passages don't derail it.
    """
    bw = [w for w, _, _ in book]
    hw = [w for w, _, _, _ in heard]
    if idx is None:
        idx = index_book(bw)
    mapping = {}
    # Overlapping stretches, so words where the recording jumps (a chapter boundary) get a second
    # chance in a stretch that sits wholly on one side of the jump.
    for start in range(0, len(hw), CHUNK // 2):
        part = hw[start : start + CHUNK]
        anchor = locate(idx, part, near)
        if anchor is None:
            continue
        lo = max(0, anchor - 80)
        hi = min(len(bw), anchor + int(len(part) * 1.3) + 80)
        sm = difflib.SequenceMatcher(None, bw[lo:hi], part, autojunk=False)
        last = None
        for a, b, size in sm.get_matching_blocks():
            # One- and two-word matches of common words are noise.
            if size < 3:
                continue
            for k in range(size):
                mapping.setdefault(lo + a + k, start + b + k)
            last = lo + a + size
        if last is not None:
            near = last
    return mapping


def spans(book, heard, mapping):
    """One span per sentence; sentences with no matched words are interpolated.

    A stray word matched far from the rest of its sentence (a common phrase that also occurs
    elsewhere) would stretch the span, so each sentence keeps only the words heard near the
    middle of its matches.
    """
    words = {}
    for bi, hi in mapping.items():
        _, ch, si = book[bi]
        _, track, s, e = heard[hi]
        words.setdefault((ch, si), []).append((track, s, e))
    by_sentence = {}
    for key, ws in words.items():
        ws.sort()
        mid_track, mid_s, _ = ws[len(ws) // 2]
        kept = [w for w in ws if w[0] == mid_track and abs(w[1] - mid_s) < 30.0]
        by_sentence[key] = [mid_track, min(w[1] for w in kept), max(w[2] for w in kept)]
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
            # Only fill a small gap: across a jump in the recording there's nothing to interpolate.
            gap = known[j] - known[j - 1] - 1
            if prev[0] != nxt[0] or not (0 <= nxt[1] - prev[2] <= 12.0 * gap):
                continue
            track, s, e = prev[0], prev[2], nxt[1]
        out.append({"chapter": ch, "sentence": si, "track": track, "start": round(s, 3), "end": round(max(e, s), 3)})
    return out
