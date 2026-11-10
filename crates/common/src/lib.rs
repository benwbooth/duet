//! Types shared between the duet server and the Leptos web UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod podcast;
pub mod text;

/// A work is one book, which may have any number of ebook and audiobook editions linked to it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkSummary {
    pub id: i64,
    pub title: String,
    pub author: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    /// Publication year, when any edition states one.
    pub year: Option<i32>,
    pub has_ebook: bool,
    pub has_audiobook: bool,
    /// A recorded (not AI-generated) audiobook edition exists.
    #[serde(default)]
    pub has_narration: bool,
    /// An AI full-cast audiobook edition exists.
    #[serde(default)]
    pub has_cast: bool,
    pub has_sync: bool,
    pub cover_url: Option<String>,
    /// 0.0..=1.0, from the saved reading position.
    pub progress: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkDetail {
    pub id: i64,
    pub title: String,
    pub author: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub description: Option<String>,
    pub year: Option<i32>,
    /// Subjects and genres, looked up online.
    #[serde(default)]
    pub subjects: Vec<String>,
    /// Pages in print, looked up online.
    #[serde(default)]
    pub pages: Option<i64>,
    pub cover_url: Option<String>,
    pub assets: Vec<Asset>,
    pub position: Option<Position>,
    pub has_sync: bool,
    pub cast: Vec<CastMember>,
    /// The editions the book opens with (set on its page, or the last one picked in the reader).
    #[serde(default)]
    pub default_ebook: Option<i64>,
    #[serde(default)]
    pub default_audio: Option<i64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    Ebook,
    Audiobook,
}

impl AssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AssetKind::Ebook => "ebook",
            AssetKind::Audiobook => "audiobook",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ebook" => Some(AssetKind::Ebook),
            "audiobook" => Some(AssetKind::Audiobook),
            _ => None,
        }
    }
}

/// One edition of a work: an ebook file, or an audiobook (one file or a folder of tracks).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Asset {
    pub id: i64,
    pub work_id: i64,
    pub kind: AssetKind,
    /// Lowercase file extension: epub, pdf, txt, mobi, m4b, mp3, ...
    pub format: String,
    pub path: String,
    pub size: i64,
    pub duration_secs: Option<f64>,
    pub tracks: Vec<Track>,
    /// Where it came from: "scan", "generated", "download".
    pub origin: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub index: i64,
    pub title: String,
    pub duration_secs: f64,
    pub url: String,
}

/// A table of contents entry for a readable ebook.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TocEntry {
    pub title: String,
    pub chapter: usize,
    pub depth: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BookLayout {
    /// How the UI should render it.
    pub mode: ReadMode,
    pub chapters: usize,
    pub toc: Vec<TocEntry>,
    /// For page-image books (comics, PDF): how many pages.
    #[serde(default)]
    pub pages: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReadMode {
    /// Server-rendered chapters with sentence spans (EPUB, text, HTML, converted formats).
    Chapters,
    /// Pictures of pages (comics, PDF), served one image per page; one "chapter" holds them all.
    Images,
    /// Needs conversion first (e.g. MOBI without Calibre installed).
    Unsupported,
}

/// A rendered chapter. Every sentence is wrapped in `<span class="s" data-s="N">` so the reader
/// can highlight what the audiobook is saying; N matches `SyncSpan::sentence`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Chapter {
    pub index: usize,
    pub html: String,
    pub sentences: usize,
}

/// A place in a work, shared by the reader and the player.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub chapter: usize,
    pub sentence: usize,
    /// Audiobook position: track index and seconds into it.
    pub track: usize,
    pub seconds: f64,
    /// 0.0..=1.0 overall, for the library view.
    pub fraction: f64,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

/// Maps one sentence of the ebook to a time range in the audiobook.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SyncSpan {
    pub chapter: usize,
    pub sentence: usize,
    pub track: usize,
    pub start: f64,
    pub end: f64,
    /// When each word of the sentence starts (its words as `[\w']+` finds them), when known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SyncMap {
    pub ebook_asset: i64,
    pub audiobook_asset: i64,
    pub spans: Vec<SyncSpan>,
}

/// A character (or the narrator) with a designed voice, from the full-cast pipeline or the user.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CastMember {
    pub id: i64,
    pub name: String,
    pub description: String,
    /// Voice name the TTS endpoint understands, or a reference clip path for cloning.
    pub voice: Option<String>,
    pub is_narrator: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Convert an ebook to EPUB with Calibre.
    Convert,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Convert => "convert",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "convert" => Some(JobKind::Convert),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            JobKind::Convert => "Convert to EPUB",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: JobKind,
    pub work_id: i64,
    /// queued, running, done, failed
    pub status: String,
    pub progress: f64,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct NewJob {
    pub kind: JobKind,
    pub work_id: i64,
    /// For syncing: which editions to pair. Empty: the original ebook and audiobook.
    #[serde(default)]
    pub ebook_asset: Option<i64>,
    #[serde(default)]
    pub audiobook_asset: Option<i64>,
}

/// Everything configured on the settings page. Stored as one JSON document.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Folders scanned for books and audiobooks.
    pub library_dirs: Vec<String>,
    /// Where downloads and generated files are written (inside the library).
    pub import_dir: String,
    /// HTTP proxy all site searches and downloads go through
    /// (e.g. your VPN's forward proxy). Empty: connect directly.
    #[serde(default = "default_proxy_url")]
    pub proxy_url: String,
    /// Browser inbox: duet imports any ebook/audiobook file dropped here
    /// (e.g. the remote Firefox's downloads). Empty: disabled.
    #[serde(default)]
    pub inbox_dir: String,
    /// Remote browser (Firefox on the VPN) web UI address, shown inside
    /// duet's Browser page. Empty: built from the page's own host (:3001).
    #[serde(default)]
    pub browser_url: String,
    pub qbittorrent: QbitSettings,
    pub indexers: Vec<Indexer>,
    /// Built-in public-domain sources that can be switched off.
    pub free_sources: FreeSources,
    /// Anna's Archive (search requires a member secret key).
    pub annas_archive: AnnasArchiveSettings,
    /// Library Genesis — includes the RandomBook / libgen.pw / libgen.me mirrors.
    pub libgen: LibGenSettings,
    /// Audiobook Bay (audiobook torrents as magnet links).
    pub audiobookbay: AudiobookBaySettings,
    pub ai: AiSettings,
    pub reader: ReaderSettings,
    /// The first-run guide was finished or skipped.
    pub setup_done: bool,
}


#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct QbitSettings {
    pub enabled: bool,
    /// e.g. http://localhost:8080
    pub url: String,
    pub username: String,
    pub password: String,
    pub category_ebooks: String,
    pub category_audiobooks: String,
    /// Import finished downloads into the library automatically.
    pub auto_import: bool,
    /// Host-side path that qBittorrent's `url` sees as its save path (for
    /// Dockerized qBittorrent, e.g. /downloads -> /mnt/stuff/Downloads).
    /// Empty: duet assumes qBittorrent runs on this host.
    pub host_save_path: String,
}

impl Default for QbitSettings {
    fn default() -> Self {
        QbitSettings {
            enabled: false,
            url: "http://localhost:8080".into(),
            username: "admin".into(),
            password: String::new(),
            category_ebooks: "duet-ebooks".into(),
            category_audiobooks: "duet-audiobooks".into(),
            auto_import: true,
            host_save_path: String::new(),
        }
    }
}

fn default_proxy_url() -> String {
    "http://localhost:8888".into()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            library_dirs: Vec::new(),
            import_dir: String::new(),
            proxy_url: default_proxy_url(),
            inbox_dir: String::new(),
            browser_url: String::new(),
            qbittorrent: QbitSettings::default(),
            indexers: Vec::new(),
            free_sources: FreeSources::default(),
            annas_archive: AnnasArchiveSettings::default(),
            libgen: LibGenSettings::default(),
            audiobookbay: AudiobookBaySettings::default(),
            ai: AiSettings::default(),
            reader: ReaderSettings::default(),
            setup_done: false,
        }
    }
}

/// A Torznab indexer (Prowlarr, Jackett or a native one), added by the user.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Indexer {
    pub name: String,
    pub enabled: bool,
    /// The Torznab API URL, e.g. http://localhost:9696/1/api
    pub url: String,
    pub api_key: String,
    /// Torznab categories for ebooks (7000-range) and audiobooks (3030).
    pub ebook_categories: String,
    pub audiobook_categories: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FreeSources {
    pub gutenberg: bool,
    pub standard_ebooks: bool,
    pub librivox: bool,
    pub internet_archive: bool,
    pub annas_archive: bool,
    pub libgen: bool,
    pub audiobookbay: bool,
}

impl Default for FreeSources {
    fn default() -> Self {
        FreeSources { gutenberg: true, standard_ebooks: true, librivox: true, internet_archive: true, annas_archive: true, libgen: true, audiobookbay: true }
    }
}

/// Anna's Archive: search is scraped from the mirror's /search pages, and
/// downloads go through the member fast-download API. Anna's Archive now
/// challenges anonymous requests, so `secret_key` (the account secret shown
/// on your account page, the same key the fast-download API uses) is
/// required; the server trades it for a session cookie and never logs it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AnnasArchiveSettings {
    /// Which mirror to use (domains rotate; check the current list on the
    /// Anna's Archive Wikipedia page before changing it).
    pub mirror: String,
    pub secret_key: String,
}

impl Default for AnnasArchiveSettings {
    fn default() -> Self {
        AnnasArchiveSettings { mirror: "https://annas-archive.gl".into(), secret_key: String::new() }
    }
}

/// Library Genesis (LibGen). The RandomBook, libgen.pw and libgen.me mirrors
/// serve the same database, so one search covers them all; downloads go
/// through the mirror's own `/ads.php` → `/get.php` hop.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LibGenSettings {
    /// Which mirror to use (Open SLUM tracks these; change it here if the
    /// default stops answering).
    pub base_url: String,
}

impl Default for LibGenSettings {
    fn default() -> Self {
        LibGenSettings { base_url: "https://libgen.li".into() }
    }
}

/// Audiobook Bay: a community torrent index for audiobooks. There is no
/// official API; duet searches the listing pages and fetches each torrent's
/// detail page for its info hash, then builds a magnet link for qBittorrent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudiobookBaySettings {
    /// Base URL of the site (current mirrors: audiobookbay.lu and friends;
    /// change it here if the default stops working).
    pub base_url: String,
}

impl Default for AudiobookBaySettings {
    fn default() -> Self {
        AudiobookBaySettings { base_url: "https://audiobookbay.lu".into() }
    }
}

/// AI services. Everything speaks the OpenAI-compatible HTTP API, so any local server works
/// (llama.cpp, vLLM, Ollama, speaches/faster-whisper-server, a Qwen3-TTS or VoxCPM wrapper...).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AiSettings {
    /// Where the chat model runs: "local" (OpenAI-compatible server below), "anthropic",
    /// "openai", "claude_cli" (Claude Code) or "codex_cli" (Codex).
    pub llm_provider: String,
    pub llm_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub anthropic_api_key: String,
    pub anthropic_model: String,
    pub openai_api_key: String,
    pub openai_model: String,
    /// Model for the Claude Code / Codex / Gemini / OpenCode CLIs; empty uses the tool's default.
    pub cli_model: String,
    /// "service": any other OpenAI-compatible API (OpenRouter, Google Gemini, Groq, Mistral...).
    pub service_url: String,
    pub service_key: String,
    pub service_model: String,
    /// Speech to text: POST {stt_url}/audio/transcriptions
    pub stt_url: String,
    pub stt_model: String,
    /// Text to speech: POST {tts_url}/audio/speech
    pub tts_url: String,
    pub tts_model: String,
    pub narrator_voice: String,
    /// How the plain reading companion sounds, described in words (cast by VoiceDesign).
    pub assistant_voice: String,
    /// Python used to run the workers in ./workers (a venv with their requirements).
    pub python: String,
    /// Directory holding align.py, full_cast.py and transcribe.py.
    pub workers_dir: String,
    /// "cuda", "cpu" or "auto", passed to the workers.
    pub device: String,
    /// Start the local voice server with duet once it's installed.
    pub auto_start_voices: bool,
    /// Unload voice and speech models after this many idle minutes (0: keep them loaded).
    pub voice_idle_minutes: f64,
    /// Reading comics (speech balloons, captions, who says what, panels): "local" (a vision
    /// model on an OpenAI-compatible or Ollama server, below) or "chat" (the chat model above,
    /// when it can see pictures: Claude, OpenAI, Claude Code, Codex).
    pub vision_provider: String,
    pub vision_url: String,
    pub vision_model: String,
    /// The language comics in other languages are translated into, for reading and listening.
    pub translate_to: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            llm_provider: "local".into(),
            anthropic_api_key: String::new(),
            anthropic_model: "claude-opus-5-5".into(),
            openai_api_key: String::new(),
            openai_model: String::new(),
            cli_model: String::new(),
            service_url: "https://openrouter.ai/api/v1".into(),
            service_key: String::new(),
            service_model: String::new(),
            llm_url: "http://localhost:11434/v1".into(),
            llm_api_key: String::new(),
            llm_model: "qwen3:14b".into(),
            // The local voice server also does speech recognition (faster-whisper).
            stt_url: "http://127.0.0.1:8880/v1".into(),
            stt_model: "large-v3-turbo".into(),
            tts_url: "http://127.0.0.1:8880/v1".into(),
            tts_model: "qwen3-tts".into(),
            narrator_voice: "assistant".into(),
            assistant_voice: "A warm, clear, friendly voice, relaxed and conversational.".into(),
            python: "python3".into(),
            workers_dir: "workers".into(),
            device: "auto".into(),
            auto_start_voices: true,
            voice_idle_minutes: 5.0,
            vision_provider: "local".into(),
            vision_url: "http://localhost:11434/v1".into(),
            vision_model: "qwen3-vl:8b".into(),
            translate_to: "English".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReaderSettings {
    pub font_size: u32,
    pub font: String,
    /// "auto", "light", "sepia", "dark"
    pub theme: String,
    pub line_height: f64,
    pub playback_rate: f64,
    /// Turn pages / scroll to follow the audio.
    pub follow_audio: bool,
    /// "pages" (turn pages like a book) or "scroll"
    pub layout: String,
    /// Pages at a time: "one", "two" (facing pages) or "auto" (two on wide windows)
    pub spread: String,
    /// Animate page turns (the page curls over); off, pages change at once.
    pub page_curl: bool,
    /// AI-drawn pictures through the book, one every few pages.
    pub pictures: bool,
}

/// The reader's default typeface: book faces where the system has them.
pub const BOOK_FONT: &str = "\"Iowan Old Style\", \"Palatino Linotype\", Palatino, \"Book Antiqua\", Georgia, serif";

impl Default for ReaderSettings {
    fn default() -> Self {
        ReaderSettings {
            font_size: 19,
            font: BOOK_FONT.into(),
            theme: "auto".into(),
            line_height: 1.6,
            playback_rate: 1.0,
            follow_audio: true,
            layout: "pages".into(),
            spread: "auto".into(),
            page_curl: true,
            pictures: false,
        }
    }
}

/// A search hit from an indexer or a built-in free source.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchResult {
    pub source: String,
    pub title: String,
    pub author: Option<String>,
    pub kind: AssetKind,
    pub size: Option<i64>,
    pub seeders: Option<i64>,
    /// What to hand to POST /api/downloads.
    pub download: DownloadTarget,
    pub info_url: Option<String>,
    pub format: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DownloadTarget {
    /// A magnet link or .torrent URL for qBittorrent.
    Torrent { url: String },
    /// A direct HTTP download of one file, or a zip of audio files.
    Direct { url: String, filename: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct NewDownload {
    pub kind: AssetKind,
    pub title: String,
    pub target: DownloadTarget,
    /// The book this belongs to, so an ebook and audiobook fetched together land in one book
    /// even when their files are titled differently.
    #[serde(default)]
    pub author: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Download {
    pub id: String,
    pub name: String,
    pub kind: AssetKind,
    pub progress: f64,
    /// downloading, seeding, done, imported, failed, ...
    pub state: String,
    pub size: Option<i64>,
    pub source: String,
    /// Unix seconds when the download started (direct: row creation;
    /// torrents: qBittorrent's added_on). Newest first in the pane.
    #[serde(default)]
    pub started_at: Option<i64>,
}

/// A question to the assistant, typed or transcribed from the microphone.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AskRequest {
    pub work_id: Option<i64>,
    pub question: String,
    /// Talk to this cast member in character instead of the plain assistant.
    pub persona: Option<i64>,
    pub position: Option<Position>,
    /// Speak the answer with the persona's (or narrator's) voice.
    pub speak: bool,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatTurn {
    /// "user" or "assistant"
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AskReply {
    pub question: String,
    pub answer: String,
    /// data: URL with the spoken answer, when requested and TTS is configured.
    pub audio: Option<String>,
    pub speaker: String,
    /// The answer as it should be voiced (characters' acting directions included); fetch the
    /// audio separately with /api/assistant/say so the text shows straight away.
    #[serde(default)]
    pub spoken: String,
}

/// Voice an assistant answer: in the speaker's voice (a character, the narrator, or the companion).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SayRequest {
    pub work_id: Option<i64>,
    pub persona: Option<i64>,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScanReport {
    pub works: usize,
    pub ebooks: usize,
    pub audiobooks: usize,
    pub errors: Vec<String>,
}

/// State of the local voice stack (Settings → Voices).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct VoiceSetup {
    /// e.g. "AMD GPU, 24 GB"
    pub gpu: Option<String>,
    pub gpu_vram_gb: Option<u64>,
    pub fish_installed: bool,
    pub fish_weights: bool,
    pub workers_installed: bool,
    pub casting_weights: bool,
    pub installing: bool,
    pub step: String,
    pub progress: f64,
    pub error: Option<String>,
    pub log: Vec<String>,
    pub server_running: bool,
}

impl VoiceSetup {
    pub fn ready(&self) -> bool {
        self.fish_installed && self.fish_weights && self.workers_installed && self.casting_weights
    }
}

/// What the chat-model setup found on this computer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChatSetup {
    pub nixos: bool,
    /// The Claude Code and Codex command-line tools are installed.
    pub claude_cli: bool,
    pub codex_cli: bool,
    pub gemini_cli: bool,
    pub opencode_cli: bool,
    pub gpu: Option<String>,
    pub gpu_vram_gb: Option<u64>,
    pub servers: Vec<DetectedServer>,
    pub pull: Option<PullStatus>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DetectedServer {
    pub name: String,
    pub url: String,
    /// "ollama" (can download models) or "openai"
    pub kind: String,
    pub running: bool,
    pub models: Vec<LocalModel>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LocalModel {
    pub name: String,
    pub size_gb: Option<f64>,
    pub parameters: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PullStatus {
    pub model: String,
    pub status: String,
    pub progress: f64,
    pub done: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChatTest {
    pub answer: String,
    pub seconds: f64,
    pub tokens_per_second: Option<f64>,
}

/// A folder on the server, for the folder picker.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FolderListing {
    pub path: String,
    pub parent: Option<String>,
    pub folders: Vec<FolderEntry>,
    /// Likely book folders that exist on this computer (only for the starting listing).
    pub suggestions: Vec<FolderEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    /// Ebooks and audio files directly inside (capped), to show what's there.
    pub books: usize,
}

/// A dictionary entry (from Wiktionary through the Free Dictionary API).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DictEntry {
    pub word: String,
    pub phonetic: Option<String>,
    pub meanings: Vec<DictMeaning>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DictMeaning {
    pub part_of_speech: String,
    pub definitions: Vec<DictSense>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DictSense {
    pub definition: String,
    pub example: Option<String>,
}
