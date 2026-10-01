//! More than one mpv at once ("decks"): Smooth mixes, and the volume of
//! every deck (ducking under an audition, blends, the sleep timer's fade).
//!
//! The current song always plays on `Worker::mpv`, the main deck, and the
//! main deck's events drive playback as they always did. With Smooth mixes
//! on, when the song after the current one is part of a radio, a mix or
//! autoplay, it is cued paused and silent on a second deck instead of being
//! appended behind the current song. Shortly before the current song ends
//! the cued deck starts and becomes the main deck at once: the song change
//! happens there, with every stamp of a gapless change (generation, queue
//! position, the new main deck's playlist entry id). The old main deck plays
//! out as the tail, fading, and its events only end the blend. The two
//! fades are equal power: amplitudes `sin` and `cos` of the progress, which
//! is read from the incoming song's own position, so pausing or buffering
//! holds the blend. When the blend is done the tail is stopped and kept as
//! the spare deck for the next cue.
//!
//! mpv maps its `volume` property cubically to amplitude, so a deck playing
//! at an amplitude share `a` of the user's volume `V` is set to `V·∛a`.
//! Per-song loudness gains stay per-file `volume-gain` options on each deck.

use std::collections::HashSet;
use std::f64::consts::FRAC_PI_2;

use super::*;
use crate::model::Mixes;

/// The amplitude the current song keeps under an audition (−14 dB).
pub(super) const DUCK: f64 = 0.2;
/// How long ducking and an audition's fades take.
pub(super) const FADE: Duration = Duration::from_millis(250);
/// The volume clock's period while anything fades.
const TICK: Duration = Duration::from_millis(20);

pub(super) enum Message {
    /// The volume clock, for the clock `stamp`.
    Tick { stamp: u64 },
    /// An audition's stream, for the request `stamp`.
    AuditionReady {
        stamp: u64,
        stream: anyhow::Result<Stream>,
    },
    /// No audition since request `stamp` for a while: stop its deck.
    AuditionIdle { stamp: u64 },
    /// Read every deck back for the E2E run.
    #[cfg(feature = "e2e")]
    Sample { stamp: u64 },
}

/// An amplitude share moving from one value to another.
#[derive(Clone, Copy)]
pub(super) struct Ramp {
    from: f64,
    to: f64,
    start: Instant,
    length: Duration,
}

impl Ramp {
    pub fn steady(value: f64) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            length: Duration::ZERO,
        }
    }

    /// From wherever it is now to `to`, over `length`.
    pub fn toward(self, to: f64, length: Duration) -> Self {
        Self {
            from: self.now(),
            to,
            start: Instant::now(),
            length,
        }
    }

    pub fn now(&self) -> f64 {
        if self.length.is_zero() {
            return self.to;
        }
        let k = (self.start.elapsed().as_secs_f64() / self.length.as_secs_f64()).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * k * k * (3.0 - 2.0 * k)
    }

    pub fn target(&self) -> f64 {
        self.to
    }

    pub fn moving(&self) -> bool {
        self.start.elapsed() < self.length && self.from != self.to
    }
}

/// The next song, cued paused and silent on a second deck.
pub(super) struct Cued {
    pub mpv: Arc<Mpv>,
    pub next: Appended,
}

/// The song before, playing out under the new one.
pub(super) struct Tail {
    mpv: Arc<Mpv>,
    /// Seconds the blend lasts, on the incoming song's clock.
    length: f64,
}

pub(super) struct Decks {
    pub mixes: Mixes,
    /// The queue is a radio or a mix (an `RD…` playlist, Start radio).
    pub radio: bool,
    /// Queue entries autoplay added: their changes blend too.
    pub autoplay: HashSet<u64>,
    pub cued: Option<Cued>,
    tail: Option<Tail>,
    /// A stopped deck kept for the next cue.
    spare: Option<Arc<Mpv>>,
    /// The main and tail decks' share under an audition.
    pub duck: Ramp,
    pub audition: super::audition::Audition,
    /// When the main deck last reported its position.
    position_at: Instant,
    /// The volume clock: running, and the stamp that stops a stale one.
    ticking: bool,
    clock: Arc<AtomicU64>,
    #[cfg(feature = "e2e")]
    sampling: Arc<AtomicU64>,
}

impl Decks {
    pub fn new(mixes: Mixes) -> Self {
        Self {
            mixes,
            radio: false,
            autoplay: HashSet::new(),
            cued: None,
            tail: None,
            spare: None,
            duck: Ramp::steady(1.0),
            audition: Default::default(),
            position_at: Instant::now(),
            ticking: false,
            clock: Arc::default(),
            #[cfg(feature = "e2e")]
            sampling: Arc::default(),
        }
    }

    pub fn blending(&self) -> bool {
        self.tail.is_some()
    }
}

/// Whether a play request starts a radio or a mix.
pub(super) fn is_radio(target: &Target) -> bool {
    matches!(target, Target::Watch { playlist_id: Some(id), .. } if id.starts_with("RD"))
}

impl super::Worker {
    // ---- routing ----

    /// An event from one of the decks, by its serial.
    pub(super) async fn deck_event(&mut self, serial: u64, event: MpvEvent) {
        let is = |mpv: Option<&Arc<Mpv>>| mpv.is_some_and(|m| m.serial() == serial);
        if is(self.mpv.as_ref()) {
            self.mpv_event(event).await;
        } else if is(self.decks.tail.as_ref().map(|t| &t.mpv)) {
            // The old song played out (or its deck went): the blend is over.
            match event {
                MpvEvent::Died => {
                    self.decks.tail = None;
                    self.blend_over().await;
                }
                MpvEvent::EndFile { .. } => self.finish_blend().await,
                _ => {}
            }
        } else if is(self.decks.cued.as_ref().map(|c| &c.mpv)) {
            let failed = match &event {
                MpvEvent::Died => true,
                MpvEvent::EndFile { reason, .. } => reason == "error",
                _ => false,
            };
            if failed {
                log::warn!("the cued song failed; it will start the usual way");
                if let Some(cued) = self.decks.cued.take()
                    && !matches!(event, MpvEvent::Died)
                {
                    self.decks.spare = Some(cued.mpv);
                }
                self.emit(true);
            }
        } else if is(self.decks.audition.deck()) {
            self.audition_event(event).await;
        } else if is(self.decks.spare.as_ref()) && matches!(event, MpvEvent::Died) {
            self.decks.spare = None;
        }
    }

    pub(super) async fn deck_message(&mut self, message: Message) {
        match message {
            Message::Tick { stamp } => self.tick(stamp).await,
            Message::AuditionReady { stamp, stream } => self.audition_ready(stamp, stream).await,
            Message::AuditionIdle { stamp } => self.audition_idle(stamp),
            #[cfg(feature = "e2e")]
            Message::Sample { stamp } => self.sample(stamp),
        }
    }

    /// Every running mpv: the equalizer goes to all of them.
    pub(super) fn decks(&self) -> Vec<Arc<Mpv>> {
        self.mpv
            .iter()
            .chain(self.decks.tail.as_ref().map(|t| &t.mpv))
            .chain(self.decks.cued.as_ref().map(|c| &c.mpv))
            .chain(self.decks.spare.as_ref())
            .chain(self.decks.audition.deck())
            .cloned()
            .collect()
    }

    // ---- volumes ----

    /// The blend's progress, 0 to 1, while one runs.
    fn blend_progress(&self) -> Option<f64> {
        let tail = self.decks.tail.as_ref()?;
        let mut position = self.state.position;
        if self.state.playing && !self.state.loading {
            position += self.decks.position_at.elapsed().as_secs_f64().min(0.5);
        }
        Some((position / tail.length).clamp(0.0, 1.0))
    }

    /// The volume the main deck plays at.
    pub(super) fn main_volume(&self) -> f64 {
        let blend = self.blend_progress().map_or(1.0, |t| (t * FRAC_PI_2).sin());
        self.state.volume * self.fade * (self.decks.duck.now() * blend).cbrt()
    }

    fn tail_volume(&self) -> f64 {
        let blend = self.blend_progress().map_or(0.0, |t| (t * FRAC_PI_2).cos());
        self.state.volume * self.fade * (self.decks.duck.now() * blend).cbrt()
    }

    /// Sets every playing deck's volume from the user's volume, the sleep
    /// fade, the duck, the blend and the audition's fades.
    pub(super) async fn apply_volumes(&mut self) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.set("volume", json!(self.main_volume())).await;
        }
        if let Some(tail) = &self.decks.tail {
            let _ = tail.mpv.set("volume", json!(self.tail_volume())).await;
        }
        if let Some(deck) = self.decks.audition.deck() {
            let volume = self.audition_volume();
            let _ = deck.set("volume", json!(volume)).await;
        }
    }

    fn fading(&self) -> bool {
        self.decks.tail.is_some() || self.decks.duck.moving() || self.decks.audition.fading()
    }

    /// Runs the volume clock while anything fades.
    pub(super) fn keep_time(&mut self) {
        if self.decks.ticking {
            return;
        }
        self.decks.ticking = true;
        let stamp = self.decks.clock.fetch_add(1, Ordering::SeqCst) + 1;
        let clock = self.decks.clock.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TICK).await;
                if clock.load(Ordering::SeqCst) != stamp
                    || tx.send(Internal::Deck(Message::Tick { stamp })).is_err()
                {
                    return;
                }
            }
        });
    }

    async fn tick(&mut self, stamp: u64) {
        if stamp != self.decks.clock.load(Ordering::SeqCst) {
            return;
        }
        if self.blend_progress().is_some_and(|t| t >= 1.0) {
            self.finish_blend().await;
        }
        self.apply_volumes().await;
        self.audition_faded().await;
        if !self.fading() {
            self.decks.ticking = false;
            self.decks.clock.fetch_add(1, Ordering::SeqCst);
        }
    }

    // ---- smooth mixes ----

    pub(super) async fn set_mixes(&mut self, mixes: Mixes) {
        let mixes = Mixes {
            on: mixes.on,
            seconds: mixes.seconds.clamp(Mixes::SHORTEST, Mixes::LONGEST),
        };
        self.state.mixes = mixes;
        self.decks.mixes = mixes;
        self.update_settings(|s| s.mixes = mixes);
        self.emit(true);
        self.requeue_next().await;
        if !mixes.on {
            self.decks.spare = None;
        }
    }

    /// After a change to what blends (Smooth mixes, repeat): the song after
    /// the current one is queued again if it now changes the other way.
    pub(super) async fn requeue_next(&mut self) {
        let Some(next) = self.pos.and_then(|p| self.queue.id(p + 1)) else {
            return;
        };
        let queued = self.appended.is_some() || self.decks.cued.is_some();
        if queued && self.blends_into(next) != self.decks.cued.is_some() {
            self.drop_appended().await;
            if self.current_entry.is_some() {
                self.prefetch();
            }
        }
    }

    /// Whether the change to queue entry `id` (the song after the current
    /// one) blends rather than being gapless.
    pub(super) fn blends_into(&self, id: u64) -> bool {
        self.decks.mixes.on
            && self.state.repeat != Repeat::One
            && !self.sleeping_at_song_end()
            && (self.decks.radio || self.decks.autoplay.contains(&id))
    }

    /// Cues the next song paused and silent on a second deck.
    pub(super) async fn cue(&mut self, id: u64, video_id: &str, stream: &Stream) {
        let deck = match self.decks.spare.take() {
            Some(deck) => deck,
            None => match Mpv::spawn(
                &self.paths.runtime.join("mpv-cue.sock"),
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
                    deck
                }
                Err(error) => {
                    log::warn!("couldn't start a second audio player: {error:#}");
                    return;
                }
            },
        };
        let _ = deck.set("pause", json!(true)).await;
        let _ = deck.set("volume", json!(0.0)).await;
        let _ = deck.set("loop-file", json!("no")).await;
        let (options, gain) = self.file_options(video_id, stream, None);
        match deck.load(&stream.url, "replace", &options).await {
            Ok(entry) => {
                self.decks.cued = Some(Cued {
                    mpv: deck,
                    next: Appended {
                        id,
                        itag: stream.itag,
                        entry,
                        gain,
                    },
                });
                self.emit(true);
            }
            Err(error) => {
                log::warn!("couldn't cue the next song: {error:#}");
                self.decks.spare = Some(deck);
            }
        }
    }

    /// Stops the cued song; its deck is kept as the spare.
    pub(super) async fn drop_cued(&mut self) {
        if let Some(cued) = self.decks.cued.take() {
            let _ = cued.mpv.command(json!(["stop"])).await;
            self.decks.spare = Some(cued.mpv);
        }
    }

    /// The main deck reported its position: start the blend when the
    /// current song is close enough to its end. True when it started.
    pub(super) async fn deck_position(&mut self) -> bool {
        self.decks.position_at = Instant::now();
        let Some(id) = self.decks.cued.as_ref().map(|c| c.next.id) else {
            return false;
        };
        if self.decks.tail.is_some()
            || self.paused
            || self.state.duration <= 0.0
            || !self.blends_into(id)
        {
            return false;
        }
        let length = f64::from(self.decks.mixes.seconds).min(self.state.duration / 3.0);
        let left = self.state.duration - self.state.position;
        // Closer to the end than that, the end of the file changes songs at once.
        if left > length || left <= 0.25 {
            return false;
        }
        self.swap(left).await;
        true
    }

    /// The cued song becomes the current one on its own deck, now. Over
    /// `length` seconds the old one fades out under it; 0 cuts it.
    pub(super) async fn swap(&mut self, length: f64) {
        self.finish_blend().await;
        let Some(cued) = self.decks.cued.take() else {
            return;
        };
        let outgoing = self.mpv.replace(cued.mpv.clone());
        self.current_entry = Some(cued.next.entry);
        self.idle = false;
        self.paused = false;
        self.state.position = 0.0;
        self.decks.position_at = Instant::now();
        if let Some(old) = outgoing {
            if length > 0.0 {
                log::info!("blending into the next song over {length:.1}s");
                self.decks.tail = Some(Tail { mpv: old, length });
                self.keep_time();
            } else {
                let _ = old.command(json!(["stop"])).await;
                if self.decks.mixes.on {
                    self.decks.spare = Some(old);
                }
            }
        }
        self.apply_loop().await;
        self.apply_volumes().await;
        let _ = cued.mpv.set("pause", json!(false)).await;
        let entry = cued.next.entry;
        self.advanced(cued.next).await;
        // The cued file's length arrived while it waited; ask for it again.
        if self.current_entry == Some(entry)
            && let Ok(duration) = cued.mpv.get("duration").await
            && let Some(duration) = duration.as_f64()
        {
            self.state.duration = duration;
            self.emit(true);
        }
    }

    /// Ends a blend at once: the old song stops, the new one plays at full
    /// volume, and the song after it is queued.
    pub(super) async fn finish_blend(&mut self) {
        if self.stop_tail().await {
            self.blend_over().await;
        }
    }

    /// Ends a blend because the current song is changing or failing: the
    /// old song stops and the current deck is at full volume at once, so the
    /// blend's progress (the current song's position) can't bring the old
    /// one back. Whatever changes the song queues the one after it.
    pub(super) async fn stop_tail(&mut self) -> bool {
        let Some(tail) = self.decks.tail.take() else {
            return false;
        };
        let _ = tail.mpv.command(json!(["stop"])).await;
        if self.decks.mixes.on {
            self.decks.spare = Some(tail.mpv);
        }
        self.apply_volumes().await;
        true
    }

    /// After a blend: the new song at full volume, and the song after it
    /// queued (its resolve was set aside while both decks were busy).
    async fn blend_over(&mut self) {
        self.apply_volumes().await;
        if self.current_entry.is_some() {
            self.prefetch();
        }
    }

    // ---- E2E: what every deck has ----

    /// Reads every deck back five times a second while on.
    #[cfg(feature = "e2e")]
    pub(super) fn sample_decks(&mut self, on: bool) {
        let stamp = self.decks.sampling.fetch_add(1, Ordering::SeqCst) + 1;
        if !on {
            return;
        }
        let current = self.decks.sampling.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            while current.load(Ordering::SeqCst) == stamp
                && tx.send(Internal::Deck(Message::Sample { stamp })).is_ok()
            {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    #[cfg(feature = "e2e")]
    fn sample(&mut self, stamp: u64) {
        if stamp != self.decks.sampling.load(Ordering::SeqCst) {
            return;
        }
        let roles: Vec<(&'static str, Arc<Mpv>)> = [
            ("main", self.mpv.clone()),
            ("tail", self.decks.tail.as_ref().map(|t| t.mpv.clone())),
            ("cued", self.decks.cued.as_ref().map(|c| c.mpv.clone())),
            ("audition", self.decks.audition.deck().cloned()),
        ]
        .into_iter()
        .filter_map(|(role, mpv)| mpv.map(|m| (role, m)))
        .collect();
        let state = json!({
            "ms": crate::e2e::clock_ms(),
            "volume": self.state.volume,
            "index": self.state.index,
            "current": self.current().map(|t| t.video_id.clone()),
            "position": self.state.position,
            "playing": self.state.playing,
            "duck": self.decks.duck.now(),
            "duration": self.state.duration,
            "blend": self.blend_progress(),
            "next_ready": self.appended.is_some() || self.decks.cued.is_some(),
            "held": self.state.audition.as_ref().map(|a| json!({"id": a.video_id, "playing": a.playing})),
        });
        tokio::spawn(async move {
            let mut sample = state;
            for (role, mpv) in roles {
                let number = |v: Option<serde_json::Value>| v.and_then(|v| v.as_f64());
                let volume = number(mpv.get("volume").await.ok());
                let position = number(mpv.get("time-pos").await.ok());
                let pause = mpv.get("pause").await.ok();
                sample[role] = json!({
                    "serial": mpv.serial(),
                    "volume": volume,
                    "time_pos": position,
                    "pause": pause,
                });
            }
            crate::e2e::probe_push("decks", sample);
        });
    }
}
