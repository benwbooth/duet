#!/usr/bin/env python3
"""Turns an audiobook into a formatted EPUB.

Transcribes each track with faster-whisper, breaks paragraphs at long pauses, optionally asks a
chat model to fix punctuation and paragraphing and mark headings, and writes an EPUB 3 with one
chapter per track (or per spoken "Chapter N" when the book is a single file).

    transcribe.py --audio audio.json --out book.epub --title T --author A
                  [--model large-v3] [--device auto] [--llm-url URL --llm-model M --llm-key K]
"""

import argparse
import html
import re
import uuid
import zipfile

from common import chat, device, load, load_audio, log, progress

PARAGRAPH_PAUSE = 1.2
CHAPTER = re.compile(r"^(chapter|book|part)\s+([0-9]+|[a-z-]+)\b[.:]?", re.I)


def transcribe(tracks, model_name, dev):
    from faster_whisper import WhisperModel

    try:
        model = WhisperModel(model_name, device=dev, compute_type="float16" if dev == "cuda" else "int8")
    except Exception as e:  # ROCm torch reports a GPU that CTranslate2 can't use
        log(f"faster-whisper can't use {dev} ({e}); using the CPU")
        model = WhisperModel(model_name, device="cpu", compute_type="int8")
    total = sum(t["duration"] for t in tracks) or 1.0
    done = 0.0
    out = []  # per track: list of paragraphs
    for t in tracks:
        segments, _ = model.transcribe(load_audio(t["path"]), vad_filter=True, language="en")
        paras, cur, last_end = [], [], 0.0
        for seg in segments:
            if cur and seg.start - last_end > PARAGRAPH_PAUSE:
                paras.append(" ".join(cur))
                cur = []
            cur.append(seg.text.strip())
            last_end = seg.end
            progress(0.7 * (done + seg.end) / total, f"Transcribing {t['title']}")
        if cur:
            paras.append(" ".join(cur))
        out.append((t["title"], paras))
        done += t["duration"]
    return out


def polish(args, paras):
    """Lets the chat model fix punctuation and paragraphing, a few thousand characters at a time."""
    if not args.llm_url:
        return paras
    out, batch = [], []
    for p in paras + [None]:
        if p is not None:
            batch.append(p)
        if batch and (p is None or sum(map(len, batch)) > 4000):
            text = "\n\n".join(batch)
            try:
                fixed = chat(args.llm_url, args.llm_model, args.llm_key, [
                    {"role": "system", "content": (
                        "You format audiobook transcripts as printed book text. Fix punctuation, capitalisation, "
                        "dialogue quotation marks and paragraph breaks. Put any chapter or section heading on its "
                        "own line starting with '# '. Do not add, remove, summarise or reword anything else. "
                        "Reply with the text only, paragraphs separated by blank lines."
                    )},
                    {"role": "user", "content": text},
                ])
                out.extend(x.strip() for x in fixed.split("\n\n") if x.strip())
            except Exception as e:
                log(f"formatting failed, keeping raw text: {e}")
                out.extend(batch)
            batch = []
    return out


def split_chapters(tracks):
    """One chapter per track, or split a single long track at headings."""
    if len(tracks) > 1:
        return tracks
    title, paras = tracks[0]
    chapters = [("Opening", [])]
    for p in paras:
        heading = p[2:].strip() if p.startswith("# ") else (p if CHAPTER.match(p) and len(p) < 80 else None)
        if heading:
            chapters.append((heading, []))
        else:
            chapters[-1][1].append(p)
    return [c for c in chapters if c[1]] or tracks


def write_epub(path, title, author, chapters):
    book_id = f"urn:uuid:{uuid.uuid4()}"
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
        z.writestr("META-INF/container.xml", (
            '<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">'
            '<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>'
        ))
        items, spine, nav = [], [], []
        for i, (ch_title, paras) in enumerate(chapters):
            name = f"ch{i + 1:03d}.xhtml"
            body = "".join(
                f"<h2>{html.escape(p[2:])}</h2>" if p.startswith("# ") else f"<p>{html.escape(p)}</p>" for p in paras
            )
            z.writestr(f"OEBPS/{name}", (
                '<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head>'
                f"<title>{html.escape(ch_title)}</title></head><body><h1>{html.escape(ch_title)}</h1>{body}</body></html>"
            ))
            items.append(f'<item id="c{i}" href="{name}" media-type="application/xhtml+xml"/>')
            spine.append(f'<itemref idref="c{i}"/>')
            nav.append(f'<li><a href="{name}">{html.escape(ch_title)}</a></li>')
        z.writestr("OEBPS/nav.xhtml", (
            '<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml" '
            'xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body>'
            f'<nav epub:type="toc"><ol>{"".join(nav)}</ol></nav></body></html>'
        ))
        z.writestr("OEBPS/content.opf", (
            '<?xml version="1.0" encoding="utf-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" '
            'unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">'
            f'<dc:identifier id="id">{book_id}</dc:identifier><dc:title>{html.escape(title)}</dc:title>'
            f'<dc:creator>{html.escape(author)}</dc:creator><dc:language>en</dc:language>'
            '<meta property="dcterms:modified">2000-01-01T00:00:00Z</meta></metadata><manifest>'
            '<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>'
            f'{"".join(items)}</manifest><spine>{"".join(spine)}</spine></package>'
        ))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--audio", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--title", required=True)
    ap.add_argument("--author", default="")
    ap.add_argument("--model", default="large-v3")
    ap.add_argument("--device", default="auto")
    ap.add_argument("--llm-url", default="")
    ap.add_argument("--llm-model", default="")
    ap.add_argument("--llm-key", default="")
    args = ap.parse_args()

    tracks = load(args.audio)["tracks"]
    progress(0, "Loading speech model")
    raw = transcribe(tracks, args.model, device(args.device))
    polished = []
    for i, (title, paras) in enumerate(raw):
        progress(0.7 + 0.25 * i / len(raw), f"Formatting {title}")
        polished.append((title, polish(args, paras)))
    chapters = split_chapters(polished)
    write_epub(args.out, args.title, args.author, chapters)
    progress(1, f"Wrote {len(chapters)} chapters")


if __name__ == "__main__":
    main()
