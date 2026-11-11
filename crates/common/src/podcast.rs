//! The daily briefing: a podcast made each day from your feeds (Reddit, X, Instagram, YouTube,
//! news sites...), written by the chat model and performed by the local voices.

use serde::{Deserialize, Serialize};

/// What the briefing is made from and how it sounds. Kept apart from the main settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PodcastSettings {
    pub sources: Vec<PodcastSource>,
    /// What you care about, in your own words: steers which stories are picked and how deep
    /// they go.
    pub interests: String,
    /// Roughly how long an episode runs.
    pub minutes: u32,
    /// One host talking to you (the default), or two talking with each other.
    pub hosts: Vec<PodcastHost>,
    /// Make the day's episode at this local time ("06:30"), ready before you head out. Empty:
    /// only when you ask.
    pub daily_at: String,
    /// The remote browser's WebDriver (geckodriver attached to the Firefox you can see in
    /// duet), for X and Instagram: you log in there once, and duet reads your feeds off the page.
    pub webdriver_url: String,
    /// Its viewer, for logging in. Empty: this page's host, port 3001.
    pub viewer_url: String,
    /// An RSSHub instance (https://docs.rsshub.app), for X and Instagram accounts, which have
    /// no free feeds of their own.
    pub rsshub_url: String,
    /// Listen to linked videos and reels (yt-dlp + speech recognition) to cover what's said in them.
    pub watch_videos: bool,
    /// Which defaults these settings have been brought up to (see `upgrade`).
    pub version: u32,
}

/// What a briefing is about until the listener says otherwise.
pub const DEFAULT_INTERESTS: &str = "Deep dives, not headlines: tech, AI, retro video games and new games, movies, TV, books, \
     sci-fi, fantasy and horror, what's in my X feed, and anything cool going on in the world. Some world news, and a \
     little politics, but not too much.";

/// The default host: one man, a deep baritone.
pub fn default_host() -> PodcastHost {
    PodcastHost {
        name: "Cole".into(),
        voice: "A man in his forties with a deep, rich, resonant baritone voice and a General American accent: smooth, \
                unhurried and warm, quietly confident and wry, like a late-night public radio host or a great \
                audiobook narrator."
            .into(),
    }
}

/// The settings version this build brings saved settings up to.
pub const SETTINGS_VERSION: u32 = 2;

impl PodcastSettings {
    /// Brings settings saved by an older duet up to today's defaults, keeping what the
    /// listener chose: version 2 makes deep dives with a baritone host and adds the games,
    /// screen, books and AI sources.
    pub fn upgrade(&mut self) -> bool {
        if self.version >= SETTINGS_VERSION {
            return false;
        }
        if self.version < 2 {
            if self.interests.trim().is_empty() {
                self.interests = DEFAULT_INTERESTS.into();
            }
            // The old default host (Maya) gives way to the baritone; a host the listener
            // named or described themselves stays.
            if self.hosts.len() == 1 && self.hosts[0].name == "Maya" && self.hosts[0].voice.starts_with("A woman in her thirties with a warm, bright American voice") {
                self.hosts = vec![default_host()];
            }
            if self.minutes == 20 {
                self.minutes = 30;
            }
            for c in CATALOG.iter().filter(|c| ADDED_IN_2.contains(&c.name)) {
                if !self.sources.iter().any(|s| s.kind == c.kind && s.value == c.value) {
                    self.sources.push(PodcastSource { kind: c.kind.into(), value: c.value.into(), enabled: true });
                }
            }
        }
        self.version = SETTINGS_VERSION;
        true
    }
}

impl Default for PodcastSettings {
    fn default() -> Self {
        PodcastSettings {
            sources: CATALOG
                .iter()
                .filter(|c| DEFAULT_ON.contains(&c.name))
                .map(|c| PodcastSource { kind: c.kind.into(), value: c.value.into(), enabled: true })
                .collect(),
            interests: DEFAULT_INTERESTS.into(),
            minutes: 30,
            hosts: vec![default_host()],
            daily_at: "06:30".into(),
            webdriver_url: "http://127.0.0.1:4444".into(),
            viewer_url: String::new(),
            rsshub_url: String::new(),
            watch_videos: true,
            version: SETTINGS_VERSION,
        }
    }
}

/// A well-known source, one tap to add.
pub struct CatalogEntry {
    pub category: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
    pub value: &'static str,
}

const fn c(category: &'static str, name: &'static str, kind: &'static str, value: &'static str) -> CatalogEntry {
    CatalogEntry { category, name, kind, value }
}

/// Major news, tech, science and the aggregators, ready to switch on.
pub const CATALOG: &[CatalogEntry] = &[
    c("Social", "X (your timeline)", "x", "home"),
    c("Social", "Instagram (your feed)", "instagram", "home"),
    c("Aggregators", "Hacker News", "hackernews", "best"),
    c("Aggregators", "Lobsters", "lobsters", "hottest"),
    c("Aggregators", "Techmeme", "feed", "https://www.techmeme.com/feed.xml"),
    c("Aggregators", "Slashdot", "feed", "https://rss.slashdot.org/Slashdot/slashdotMain"),
    c("Aggregators", "Reddit popular", "reddit", "popular"),
    c("Aggregators", "Reddit r/news", "reddit", "news"),
    c("Aggregators", "Reddit r/worldnews", "reddit", "worldnews"),
    c("Aggregators", "Reddit r/technology", "reddit", "technology"),
    c("Aggregators", "Reddit r/science", "reddit", "science"),
    c("Aggregators", "MetaFilter", "feed", "https://rss.metafilter.com/metafilter.rss"),
    c("Aggregators", "Kottke", "feed", "https://feeds.kottke.org/main"),
    c("Aggregators", "Product Hunt", "feed", "https://www.producthunt.com/feed"),
    c("News", "BBC News (world)", "feed", "https://feeds.bbci.co.uk/news/world/rss.xml"),
    c("News", "BBC News (top)", "feed", "https://feeds.bbci.co.uk/news/rss.xml"),
    c("News", "NPR", "feed", "https://feeds.npr.org/1001/rss.xml"),
    c("News", "The Guardian (international)", "feed", "https://www.theguardian.com/international/rss"),
    c("News", "The New York Times", "feed", "https://rss.nytimes.com/services/xml/rss/nyt/HomePage.xml"),
    c("News", "The Washington Post (world)", "feed", "https://feeds.washingtonpost.com/rss/world"),
    c("News", "Al Jazeera", "feed", "https://www.aljazeera.com/xml/rss/all.xml"),
    c("News", "PBS NewsHour", "feed", "https://www.pbs.org/newshour/feeds/rss/headlines"),
    c("News", "Axios", "feed", "https://api.axios.com/feed/"),
    c("News", "Politico", "feed", "https://rss.politico.com/politics-news.xml"),
    c("News", "ProPublica", "feed", "https://www.propublica.org/feeds/propublica/main"),
    c("News", "The Atlantic", "feed", "https://www.theatlantic.com/feed/all/"),
    c("News", "Vox", "feed", "https://www.vox.com/rss/index.xml"),
    c("News", "The Economist (this week)", "feed", "https://www.economist.com/the-world-this-week/rss.xml"),
    c("News", "CNBC (top news)", "feed", "https://www.cnbc.com/id/100003114/device/rss/rss.html"),
    c("Tech", "The Verge", "feed", "https://www.theverge.com/rss/index.xml"),
    c("Tech", "Ars Technica", "feed", "https://feeds.arstechnica.com/arstechnica/index"),
    c("Tech", "Engadget", "feed", "https://www.engadget.com/rss.xml"),
    c("Tech", "TechCrunch", "feed", "https://techcrunch.com/feed/"),
    c("Tech", "Wired", "feed", "https://www.wired.com/feed/rss"),
    c("Tech", "404 Media", "feed", "https://www.404media.co/rss/"),
    c("Tech", "MIT Technology Review", "feed", "https://www.technologyreview.com/feed/"),
    c("Tech", "The Register", "feed", "https://www.theregister.com/headlines.atom"),
    c("Tech", "Tom's Hardware", "feed", "https://www.tomshardware.com/feeds/all"),
    c("Tech", "Phoronix", "feed", "https://www.phoronix.com/rss.php"),
    c("Tech", "LWN.net", "feed", "https://lwn.net/headlines/rss"),
    c("Tech", "9to5Mac", "feed", "https://9to5mac.com/feed/"),
    c("AI", "Reddit r/LocalLLaMA", "reddit", "LocalLLaMA"),
    c("AI", "Reddit r/singularity", "reddit", "singularity"),
    c("AI", "Simon Willison", "feed", "https://simonwillison.net/atom/everything/"),
    c("AI", "The Decoder", "feed", "https://the-decoder.com/feed/"),
    c("AI", "Hugging Face blog", "feed", "https://huggingface.co/blog/feed.xml"),
    c("Games", "Polygon (games)", "feed", "https://www.polygon.com/rss/index.xml"),
    c("Games", "Time Extension (retro)", "feed", "https://www.timeextension.com/feeds/latest"),
    c("Games", "Retro Dodo", "feed", "https://retrododo.com/feed/"),
    c("Games", "Reddit r/retrogaming", "reddit", "retrogaming"),
    c("Games", "Reddit r/gaming", "reddit", "gaming"),
    c("Games", "Rock Paper Shotgun", "feed", "https://www.rockpapershotgun.com/feed"),
    c("Games", "Eurogamer", "feed", "https://www.eurogamer.net/feed"),
    c("Games", "Kotaku", "feed", "https://kotaku.com/rss"),
    c("Games", "Game Developer", "feed", "https://www.gamedeveloper.com/rss.xml"),
    c("Screen & books", "Deadline", "feed", "https://deadline.com/feed/"),
    c("Screen & books", "/Film", "feed", "https://www.slashfilm.com/feed/"),
    c("Screen & books", "Bloody Disgusting (horror)", "feed", "https://bloody-disgusting.com/feed/"),
    c("Screen & books", "Reactor (sci-fi & fantasy)", "feed", "https://reactormag.com/feed/"),
    c("Screen & books", "Locus (sci-fi & fantasy books)", "feed", "https://locusmag.com/feed/"),
    c("Screen & books", "Reddit r/movies", "reddit", "movies"),
    c("Screen & books", "Reddit r/television", "reddit", "television"),
    c("Screen & books", "Reddit r/printSF", "reddit", "printSF"),
    c("Screen & books", "Reddit r/Fantasy", "reddit", "Fantasy"),
    c("Screen & books", "Reddit r/horror", "reddit", "horror"),
    c("Science", "Quanta Magazine", "feed", "https://www.quantamagazine.org/feed/"),
    c("Science", "Science Daily", "feed", "https://www.sciencedaily.com/rss/top.xml"),
    c("Science", "New Scientist", "feed", "https://www.newscientist.com/feed/"),
    c("Science", "Nature", "feed", "https://www.nature.com/nature.rss"),
    c("Science", "NASA", "feed", "https://www.nasa.gov/feed/"),
];

/// What a new briefing starts with: a broad mix, to be tuned.
const DEFAULT_ON: &[&str] = &[
    "X (your timeline)", "Hacker News", "Lobsters", "Techmeme", "Slashdot", "Reddit r/worldnews", "BBC News (world)", "NPR",
    "The Guardian (international)", "Al Jazeera", "The Verge", "Ars Technica", "Engadget", "Quanta Magazine",
    "Simon Willison", "Reddit r/LocalLLaMA", "Time Extension (retro)", "Reddit r/retrogaming", "Rock Paper Shotgun",
    "Polygon (games)", "Deadline", "/Film", "Bloody Disgusting (horror)", "Reactor (sci-fi & fantasy)", "Reddit r/printSF",
];

/// Switched on for settings saved before version 2 (games, screen, books, AI).
const ADDED_IN_2: &[&str] = &[
    "X (your timeline)", "Simon Willison", "Reddit r/LocalLLaMA", "Time Extension (retro)", "Reddit r/retrogaming", "Rock Paper Shotgun",
    "Polygon (games)", "Deadline", "/Film", "Bloody Disgusting (horror)", "Reactor (sci-fi & fantasy)", "Reddit r/printSF",
];

/// One place stories come from.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PodcastSource {
    /// "feed" (any RSS/Atom address), "reddit" (a subreddit, or your private front-page feed
    /// address), "x" (an account, "home" or "list/<id>", read in the remote browser),
    /// "instagram" (an account or "home", read in the remote browser), "youtube" (a channel's address or id), "hackernews"
    /// ("best", "frontpage", "newest", "ask", "show"), "lobsters" ("hottest", "newest" or a
    /// tag), "bluesky" (a handle), "mastodon" ("@name@instance" or "#tag@instance").
    pub kind: String,
    pub value: String,
    pub enabled: bool,
}

/// The second host offered when switching to two.
pub fn second_host() -> PodcastHost {
    PodcastHost {
        name: "Theo".into(),
        voice: "A man in his forties with a relaxed, dry-witted British voice, Southern English accent, thoughtful and wry.".into(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PodcastHost {
    pub name: String,
    /// How the voice sounds, in words: VoiceDesign makes the voice from it.
    pub voice: String,
}

/// An episode as the player sees it. Lines get their recordings one by one, so playing can
/// start as soon as the first is ready.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Episode {
    pub id: String,
    pub title: String,
    /// When it was started (RFC 3339).
    pub created: String,
    /// "gathering", "choosing", "reading", "writing", "recording", "ready" or "failed".
    pub stage: String,
    /// What it's doing right now, for the progress line.
    pub doing: String,
    pub error: Option<String>,
    pub segments: Vec<Segment>,
    /// Hosts' names, in voice order.
    pub hosts: Vec<String>,
}

impl Episode {
    pub fn finished(&self) -> bool {
        matches!(self.stage.as_str(), "ready" | "failed")
    }
}

/// The intro, one story, or the sign-off.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Segment {
    /// Names its recordings: `/api/podcast/episodes/{id}/audio/{key}/{line}`.
    pub key: String,
    pub title: String,
    /// "intro", "story", "detour" (somewhere the listener asked to go) or "outro".
    pub kind: String,
    pub sources: Vec<SourceLink>,
    /// The script is still being written.
    pub writing: bool,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SourceLink {
    pub title: String,
    pub url: String,
    /// "Reddit · r/worldnews", "The Verge", "X · @someone"...
    pub site: String,
    pub image: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Line {
    pub speaker: String,
    /// With inline acting directions ("[laughing] No way."), which the voices follow.
    pub text: String,
    /// Recorded: `/api/podcast/episodes/{id}/audio/{key}/{line}` plays it.
    pub ready: bool,
}

/// A summary for the episode list.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EpisodeSummary {
    pub id: String,
    pub title: String,
    pub created: String,
    pub stage: String,
    pub stories: usize,
}

/// A question asked while listening.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PodcastAsk {
    pub episode: String,
    /// Where the listener is: the story being talked about.
    pub segment: usize,
    pub line: usize,
    pub question: String,
    #[serde(default)]
    pub history: Vec<crate::ChatTurn>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PodcastAnswer {
    pub speaker: String,
    /// As shown (directions taken out).
    pub text: String,
    /// As spoken, for the history.
    pub spoken: String,
    /// An mp3 data URL, when the voices are up.
    pub audio: Option<String>,
    /// What was found while looking into it.
    #[serde(default)]
    pub links: Vec<SourceLink>,
    /// A detour being made on it (the segment's key), to play next.
    #[serde(default)]
    pub detour: Option<String>,
}

/// A host's voice clip and whether it's been made.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HostVoice {
    pub name: String,
    pub voice: String,
    pub clip: bool,
}

/// Drops inline acting directions ("[warmly]") for showing a line as text.
pub fn strip_directions(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
