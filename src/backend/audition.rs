//! Audition: a song held under the pointer plays from its best part on a
//! deck of its own, fading in over the current song, which ducks to a fifth
//! of its amplitude and keeps playing underneath. Letting go fades the
//! audition out and brings the current song back up where it now is, with
//! no gap. An audition never touches the queue, the session or history.
//!
//! Its stream comes through the resolver's speculative path: songs under
//! the pointer are usually prepared already; otherwise the row shows the
//! audition as loading until it can start, and letting go first cancels it.

use super::deck::{DUCK, FADE, Message, Ramp};
use super::*;
use crate::model::Audition as Shown;

/// How long the audition deck stays after the last audition.
const KEEP: Duration = Duration::from_secs(120);

pub(super) struct Audition {
    deck: Option<Arc<Mpv>>,
    /// Bumped by every request and release; late answers for older ones are dropped.
    stamp: u64,
    /// The song held and where it starts.
    held: Option<(String, Option<f64>)>,
    resolving: Option<tokio::task::AbortHandle>,
    /// The audition deck's playlist entry id for the song loaded on it, and
    /// whether that file has started.
    entry: Option<i64>,
    started: bool,
    /// The audition's share of the volume (amplitude).
    level: Ramp,
}

impl Default for Audition {
    fn default() -> Self {
        Self {
            deck: None,
            stamp: 0,
            held: None,
            resolving: None,
            entry: None,
            started: false,
            level: Ramp::steady(0.0),
        }
    }
}

impl Audition {
    pub fn deck(&self) -> Option<&Arc<Mpv>> {
        self.deck.as_ref()
    }

    pub fn fading(&self) -> bool {
        self.level.moving()
    }
}

impl super::Worker {
    /// Holds `track`: it plays from `start` seconds in (a third of the way
    /// when `None`) over the ducked current song.
    pub(super) async fn audition(&mut self, track: Track, start: Option<f64>) {
        let video_id = track.video_id;
        let a = &mut self.decks.audition;
        if a.held.as_ref().is_some_and(|(id, _)| *id == video_id) {
            return;
        }
        a.stamp += 1;
        if let Some(old) = a.resolving.take() {
            old.abort();
        }
        a.held = Some((video_id.clone(), start));
        let stamp = a.stamp;
        self.state.audition = Some(Shown {
            video_id: video_id.clone(),
            playing: false,
        });
        self.emit(true);
        self.fetch_player(&video_id);
        if let Some(stream) = self.resolver.cached(&video_id) {
            self.audition_start(stamp, stream).await;
            return;
        }
        let resolver = self.resolver.clone();
        let tx = self.internal_tx.clone();
        let task = tokio::spawn(async move {
            let stream = resolver.prepared(&video_id).await;
            let _ = tx.send(Internal::Deck(Message::AuditionReady { stamp, stream }));
        });
        self.decks.audition.resolving = Some(task.abort_handle());
    }

    /// Lets go: the audition fades out and the current song comes back up.
    pub(super) async fn end_audition(&mut self) {
        let a = &mut self.decks.audition;
        a.stamp += 1;
        if let Some(old) = a.resolving.take() {
            old.abort();
        }
        a.held = None;
        a.level = a.level.toward(0.0, FADE);
        self.decks.duck = self.decks.duck.toward(1.0, FADE);
        if self.state.audition.take().is_some() {
            self.emit(true);
        }
        self.keep_time();
        let stamp = self.decks.audition.stamp;
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(KEEP).await;
            let _ = tx.send(Internal::Deck(Message::AuditionIdle { stamp }));
        });
    }

    pub(super) async fn audition_ready(&mut self, stamp: u64, stream: anyhow::Result<Stream>) {
        if stamp != self.decks.audition.stamp {
            return;
        }
        self.decks.audition.resolving = None;
        match stream {
            Ok(stream) => self.audition_start(stamp, stream).await,
            Err(error) => {
                log::warn!("couldn't audition a song: {error:#}");
                self.end_audition().await;
            }
        }
    }

    /// Loads the held song on the audition deck, silent; it fades in once
    /// its audio starts.
    async fn audition_start(&mut self, stamp: u64, stream: Stream) {
        let Some((video_id, start)) = self.decks.audition.held.clone() else {
            return;
        };
        let deck = match self.decks.audition.deck.clone() {
            Some(deck) => deck,
            None => match Mpv::spawn(
                &self.paths.runtime.join("mpv-audition.sock"),
                0.0,
                self.mpv_tx.clone(),
            )
            .await
            {
                Ok(deck) => {
                    let filter = self.state.equalizer.filter();
                    let af = if self.af.is_empty() {
                        &filter
                    } else {
                        &self.af
                    };
                    let _ = deck.set("af", json!(af)).await;
                    self.decks.audition.deck = Some(deck.clone());
                    deck
                }
                Err(error) => {
                    log::warn!("couldn't start the audition player: {error:#}");
                    self.end_audition().await;
                    return;
                }
            },
        };
        if stamp != self.decks.audition.stamp {
            return;
        }
        // Another song may still be fading out on the deck: it stops here.
        self.decks.audition.level = Ramp::steady(0.0);
        let _ = deck.set("volume", json!(0.0)).await;
        let gain = self.gain_for(&video_id).unwrap_or(0.0);
        let mut options = vec![("volume-gain", format!("{gain:.2}"))];
        if let Some(agent) = &stream.user_agent {
            options.push(("user-agent", agent.clone()));
        }
        // mpv takes "33%" as a third of the way in, once it knows the length.
        let at = start.map_or_else(|| "33%".to_owned(), |s| format!("{:.2}", s.max(0.0)));
        options.push(("start", at));
        match deck.load(&stream.url, "replace", &options).await {
            Ok(entry) => {
                let _ = deck.set("pause", json!(false)).await;
                self.decks.audition.entry = Some(entry);
                self.decks.audition.started = false;
                log::info!("auditioning {video_id}");
            }
            Err(error) => {
                log::warn!("couldn't audition {video_id}: {error:#}");
                self.end_audition().await;
            }
        }
    }

    pub(super) async fn audition_event(&mut self, event: MpvEvent) {
        let a = &mut self.decks.audition;
        match event {
            MpvEvent::StartFile { entry } => {
                if Some(entry) == a.entry {
                    a.started = true;
                }
            }
            MpvEvent::Property { name, data } if name == "time-pos" => {
                // Audio is coming: fade in over the ducking current song.
                if a.started && a.held.is_some() && data.as_f64().is_some() {
                    a.started = false;
                    a.level = a.level.toward(1.0, FADE);
                    self.decks.duck = self.decks.duck.toward(DUCK, FADE);
                    if let Some(shown) = &mut self.state.audition {
                        shown.playing = true;
                    }
                    self.emit(true);
                    self.keep_time();
                }
            }
            MpvEvent::EndFile { entry, reason, .. } => {
                if Some(entry) == a.entry && reason != "stop" {
                    // The song ended or failed while held.
                    a.entry = None;
                    if a.held.is_some() {
                        self.end_audition().await;
                    }
                }
            }
            MpvEvent::Died => {
                a.deck = None;
                a.entry = None;
                if a.held.is_some() {
                    self.end_audition().await;
                }
            }
            MpvEvent::Property { .. } => {}
        }
    }

    pub(super) fn audition_volume(&self) -> f64 {
        self.state.volume * self.decks.audition.level.now().cbrt()
    }

    /// Faded out after letting go: the audition stops.
    pub(super) async fn audition_faded(&mut self) {
        let a = &mut self.decks.audition;
        if a.held.is_none() && a.entry.is_some() && a.level.target() == 0.0 && !a.level.moving() {
            a.entry = None;
            if let Some(deck) = &a.deck {
                let _ = deck.command(json!(["stop"])).await;
            }
        }
    }

    /// Nothing auditioned for a while: the audition deck goes.
    pub(super) fn audition_idle(&mut self, stamp: u64) {
        let a = &mut self.decks.audition;
        if stamp == a.stamp && a.held.is_none() && a.entry.is_none() {
            a.deck = None;
        }
    }

    /// A player response arrived: an audition of that song takes its gain.
    pub(super) async fn audition_gain(&mut self, video_id: &str) {
        let a = &self.decks.audition;
        if a.entry.is_none() || a.held.as_ref().is_none_or(|(id, _)| id != video_id) {
            return;
        }
        if let (Some(deck), Some(gain)) = (&a.deck, self.gain_for(video_id)) {
            let _ = deck.set("volume-gain", json!(gain)).await;
        }
    }
}
