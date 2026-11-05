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

For development, `scripts/dev.sh` watches the source with watchexec. It rebuilds the UI or the server on any change and swaps in the new one only when the build succeeds. Open pages reload themselves.

`nix build` produces a wrapped `duet` binary that has the UI, the workers, Calibre and ffmpeg. The options are `--listen` / `DUET_LISTEN`, `--data-dir` / `DUET_DATA_DIR` and `--web-dir` / `DUET_WEB_DIR`.

Then open **Settings**. Add your book folders, then press *Scan folders* in the library.

## AI services

Everything talks to OpenAI-compatible HTTP endpoints, so any local server works:

| Use | Endpoint | Suggestions |
| --- | --- | --- |
| Chat (assistant, casting, formatting) | `/chat/completions` | llama.cpp, vLLM, Ollama |
| Speech to text (microphone) | `/audio/transcriptions` | speaches / faster-whisper-server |
| Text to speech (voices) | `/audio/speech` | `workers/tts_server.py` (Fish Audio + Qwen3-TTS VoiceDesign) |

The long jobs run the Python workers in `workers/`. Set them up with `python -m venv .venv && .venv/bin/pip install -r workers/requirements.txt`, then set *Settings → AI → Python* to `.venv/bin/python`.

### Voices

Voices are split into two jobs. **Casting:** Qwen3-TTS *VoiceDesign* creates each character's voice from the description the director model wrote, and saves a reference clip in `voices/`. You can drop in your own clip to recast a character. **Acting:** every line is then performed by [Fish Audio](https://github.com/fishaudio/fish-speech), cloning that clip. The director model adds an acting direction to each line of dialogue (for example `[barely holding back tears]`), and Fish follows inline directions like that.

| Acting model | Quality | Hardware |
| --- | --- | --- |
| Fish Audio **Drama 3** | #1 open-weights model on the Artificial Analysis TTS arena (Oct 2026), word-level direction | NVIDIA, 48 GB+ (no ROCm) |
| Fish Audio **S2-Pro** | Next-best Fish model, same direction tags | 24 GB, NVIDIA or AMD ROCm |
| Qwen3-TTS Base (`--engine qwen`) | Good, no directions | ~6 GB |

Both Fish models are under the Fish Audio Community/Research license: free for personal and non-commercial use.

```bash
git clone https://github.com/fishaudio/fish-speech ~/src/fish-speech   # install per its docs
hf download fishaudio/s2-pro --local-dir ~/src/fish-speech/checkpoints/s2-pro
.venv/bin/python workers/tts_server.py --engine fish --fish-dir ~/src/fish-speech \
    --fish-python ~/src/fish-speech/.venv/bin/python --fish-checkpoint checkpoints/s2-pro
```

Set up this way, the TTS server starts fish-speech itself and stops it while VoiceDesign is casting, because both won't fit on one 24 GB card at once. The full-cast job casts every voice before it records anything. To use Drama 3 on another machine, run its `tools/api_server.py` there and pass `--fish-url http://thatbox:8080` with no `--fish-dir`.

## Layout

- `crates/common`: types shared by the server and UI, and the sentence splitter that keeps sentence numbers identical everywhere.
- `crates/server`: library scanning and linking (`library.rs`), chapter rendering (`book.rs`), jobs (`jobs.rs`), the assistant (`ai.rs`), qBittorrent (`qbit.rs`), Torznab (`torznab.rs`), free sources (`free.rs`) and downloads (`downloads.rs`).
- `crates/web`: the Leptos UI (`lib.rs` for the library and book pages, `reader.rs` for the reader, player and assistant, `discover.rs`, `settings.rs`).
- `workers`: `align.py`, `full_cast.py`, `transcribe.py` and `tts_server.py`. Each worker reports progress as JSON lines on stdout.

## Not done yet

- Sentence sync for PDFs (they open in the browser's viewer for now).
- Character-level timing inside long sentences; sync is per sentence.
- Sign-in. Run it on your LAN or behind a reverse proxy with auth.
