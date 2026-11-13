//! Podcasts you subscribe to (not the AI-made daily briefing, see `podcast`): shows from their
//! RSS feeds, their episodes, where you are in each, and the ads found in them so the player
//! can skip them.

use serde::{Deserialize, Serialize};

/// A show you follow.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Show {
    pub id: i64,
    pub feed_url: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub image: Option<String>,
    pub link: String,
    /// "rss" (a podcast feed) or "youtube" (a channel or playlist, played as audio).
    pub kind: String,
    /// Skip the ads found in this show's episodes (on unless you turn it off for a show).
    pub skip_ads: bool,
    /// Episodes not played to the end.
    pub unplayed: i64,
    /// When its newest episode came out (RFC 3339).
    pub latest: Option<String>,
    pub episodes: i64,
}

/// One episode of a show.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShowEpisode {
    pub id: i64,
    pub show_id: i64,
    pub show_title: String,
    /// The show's ads are skipped (its own switch).
    pub show_skip_ads: bool,
    pub title: String,
    /// The show notes, as plain text.
    pub description: String,
    /// The show notes as the feed gave them (HTML), for the notes pane.
    pub notes_html: String,
    /// RFC 3339.
    pub published: Option<String>,
    /// Seconds (0 when the feed doesn't say).
    pub duration: f64,
    pub image: Option<String>,
    /// The recording on the show's own server (played from there when it isn't downloaded).
    pub audio_url: String,
    /// Can only be played once downloaded (a YouTube video, whose sound duet pulls out).
    pub needs_download: bool,
    /// Where you got to, in seconds.
    pub position: f64,
    pub finished: bool,
    /// The recording is on the server (played from there, and searched for ads).
    pub downloaded: bool,
    /// How far the ad finder has got, and what it found.
    pub ads: AdScan,
}

/// The ad finder's progress on an episode and the ads it found.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AdScan {
    /// "" (not looked at), "queued", "downloading", "listening", "done" or "failed".
    pub state: String,
    /// What it's doing, for the progress line ("Downloading 40%", "Listening 12:00 of 58:31").
    pub doing: String,
    /// How far through the current step (0 to 1): the download, or the listening.
    pub progress: f64,
    /// Seconds of the episode listened to and checked so far (ads before here are known).
    pub checked: f64,
    pub duration: f64,
    pub ads: Vec<AdSpan>,
    pub error: Option<String>,
}

impl AdScan {
    pub fn busy(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "downloading" | "listening")
    }

    /// Seconds of ads in the episode found so far.
    pub fn ad_seconds(&self) -> f64 {
        self.ads.iter().map(|a| (a.end - a.start).max(0.0)).sum()
    }
}

/// A stretch of an episode that's an ad.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AdSpan {
    pub start: f64,
    pub end: f64,
    /// What it's for ("Squarespace", "Promo for another show").
    pub label: String,
}

/// A show found by searching (or in the charts), not yet followed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShowHit {
    pub title: String,
    pub author: String,
    pub image: Option<String>,
    pub feed_url: String,
    pub genre: String,
    pub episodes: i64,
    /// Already followed: its id here.
    pub followed: Option<i64>,
    /// Its place in the chart (1 is the top), for the charts.
    pub rank: i64,
    /// Where it was found: "apple" or "podcastindex".
    pub source: String,
}

/// Apple's podcast categories: (id, name), for the charts.
pub const GENRES: [(i64, &str); 19] = [
    (1318, "Technology"),
    (1303, "Comedy"),
    (1489, "News"),
    (1488, "True Crime"),
    (1324, "Society & Culture"),
    (1533, "Science"),
    (1487, "History"),
    (1309, "TV & Film"),
    (1483, "Fiction"),
    (1321, "Business"),
    (1545, "Sports"),
    (1512, "Health & Fitness"),
    (1304, "Education"),
    (1301, "Arts"),
    (1310, "Music"),
    (1502, "Leisure"),
    (1305, "Kids & Family"),
    (1314, "Religion & Spirituality"),
    (1511, "Government"),
];

/// The Discover page: a chart (all shows, or one category).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Chart {
    pub shows: Vec<ShowHit>,
    /// What's rising on Podcast Index, when it's set up (the "all" chart only).
    pub trending: Vec<ShowHit>,
}

/// A show's page: the show and its episodes, newest first.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShowPage {
    pub show: Show,
    pub episodes: Vec<ShowEpisode>,
}

/// The Podcasts tab's front page.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShowsHome {
    pub shows: Vec<Show>,
    /// Started and not finished, most recently played first.
    pub in_progress: Vec<ShowEpisode>,
    /// The newest unplayed episodes across your shows.
    pub latest: Vec<ShowEpisode>,
}

/// How podcasts play and how ads are found. Saved as you change them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShowSettings {
    /// Skip ads the finder has found (each show can opt out).
    pub skip_ads: bool,
    /// Download and check new episodes as they come out, so they're ready ad-free.
    pub prepare_new: bool,
    /// What reads the transcript for ads: "chat" (the chat model in Settings → AI) or "local"
    /// (the local model, Ollama or similar, at Settings → AI's local address).
    pub ad_model: String,
    /// Also skip promos for other shows and the show's own "support us" breaks.
    pub skip_promos: bool,
    pub back_seconds: u32,
    pub forward_seconds: u32,
    /// Playback speed.
    pub speed: f64,
    /// Downloaded episodes you've finished are deleted after this many days (0: kept).
    pub keep_days: u32,
    /// Whose charts Discover shows (a two-letter country code).
    pub country: String,
    /// Podcast Index (podcastindex.org, free): more shows in search, and what's trending.
    pub podcastindex_key: String,
    pub podcastindex_secret: String,
    /// For YouTube shows: skip the segments SponsorBlock's community has marked (sponsors,
    /// and with promos on, self-promotion and "like and subscribe"), before asking the AI.
    pub sponsorblock: bool,
}

impl Default for ShowSettings {
    fn default() -> Self {
        ShowSettings {
            skip_ads: true,
            prepare_new: true,
            ad_model: "chat".into(),
            skip_promos: true,
            back_seconds: 15,
            forward_seconds: 30,
            speed: 1.0,
            keep_days: 7,
            country: "us".into(),
            podcastindex_key: String::new(),
            podcastindex_secret: String::new(),
            sponsorblock: true,
        }
    }
}

/// Where you are in an episode (sent while it plays).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShowPosition {
    pub position: f64,
    pub duration: f64,
    pub finished: bool,
}
