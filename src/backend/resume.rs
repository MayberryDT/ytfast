//! The session across launches, in `~/.cache/ytfast/session.json`: the
//! queue (play order, list order and additions), the current song and its
//! position, volume, shuffle, repeat and autoplay. It is saved on pause,
//! track changes, queue changes and exit, and every ten seconds while
//! playing. At launch it comes back paused, and the current song resolves
//! ahead so Play starts at once from the same position.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::*;

#[derive(Serialize, Deserialize)]
struct Saved {
    queue: queue::Snapshot,
    /// The current song's position in the play order.
    index: Option<usize>,
    /// Seconds into it.
    position: f64,
    volume: f64,
    shuffle: bool,
    repeat: Repeat,
    autoplay: bool,
}

/// How often the session is saved while only the position changes.
const EVERY: Duration = Duration::from_secs(10);

impl super::Worker {
    fn session_file(&self) -> PathBuf {
        self.paths.cache.join("session.json")
    }

    /// Saves the session now (`now`), or if the last save is ten seconds
    /// old; exit saves whatever is newer.
    pub(super) fn save_session(&mut self, now: bool) {
        if !now && self.last_save.elapsed() < EVERY {
            return;
        }
        self.last_save = Instant::now();
        let position = match (self.current_entry, self.resume_at) {
            (None, Some(at)) => at,
            _ => self.state.position,
        };
        let saved = Saved {
            queue: self.queue.snapshot(),
            index: self.pos,
            position,
            volume: self.state.volume,
            shuffle: self.state.shuffle,
            repeat: self.state.repeat,
            autoplay: self.state.autoplay,
        };
        let written = serde_json::to_vec(&saved)
            .map_err(std::io::Error::other)
            .and_then(|bytes| crate::paths::write_atomic(&self.session_file(), &bytes));
        if let Err(error) = written {
            log::warn!("couldn't save the session: {error}");
        }
    }

    /// Brings back the last session, paused: the player bar shows the song
    /// and position at once.
    pub(super) fn restore_session(&mut self) {
        let Ok(bytes) = std::fs::read(self.session_file()) else {
            return;
        };
        let saved: Saved = match serde_json::from_slice(&bytes) {
            Ok(saved) => saved,
            Err(error) => {
                log::warn!("ignoring a damaged session file: {error}");
                return;
            }
        };
        self.state.volume = saved.volume.clamp(0.0, 100.0);
        self.state.shuffle = saved.shuffle;
        self.state.repeat = saved.repeat;
        self.state.autoplay = saved.autoplay;
        if let Some(queue) = queue::Queue::restore(saved.queue)
            && !queue.is_empty()
        {
            self.queue = queue;
            self.pos = saved.index.filter(|&i| i < self.queue.len());
            self.state.index = self.pos;
            if let Some(track) = self.current().cloned() {
                let position = saved.position.max(0.0);
                self.state.position = position;
                self.state.duration = track.duration.map(f64::from).unwrap_or(0.0);
                self.resume_at = Some(position);
                self.fetch_watch_info(&track.video_id);
            }
            self.sink.send(Event::Queue(self.queue.tracks()));
            log::info!(
                "restored the session: {} songs, song {:?} at {:.0}s",
                self.queue.len(),
                self.pos,
                self.state.position
            );
        }
        self.last_save = Instant::now();
        self.emit(true);
    }

    /// Once the session is known (at launch): resolve the restored song for
    /// playback and fetch its loudness, so Play starts at once.
    pub(super) fn prepare_restored(&mut self) {
        if self.current_entry.is_some() || self.state.loading || self.resolving.is_some() {
            return;
        }
        let Some(video_id) = self.current().map(|t| t.video_id.clone()) else {
            return;
        };
        let request = self.resolver.request(&video_id);
        let task = tokio::spawn(async move {
            let _ = request.wait().await;
        });
        self.resolving = Some(task.abort_handle());
        self.fetch_player(&video_id);
    }
}
