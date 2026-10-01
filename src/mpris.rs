//! ytfast as an MPRIS player (`org.mpris.MediaPlayer2.ytfast`): media keys,
//! `playerctl` and the Omarchy bar's media widget see the song, cover,
//! position and controls.
//!
//! The service runs on the backend's tokio runtime, fed by the backend's
//! playback state ([`Now`]) and not by the window, so it keeps answering with
//! the window closed. Method calls become backend commands (through
//! [`Remote`]); Raise, Quit and OpenUri go to the interface as requests.
//!
//! The shape follows Spotifast's MPRIS module (MIT, github.com/crmne/spotifast,
//! `src/mpris.rs`): publish structural changes at once and only when they
//! change, let clients read the position rather than signalling it, and
//! signal `Seeked` on jumps. Spotifast uses the `mpris-server` crate on a
//! thread of its own; ytfast serves the two interfaces with zbus directly.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, watch};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Str, Value};

use crate::backend::Command;
use crate::desktop::{Flags, Now, Remote, Request};
use crate::model::{Repeat, Track};
use crate::paths::Paths;

const BUS_NAME: &str = "org.mpris.MediaPlayer2.ytfast";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const TRACK_PATH: &str = "/org/mpris/MediaPlayer2/ytfast/track/";
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";
/// A position this far from where playback should have got to is a seek.
const SEEK_THRESHOLD: f64 = 1.5;

/// Connects to the session bus and serves MPRIS, and song-change
/// notifications (`notify`), until the runtime ends. Without a session bus
/// ytfast runs on without them.
pub fn start(
    runtime: &tokio::runtime::Handle,
    remote: Remote,
    now: watch::Receiver<Now>,
    flags: Arc<Flags>,
    paths: Paths,
    http: reqwest::Client,
) {
    runtime.spawn(async move {
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(error) => {
                log::warn!("no session bus, so no MPRIS or notifications: {error}");
                return;
            }
        };
        tokio::spawn(crate::notify::run(
            connection.clone(),
            now.clone(),
            flags,
            paths.clone(),
            http.clone(),
        ));
        if let Err(error) = serve(&connection, remote, now, paths, http).await {
            log::warn!("MPRIS is unavailable: {error}");
        }
    });
}

async fn serve(
    connection: &zbus::Connection,
    remote: Remote,
    mut now: watch::Receiver<Now>,
    paths: Paths,
    http: reqwest::Client,
) -> zbus::Result<()> {
    let server = connection.object_server();
    server
        .at(
            OBJECT_PATH,
            Root {
                remote: remote.clone(),
            },
        )
        .await?;
    server
        .at(
            OBJECT_PATH,
            Player {
                remote,
                shown: Shown::default(),
                position: 0.0,
                at: Instant::now(),
            },
        )
        .await?;
    connection.request_name(BUS_NAME).await?;
    log::info!("MPRIS: serving {BUS_NAME}");
    let player = server.interface::<_, Player>(OBJECT_PATH).await?;
    let emitter = player.signal_emitter();
    // Covers downloaded for the song that was playing: (video id, file:// URL).
    let (cover_tx, mut covers) = mpsc::unbounded_channel::<(String, String)>();
    loop {
        tokio::select! {
            changed = now.changed() => {
                if changed.is_err() {
                    break;
                }
                let (next, position) = {
                    let now = now.borrow_and_update();
                    (Shown::of(&now), now.playback.position)
                };
                let (old, seeked) = {
                    let mut iface = player.get_mut().await;
                    let expected = iface.position_now();
                    let same_track = iface.shown.video_id() == next.video_id();
                    let mut next = next;
                    if same_track {
                        // The cover found or downloaded when the song started.
                        next.art.clone_from(&iface.shown.art);
                    } else if let Some(track) = &next.track
                        && let Some(url) = track.thumbnail.clone()
                    {
                        let file = paths.cover_file(&url);
                        if std::fs::metadata(&file).is_ok_and(|m| m.len() > 0) {
                            next.art = Some(format!("file://{}", file.display()));
                        } else {
                            next.art = Some(url.clone());
                            let (http, paths, tx) = (http.clone(), paths.clone(), cover_tx.clone());
                            let id = track.video_id.clone();
                            tokio::spawn(async move {
                                let file = crate::desktop::cached_cover(&http, &paths, &url).await;
                                if let Some(file) = file {
                                    let _ = tx.send((id, format!("file://{}", file.display())));
                                }
                            });
                        }
                    }
                    iface.position = position;
                    iface.at = Instant::now();
                    let old = std::mem::replace(&mut iface.shown, next);
                    let seeked = same_track
                        && old.video_id().is_some()
                        && (position - expected).abs() > SEEK_THRESHOLD;
                    (old, seeked)
                };
                let iface = player.get().await;
                changed_properties(&iface, &old, emitter).await;
                if seeked {
                    let _ = Player::seeked(emitter, micros(position)).await;
                }
            }
            Some((video_id, art)) = covers.recv() => {
                let mut iface = player.get_mut().await;
                if iface.shown.video_id() == Some(video_id.as_str()) {
                    iface.shown.art = Some(art);
                    drop(iface);
                    let _ = player.get().await.metadata_changed(emitter).await;
                }
            }
        }
    }
    Ok(())
}

/// Signals `PropertiesChanged` for what differs from `old`.
async fn changed_properties(iface: &Player, old: &Shown, emitter: &SignalEmitter<'_>) {
    let new = &iface.shown;
    if new.status() != old.status() {
        let _ = iface.playback_status_changed(emitter).await;
    }
    if new.track != old.track || new.art != old.art || new.length() != old.length() {
        let _ = iface.metadata_changed(emitter).await;
    }
    if (new.volume - old.volume).abs() > 0.001 {
        let _ = iface.volume_changed(emitter).await;
    }
    if new.shuffle != old.shuffle {
        let _ = iface.shuffle_changed(emitter).await;
    }
    if new.repeat != old.repeat {
        let _ = iface.loop_status_changed(emitter).await;
    }
    if new.can_go_next != old.can_go_next {
        let _ = iface.can_go_next_changed(emitter).await;
    }
    if new.can_go_previous != old.can_go_previous {
        let _ = iface.can_go_previous_changed(emitter).await;
    }
    if new.track.is_some() != old.track.is_some() {
        let _ = iface.can_play_changed(emitter).await;
        let _ = iface.can_pause_changed(emitter).await;
    }
    if new.can_seek() != old.can_seek() {
        let _ = iface.can_seek_changed(emitter).await;
    }
}

/// What the player shows on the bus, apart from the position.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    track: Option<Track>,
    /// The cover: the cached file as `file://` when there is one, else its URL.
    art: Option<String>,
    playing: bool,
    /// Seconds; mpv's figure once it knows, else YouTube's.
    duration: f64,
    /// 0.0–1.0.
    volume: f64,
    shuffle: bool,
    repeat: Repeat,
    can_go_next: bool,
    can_go_previous: bool,
}

impl Shown {
    /// The state to show, without the cover (looked up once per song).
    fn of(now: &Now) -> Self {
        let track = now.track().cloned();
        let pb = &now.playback;
        Self {
            can_go_next: track.is_some(),
            can_go_previous: track.is_some(),
            art: None,
            playing: pb.playing,
            duration: if pb.duration > 0.0 {
                pb.duration
            } else {
                track
                    .as_ref()
                    .and_then(|t| t.duration)
                    .map(f64::from)
                    .unwrap_or(0.0)
            },
            volume: (pb.volume / 100.0).clamp(0.0, 1.0),
            shuffle: pb.shuffle,
            repeat: pb.repeat,
            track,
        }
    }

    fn video_id(&self) -> Option<&str> {
        self.track.as_ref().map(|t| t.video_id.as_str())
    }

    fn status(&self) -> &'static str {
        match (&self.track, self.playing) {
            (None, _) => "Stopped",
            (Some(_), true) => "Playing",
            (Some(_), false) => "Paused",
        }
    }

    /// Whole seconds, so mpv refining the duration doesn't republish.
    fn length(&self) -> i64 {
        self.duration.round() as i64
    }

    fn can_seek(&self) -> bool {
        self.track.is_some() && self.duration > 0.0
    }
}

fn micros(seconds: f64) -> i64 {
    (seconds.max(0.0) * 1_000_000.0) as i64
}

/// The track's D-Bus object path: object paths allow only `[A-Za-z0-9_]`,
/// so `-` and `_` in the video id are escaped.
fn track_path(video_id: &str) -> String {
    let mut path = String::from(TRACK_PATH);
    for c in video_id.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => path.push(c),
            other => path.push_str(&format!("_{:02x}", u32::from(other))),
        }
    }
    path
}

fn object_path(path: String) -> ObjectPath<'static> {
    ObjectPath::try_from(path).unwrap_or_else(|_| ObjectPath::from_static_str_unchecked(NO_TRACK))
}

/// `org.mpris.MediaPlayer2`: the application.
struct Root {
    remote: Remote,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        self.remote.request(Request::Show);
    }

    fn quit(&self) {
        self.remote.request(Request::Quit);
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn fullscreen(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_set_fullscreen(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> String {
        "Music".into()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        "ytfast".into()
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["https".into()]
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// `org.mpris.MediaPlayer2.Player`: playback.
struct Player {
    remote: Remote,
    shown: Shown,
    /// The position last reported, and when.
    position: f64,
    at: Instant,
}

impl Player {
    /// Where playback is by now: the last report plus the time since, while playing.
    fn position_now(&self) -> f64 {
        let mut position = self.position;
        if self.shown.playing {
            position += self.at.elapsed().as_secs_f64();
        }
        if self.shown.duration > 0.0 {
            position = position.min(self.shown.duration);
        }
        position
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.remote.command(Command::Next);
    }

    fn previous(&self) {
        self.remote.command(Command::Previous);
    }

    fn pause(&self) {
        self.remote.pause();
    }

    fn play_pause(&self) {
        self.remote.toggle();
    }

    /// ytfast has no stopped state with a song loaded; Stop pauses.
    fn stop(&self) {
        self.remote.pause();
    }

    fn play(&self) {
        self.remote.play();
    }

    /// Seeks by `offset` microseconds; past the end is the next song.
    fn seek(&self, offset: i64) {
        if !self.shown.can_seek() {
            return;
        }
        let target = self.position_now() + offset as f64 / 1_000_000.0;
        if target >= self.shown.duration {
            self.remote.command(Command::Next);
        } else {
            self.remote.command(Command::Seek(target.max(0.0)));
        }
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        let current = self.shown.video_id().map(track_path);
        if current.as_deref() != Some(track_id.as_str()) || !self.shown.can_seek() {
            return;
        }
        let seconds = position as f64 / 1_000_000.0;
        if (0.0..=self.shown.duration).contains(&seconds) {
            self.remote.command(Command::Seek(seconds));
        }
    }

    fn open_uri(&self, uri: String) -> zbus::fdo::Result<()> {
        if crate::links::target_from_link(&uri).is_none() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "not a YouTube Music or YouTube link".into(),
            ));
        }
        self.remote.request(Request::Open(uri));
        Ok(())
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.shown.status().into()
    }

    #[zbus(property)]
    fn loop_status(&self) -> String {
        match self.shown.repeat {
            Repeat::Off => "None",
            Repeat::All => "Playlist",
            Repeat::One => "Track",
        }
        .into()
    }

    #[zbus(property)]
    fn set_loop_status(&mut self, status: String) {
        let wanted = match status.as_str() {
            "None" => Repeat::Off,
            "Playlist" => Repeat::All,
            "Track" => Repeat::One,
            _ => return,
        };
        // The backend cycles Off → All → One → Off.
        let order = [Repeat::Off, Repeat::All, Repeat::One];
        let index = |r: Repeat| order.iter().position(|&o| o == r).unwrap_or(0);
        let steps = (index(wanted) + 3 - index(self.shown.repeat)) % 3;
        for _ in 0..steps {
            self.remote.command(Command::CycleRepeat);
        }
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn set_rate(&mut self, _rate: f64) {}

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.shown.shuffle
    }

    #[zbus(property)]
    fn set_shuffle(&mut self, shuffle: bool) {
        if shuffle != self.shown.shuffle {
            self.remote.command(Command::ToggleShuffle);
        }
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();
        let Some(track) = &self.shown.track else {
            map.insert(
                "mpris:trackid".to_owned(),
                OwnedValue::from(ObjectPath::from_static_str_unchecked(NO_TRACK)),
            );
            return map;
        };
        let mut insert = |key: &str, value: OwnedValue| {
            map.insert(key.to_owned(), value);
        };
        insert(
            "mpris:trackid",
            OwnedValue::from(object_path(track_path(&track.video_id))),
        );
        insert(
            "xesam:title",
            OwnedValue::from(Str::from(track.title.clone())),
        );
        // Linked runs are the artists; the rest are separators (", ", " & ").
        let mut artists: Vec<String> = track
            .artists
            .iter()
            .filter(|r| r.target.is_some())
            .map(|r| r.text.clone())
            .collect();
        if artists.is_empty() && !track.artists.is_empty() {
            artists.push(track.artist_line());
        }
        if let Ok(value) = OwnedValue::try_from(Value::from(artists)) {
            insert("xesam:artist", value);
        }
        if let Some(album) = &track.album {
            insert(
                "xesam:album",
                OwnedValue::from(Str::from(album.text.clone())),
            );
        }
        if let Some(art) = &self.shown.art {
            insert("mpris:artUrl", OwnedValue::from(Str::from(art.clone())));
        }
        if self.shown.duration > 0.0 {
            insert(
                "mpris:length",
                OwnedValue::from(micros(self.shown.duration)),
            );
        }
        insert(
            "xesam:url",
            OwnedValue::from(Str::from(format!(
                "https://music.youtube.com/watch?v={}",
                track.video_id
            ))),
        );
        map
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.shown.volume
    }

    #[zbus(property)]
    fn set_volume(&mut self, volume: f64) {
        if volume.is_finite() {
            self.remote
                .command(Command::Volume(volume.clamp(0.0, 1.0) * 100.0));
        }
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        micros(self.position_now())
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.shown.can_go_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.shown.can_go_previous
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.shown.track.is_some()
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.shown.track.is_some()
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.shown.can_seek()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}
