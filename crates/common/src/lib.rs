//! Types shared between the duet server and the Leptos web UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod text;

/// A work is one book, which may have any number of ebook and audiobook editions linked to it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkSummary {
    pub id: i64,
    pub title: String,
    pub author: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub has_ebook: bool,
    pub has_audiobook: bool,
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
    pub cover_url: Option<String>,
    pub assets: Vec<Asset>,
    pub position: Option<Position>,
    pub has_sync: bool,
    pub cast: Vec<CastMember>,
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
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReadMode {
    /// Server-rendered chapters with sentence spans (EPUB, text, HTML, converted formats).
    Chapters,
    /// The browser's own PDF viewer.
    Pdf,
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
    /// Align an ebook with its audiobook (STT + text matching) to build the sync map.
    Align,
    /// Generate a full-cast audiobook from an ebook.
    FullCast,
    /// Transcribe an audiobook into a formatted ebook.
    Transcribe,
    /// Convert an ebook to EPUB with Calibre.
    Convert,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Align => "align",
            JobKind::FullCast => "full_cast",
            JobKind::Transcribe => "transcribe",
            JobKind::Convert => "convert",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "align" => Some(JobKind::Align),
            "full_cast" => Some(JobKind::FullCast),
            "transcribe" => Some(JobKind::Transcribe),
            "convert" => Some(JobKind::Convert),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            JobKind::Align => "Sync text and audio",
            JobKind::FullCast => "Full-cast audiobook",
            JobKind::Transcribe => "Ebook from audio",
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
#[derive(Default)]
pub struct Settings {
    /// Folders scanned for books and audiobooks.
    pub library_dirs: Vec<String>,
    /// Where downloads and generated files are written (inside the library).
    pub import_dir: String,
    pub qbittorrent: QbitSettings,
    pub indexers: Vec<Indexer>,
    /// Built-in public-domain sources that can be switched off.
    pub free_sources: FreeSources,
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
}

impl Default for FreeSources {
    fn default() -> Self {
        FreeSources { gutenberg: true, standard_ebooks: true, librivox: true, internet_archive: true }
    }
}

/// AI services. Everything speaks the OpenAI-compatible HTTP API, so any local server works
/// (llama.cpp, vLLM, Ollama, speaches/faster-whisper-server, a Qwen3-TTS or VoxCPM wrapper...).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AiSettings {
    pub llm_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
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
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
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
}

impl Default for ReaderSettings {
    fn default() -> Self {
        ReaderSettings {
            font_size: 19,
            font: "Georgia, serif".into(),
            theme: "auto".into(),
            line_height: 1.6,
            playback_rate: 1.0,
            follow_audio: true,
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
