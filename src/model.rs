//! ytfast's own types. InnerTube responses are translated into these in
//! `parse`; views never touch raw JSON.

use serde::{Deserialize, Serialize};

/// What activating something does.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Target {
    /// Open a browse page: album, artist, playlist, mood, lyrics, related…
    Browse { id: String, params: Option<String> },
    /// Start playback through the watch-next endpoint: a song (optionally in
    /// a playlist or radio) or a whole playlist/album/mix.
    Watch {
        video_id: Option<String>,
        playlist_id: Option<String>,
        params: Option<String>,
    },
    /// Run a search, possibly filtered to one type ("Songs", "Albums"…).
    Search {
        query: String,
        params: Option<String>,
    },
}

impl Target {
    pub fn browse(id: impl Into<String>) -> Self {
        Self::Browse {
            id: id.into(),
            params: None,
        }
    }

    /// A stable key for caching the page this target opens.
    pub fn key(&self) -> String {
        match self {
            Target::Browse { id, params } => {
                format!("browse:{id}:{}", params.as_deref().unwrap_or(""))
            }
            Target::Watch {
                video_id,
                playlist_id,
                params,
            } => format!(
                "watch:{}:{}:{}",
                video_id.as_deref().unwrap_or(""),
                playlist_id.as_deref().unwrap_or(""),
                params.as_deref().unwrap_or("")
            ),
            Target::Search { query, params } => {
                format!("search:{query}:{}", params.as_deref().unwrap_or(""))
            }
        }
    }
}

/// A piece of a subtitle: plain text, or a link (artist, album…).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    pub target: Option<Target>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    Song,
    Video,
    Album,
    Playlist,
    Artist,
    /// A mood/genre or navigation button.
    Button,
    Other,
}

/// A card, row or button in a shelf.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    pub title: String,
    pub subtitle: Vec<Run>,
    pub thumbnail: Option<String>,
    /// What clicking the item opens or plays.
    pub target: Option<Target>,
    /// The play button's action, for items that open a page.
    pub play: Option<Target>,
    /// For songs and videos: the track it is.
    pub track: Option<Track>,
    /// A row's leading number (album track, chart position).
    pub index: Option<String>,
    /// A mood button's stripe colour from YouTube Music (content, not chrome).
    pub stripe: Option<u32>,
    /// For a playlist card: the playlist's id, when the account can edit it.
    #[serde(default)]
    pub editable: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShelfStyle {
    /// A horizontal carousel of cards.
    Carousel,
    /// A horizontal carousel of song rows in columns (Quick picks).
    RowCarousel,
    /// A vertical list of rows.
    List,
    /// A wrapping grid of cards.
    Grid,
    /// A wrapping grid of navigation buttons.
    Buttons,
    /// A search top result: one large card.
    TopResult,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shelf {
    pub title: String,
    pub strapline: Option<String>,
    pub style: ShelfStyle,
    pub items: Vec<Item>,
    /// "More"/"Show all".
    pub more: Option<Target>,
    /// Loads more rows into this shelf (long playlists, library lists).
    pub continuation: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub title: String,
    pub subtitle: Vec<Run>,
    pub second_subtitle: String,
    pub description: Option<String>,
    pub thumbnail: Option<String>,
    pub round: bool,
    pub play: Option<Target>,
    pub shuffle: Option<Target>,
    pub radio: Option<Target>,
    /// Albums and other people's playlists: saving to the library.
    #[serde(default)]
    pub library: Option<LibraryToggle>,
    /// Artists: the channel to subscribe to.
    #[serde(default)]
    pub subscription: Option<Subscription>,
    /// The account's own playlist: its id, for editing.
    #[serde(default)]
    pub editable: Option<String>,
}

/// Whether an album or playlist is in the library, and the id that saves it
/// (an album's is its audio playlist, `OLAK5uy_…`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryToggle {
    pub playlist_id: String,
    pub saved: bool,
}

/// An artist's channel and whether the account is subscribed to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub channel_id: String,
    pub subscribed: bool,
}

/// A filter chip above a page (search types, library sections, home moods).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chip {
    pub text: String,
    pub target: Option<Target>,
    pub selected: bool,
    /// What choosing a selected chip again does (a Home mood back to Home).
    #[serde(default)]
    pub deselect: Option<Target>,
    /// A chip that swaps the page's shelves in place (an artist's
    /// discography: Albums, Singles & EPs): the continuation to load.
    #[serde(default)]
    pub reload: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub header: Option<Header>,
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    /// Loads more shelves (Home).
    pub continuation: Option<String>,
    /// A message YouTube Music shows instead of content ("No albums yet").
    pub message: Option<String>,
}

/// A playable song or video.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artists: Vec<Run>,
    pub album: Option<Run>,
    pub thumbnail: Option<String>,
    pub duration: Option<u32>,
    /// The account's rating, where the response gave it (playlist, album and
    /// library rows).
    #[serde(default)]
    pub like: Option<LikeStatus>,
    /// This entry's id in its playlist (`playlistSetVideoId`), for removing
    /// and moving it.
    #[serde(default)]
    pub set_video_id: Option<String>,
}

/// A song's rating on the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LikeStatus {
    Like,
    Dislike,
    Indifferent,
}

impl Track {
    pub fn artist_line(&self) -> String {
        self.artists
            .iter()
            .map(|r| r.text.as_str())
            .collect::<String>()
    }
}

/// The watch-next panel for a playing track.
#[derive(Clone, Debug, Default)]
pub struct WatchNext {
    pub tracks: Vec<Track>,
    /// Which of `tracks` the request asked for.
    pub current: usize,
    pub lyrics: Option<String>,
    pub related: Option<String>,
    /// Continues the queue as radio (autoplay).
    pub radio: Option<Target>,
    /// More queue items.
    pub continuation: Option<String>,
    /// The requested song and its rating on the account.
    pub like: Option<(String, LikeStatus)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lyrics {
    pub text: String,
    pub source: Option<String>,
    /// Timed lines, earliest first; empty when only plain lyrics exist.
    pub lines: Vec<LyricLine>,
}

/// One timed line of lyrics.
#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    /// When the line starts, in seconds into the song.
    pub start: f64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Account {
    Checking,
    SignedIn {
        name: String,
        photo: Option<String>,
        source: String,
    },
    SignedOut {
        reason: String,
    },
    /// Cookies were read but YouTube could not be asked (offline).
    Unverified {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

/// Where playback is, as the interface draws it. The queue itself is sent
/// separately, in play order, when it changes; `index` points into it.
#[derive(Clone, Debug, Default)]
pub struct Playback {
    pub index: Option<usize>,
    pub playing: bool,
    /// Resolving or buffering.
    pub loading: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub autoplay: bool,
    /// "Opus 256 kbps (itag 774)".
    pub format: Option<String>,
    pub lyrics: Option<String>,
    pub related: Option<String>,
    /// The next track is resolved and queued in the player for a gapless change.
    pub next_ready: bool,
}

/// Parses "3:45" or "1:02:03" into seconds.
pub fn parse_duration(text: &str) -> Option<u32> {
    let mut total = 0u32;
    for part in text.trim().split(':') {
        total = total.checked_mul(60)?.checked_add(part.parse().ok()?)?;
    }
    Some(total)
}

pub fn format_time(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
