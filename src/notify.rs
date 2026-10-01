//! Song-change notifications (Settings, off by default) through
//! `org.freedesktop.Notifications`: the title, the artists and the cached
//! cover. Each replaces the previous one. Only real song changes count
//! (not pause, resume or queue edits), and none appear while a ytfast window
//! has the focus: the song is on screen already.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use zbus::zvariant::Value;

use crate::desktop::{Flags, Now};
use crate::paths::Paths;

/// The last notification sent: the server's id and its title.
static LAST_SENT: Mutex<Option<(u32, String)>> = Mutex::new(None);

/// The id the notification server gave the last song notification, and the
/// song's title (for the E2E run).
pub fn last_sent() -> Option<(u32, String)> {
    LAST_SENT.lock().ok().and_then(|last| last.clone())
}

pub(crate) async fn run(
    connection: zbus::Connection,
    mut now: watch::Receiver<Now>,
    flags: Arc<Flags>,
    paths: Paths,
    http: reqwest::Client,
) {
    // The song last seen playing, so the same one is never announced twice.
    let mut last: Option<String> = None;
    let mut replaces: u32 = 0;
    while now.changed().await.is_ok() {
        let song = {
            let now = now.borrow_and_update();
            match now.track() {
                Some(track) if now.playback.playing => (
                    track.video_id.clone(),
                    track.title.clone(),
                    track.artist_line(),
                    track.thumbnail.clone(),
                ),
                _ => continue,
            }
        };
        let (video_id, title, artists, cover) = song;
        if last.as_deref() == Some(video_id.as_str()) {
            continue;
        }
        last = Some(video_id);
        if !flags.notifications.load(Ordering::Relaxed) || flags.focused.load(Ordering::Relaxed) {
            continue;
        }
        let image = match cover {
            Some(url) => tokio::time::timeout(
                Duration::from_secs(5),
                crate::desktop::cached_cover(&http, &paths, &url),
            )
            .await
            .ok()
            .flatten(),
            None => None,
        };
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("desktop-entry", Value::from("ytfast"));
        hints.insert("transient", Value::from(true));
        if let Some(file) = &image {
            hints.insert(
                "image-path",
                Value::from(format!("file://{}", file.display())),
            );
        }
        let reply = connection
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "Music",
                    replaces,
                    "audio-x-generic",
                    title.as_str(),
                    artists.as_str(),
                    Vec::<&str>::new(),
                    hints,
                    -1i32,
                ),
            )
            .await;
        match reply.and_then(|message| message.body().deserialize::<u32>()) {
            Ok(id) => {
                replaces = id;
                log::info!("notified song change (notification {id})");
                if let Ok(mut last) = LAST_SENT.lock() {
                    *last = Some((id, title));
                }
            }
            Err(error) => log::warn!("couldn't show a song notification: {error}"),
        }
    }
}
