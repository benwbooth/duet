# duet

A self-hosted book reader and audiobook player that keeps the two in sync. It's a Rust (Axum + SQLite) server and a Leptos web UI with desktop and phone layouts.

- **One library for both.** It scans your folders for ebooks (EPUB, PDF, TXT/MD, HTML, plus MOBI/AZW3/FB2/DOCX/RTF/ODT through Calibre) and audiobooks (a folder of tracks or a single M4B/MP3/…). It links editions of the same book by title and author.
- **Read and listen in sync.** Chapters are rendered with every sentence numbered. The *Sync text and audio* job transcribes the audiobook with Whisper and matches it against the text, so the sentence being spoken is highlighted and followed across chapters. Tapping a sentence jumps the audio there. Your place is shared between reading and listening.
- **Full-cast audiobooks from ebooks.** A chat model attributes the dialogue and writes a voice description for each character. Qwen3-TTS *VoiceDesign* builds a voice from that description, which is then cloned for consistency. Narration and dialogue are recorded chapter by chapter, and the result is synced automatically.
- **Ebooks from audiobooks.** The audio is transcribed, cleaned up into paragraphs and headings by the chat model, and written out as an EPUB.
- **Voice assistant.** You can ask about the book so far (without spoilers), define a selected word, or talk to a character or the narrator in their own voice. It works through the microphone or by typing.
- **Discover and download.** There are built-in free sources (Project Gutenberg, Standard Ebooks, Internet Archive public-domain texts, LibriVox audiobooks). Your own Torznab indexers (Prowlarr, Jackett) are also supported, with downloads through qBittorrent and automatic import when they finish.

## Running

```bash
nix develop                 # rust, wasm-bindgen, ffmpeg
crates/web/build.sh         # builds the UI into crates/web/dist
cargo run -p duet-server    # http://127.0.0.1:8484, data in ./data
```

`nix build` produces a wrapped `duet` binary that has the UI, the workers, Calibre and ffmpeg. The options are `--listen` / `DUET_LISTEN`, `--data-dir` / `DUET_DATA_DIR` and `--web-dir` / `DUET_WEB_DIR`.

Then open **Settings**. Add your book folders, then press *Scan folders* in the library.

## AI services

Everything talks to OpenAI-compatible HTTP endpoints, so any local server works:

| Use | Endpoint | Suggestions |
| --- | --- | --- |
| Chat (assistant, casting, formatting) | `/chat/completions` | llama.cpp, vLLM, Ollama |
| Speech to text (microphone) | `/audio/transcriptions` | speaches / faster-whisper-server |
| Text to speech (voices) | `/audio/speech` | `workers/tts_server.py` (Qwen3-TTS) |

The long jobs run the Python workers in `workers/`. Set them up with `python -m venv .venv && .venv/bin/pip install -r workers/requirements.txt`, then set *Settings → AI → Python* to `.venv/bin/python`.

```bash
.venv/bin/python workers/tts_server.py --port 8880   # VoiceDesign + Base + CustomVoice
```

The first time a voice name is requested with an `instructions` description (for example "gravelly old sailor, Cornish accent, slow and warm"), the TTS server designs that voice and saves a reference clip to `voices/`. After that it clones the clip, so the character always sounds the same.

## Layout

- `crates/common`: types shared by the server and UI, and the sentence splitter that keeps sentence numbers identical everywhere.
- `crates/server`: library scanning and linking (`library.rs`), chapter rendering (`book.rs`), jobs (`jobs.rs`), the assistant (`ai.rs`), qBittorrent (`qbit.rs`), Torznab (`torznab.rs`), free sources (`free.rs`) and downloads (`downloads.rs`).
- `crates/web`: the Leptos UI (`lib.rs` for the library and book pages, `reader.rs` for the reader, player and assistant, `discover.rs`, `settings.rs`).
- `workers`: `align.py`, `full_cast.py`, `transcribe.py` and `tts_server.py`. Each worker reports progress as JSON lines on stdout.

## Not done yet

- Sentence sync for PDFs (they open in the browser's viewer for now).
- Character-level timing inside long sentences; sync is per sentence.
- Sign-in. Run it on your LAN or behind a reverse proxy with auth.
